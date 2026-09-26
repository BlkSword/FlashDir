// 全局文件搜索索引管理器
//
// 复用 fs::try_mft_scan 扫描所有 NTFS 卷，构建常驻内存索引，
// 支持按文件名毫秒级跨盘搜索（Everything 式）。索引构建一次后常驻，
// 后续搜索仅为内存过滤；刷新通过 global_search_ensure_index / refresh 全量重建。

use std::collections::{BinaryHeap, HashMap};
use std::sync::mpsc;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use parking_lot::RwLock;
use rayon::prelude::*;
use serde::Serialize;

/// 索引中的一项（绝对路径）。
///
/// 存储态只保留 path + name_lower（匹配必需）：
/// - `name` 由 path 派生（序列化给前端时现算），省掉 114 万次 String 分配；
/// - `ext` 在 `ext:` 过滤时从 name_lower 现算。
#[derive(Debug, Clone)]
pub struct IndexEntry {
    pub path: String,
    /// 小写文件名（搜索用，避免每次搜索对全量 name 做 to_lowercase）
    pub name_lower: String,
    pub size: i64,
    pub is_dir: bool,
    /// 文件修改时间（Windows FILETIME 转换而来的 Unix 时间戳，目录为 0）
    pub mtime: i64,
}

impl Serialize for IndexEntry {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeStruct;
        // 与前端约定一致的字段：path / name / size / isDir / mtime
        let mut state = serializer.serialize_struct("IndexEntry", 5)?;
        state.serialize_field("path", &self.path)?;
        state.serialize_field("name", &name_from_path(&self.path))?;
        state.serialize_field("size", &self.size)?;
        state.serialize_field("isDir", &self.is_dir)?;
        state.serialize_field("mtime", &self.mtime)?;
        state.end()
    }
}

/// 索引就绪时的元数据（独立 struct：enum 级 rename_all 在 serde 里只作用于 variant 名，
/// 不保证 struct variant 字段被重命名，故抽出来确保字段序列化为 camelCase）
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadyData {
    pub file_count: usize,
    pub dir_count: usize,
    pub drive_count: usize,
    /// MFT 扫描失败的盘（需管理员或非 NTFS），以及枚举到的全部 NTFS 盘符（诊断用）
    pub failed_drives: Vec<String>,
    pub all_drives: Vec<String>,
    /// true = 仅包含"主界面扫描过的目录"，并非全盘索引。
    /// 前端据此提示"部分目录"，避免用户误以为跨盘搜索已完整。
    pub partial: bool,
    /// 最近一次增量同步时间（Unix 秒，0 = 尚未同步）
    pub usn_last_sync: i64,
    /// 增量窗口失效、建议重建索引的盘
    pub usn_stale_drives: Vec<String>,
}

/// 索引状态（前端据 kind 判断）
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase", tag = "kind", content = "data")]
pub enum IndexState {
    NotLoaded,
    Loading { drive: String, scanned: usize },
    Ready(ReadyData),
    Failed { reason: String },
}

#[derive(Debug, Clone, Default, Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct IndexMeta {
    drive_count: usize,
    failed_drives: Vec<String>,
    all_drives: Vec<String>,
    /// 是否由"全盘索引构建"产生（false = 只散落了主界面扫描过的目录）
    full_build: bool,
    /// 是否已完成历史畸形路径修复（旧版本把反斜杠形式的扫描根与已经是绝对路径的
    /// 条目直接拼接，产生 "C:\root/C:/root/xxx" 这类损坏路径，曾占索引三成）。
    /// 修复成功执行一次即可，之后不再重复扫描。
    path_repair_done: bool,
    /// USN 增量同步状态（检查点 / 需要重建的盘 / 最近同步时间）
    usn: UsnSyncState,
}

pub struct GlobalIndex {
    /// 条目 arena：Vec 连续存储，搜索顺序/并行扫描时缓存局部性好
    entries: RwLock<Vec<IndexEntry>>,
    /// 路径 128 位哈希 → arena 下标（去重 + 按路径增删）。
    /// 用哈希代替路径字符串作 key：省掉 114 万份路径副本（约 130MB）；
    /// 命中后再用 arena 里的 path 校验，128 位下冲突概率可忽略。
    by_path: RwLock<HashMap<u128, u32>>,
    /// 文件名首字符分桶：char → arena 下标列表。
    /// 早期是 HashSet<String>，相当于把每个路径再存一份（百万级额外分配）；
    /// 改存 u32 下标后不再复制路径，分桶查询也省掉了按路径哈希查找。
    name_index: RwLock<HashMap<char, Vec<u32>>>,
    state: RwLock<IndexState>,
    meta: RwLock<IndexMeta>,
    /// 增量维护的文件/目录计数，避免每次状态刷新都 O(n) 全量重数
    file_count: AtomicUsize,
    dir_count: AtomicUsize,
    /// 索引构建中标志（进程内互斥；跨进程还有锁文件）
    building: AtomicBool,
}

/// 路径 → 128 位哈希（双 64 位 FNV-1a 拼接，冲突概率可忽略）
fn path_hash(path: &str) -> u128 {
    let mut a: u64 = 0xcbf2_9ce4_8422_2325;
    let mut b: u64 = 0x8422_2325_cbf2_9ce4;
    for byte in path.as_bytes() {
        a ^= *byte as u64;
        a = a.wrapping_mul(0x0000_0100_0000_01b3);
        b ^= *byte as u64;
        b = b.wrapping_mul(0x0000_0100_0000_01b3).rotate_left(7);
    }
    ((a as u128) << 64) | b as u128
}

/// 全局搜索命令的返回结构。
///
/// 放在 lib 内（而不是 bin 的 commands.rs）有两个原因：
/// 1. 契约可被 `cargo test --lib` 覆盖（bin crate 的测试二进制在本环境无法加载）；
/// 2. 前端依赖 `results` 是数组、字段为 camelCase —— 曾经前端把整个响应当数组用，
///    导致 `rows.map is not a function` 让命令面板渲染失败。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GlobalSearchResponse {
    pub ready: bool,
    pub state: IndexState,
    pub results: Vec<IndexEntry>,
    /// 命中总数（不受 limit/offset 影响）：前端用于"共 N 项命中，已显示前 M 项"
    pub total: usize,
    /// 是否还有更多结果（total > offset + results.len()）
    pub truncated: bool,
    /// 诊断：搜索无结果时返回索引实际条目数
    #[serde(skip_serializing_if = "Option::is_none")]
    pub index_size: Option<usize>,
    /// 诊断：搜索无结果时返回前几个索引条目名称（确认 name 字段是否正常）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sample_names: Option<Vec<String>>,
}

/// 从绝对路径取文件名（存储态不再保存 name，结果/诊断时才派生）
fn name_from_path(path: &str) -> String {
    path.rsplit_once('/')
        .map(|(_, name)| name.to_string())
        .unwrap_or_else(|| path.to_string())
}

/// 取小写文件名的首字符（空名归入 char::MIN 桶）
fn bucket_char(name_lower: &str) -> char {
    name_lower.chars().next().unwrap_or(char::MIN)
}

impl GlobalIndex {
    fn new() -> Self {
        GlobalIndex {
            entries: RwLock::new(Vec::new()),
            by_path: RwLock::new(HashMap::new()),
            name_index: RwLock::new(HashMap::new()),
            state: RwLock::new(IndexState::NotLoaded),
            meta: RwLock::new(IndexMeta::default()),
            file_count: AtomicUsize::new(0),
            dir_count: AtomicUsize::new(0),
            building: AtomicBool::new(false),
        }
    }


    /// 从 SQLite 磁盘缓存异步恢复持久化索引。
    /// 应在后台任务中调用，避免阻塞启动路径。
    pub fn load_persisted(&self) {
        if !matches!(self.state(), IndexState::NotLoaded) {
            return;
        }
        self.set_loading();
        // 低内存机器上直接跳过载入：宁可索引显示"未就绪"，也不要启动即闪退
        if crate::diag::available_physical_mb().is_some_and(|mb| mb < 300) {
            *self.state.write() = IndexState::NotLoaded;
            crate::diag::log_line("[GlobalIndex] 可用内存低于 300MB，跳过载入持久化索引");
            return;
        }
        match crate::disk_cache::DiskCache::instance().load_global_index() {
            Ok(mut entries) if !entries.is_empty() => {
                eprintln!("[GlobalIndex] 从磁盘恢复 {} 条索引", entries.len());
                crate::diag::breadcrumb(&format!(
                    "索引载入完成：{} 条，可用内存 {}",
                    entries.len(),
                    crate::diag::memory_text()
                ));
                // 恢复元数据（是否全盘构建 / 盘符列表），否则会把部分索引误判为全盘
                let mut meta = match crate::disk_cache::DiskCache::instance().load_index_meta() {
                    Some(json) => serde_json::from_str::<IndexMeta>(&json).unwrap_or_default(),
                    None => {
                        // 兼容旧库：升级前只有"全盘构建"才会持久化索引，且没有 full_build 标记，
                        // 按全盘处理，避免老用户升级后看到"部分目录"
                        IndexMeta {
                            full_build: true,
                            ..Default::default()
                        }
                    }
                };
                if meta.all_drives.is_empty() {
                    // 盘符列表缺失时用当前枚举结果补齐（仅诊断展示用）
                    let drives = list_ntfs_drives();
                    meta.drive_count = meta.drive_count.max(drives.len());
                    meta.all_drives = drives.iter().map(|c| c.to_string()).collect();
                }
                // 历史版本拼坏的索引路径（"C:\root/C:/root/xxx"）在这里一次性修掉：
                // 必须在载入内存索引之前执行，否则脏路径会继续污染搜索结果。
                if !meta.path_repair_done {
                    match crate::disk_cache::DiskCache::instance().repair_corrupt_paths() {
                        Ok((merged, written)) => {
                            if merged + written > 0 {
                                eprintln!(
                                    "[GlobalIndex] 已修复畸形索引路径 {} 条（合并 {} / 补写 {}）",
                                    merged + written,
                                    merged,
                                    written
                                );
                            }
                            meta.path_repair_done = true;
                            if let Ok(json) = serde_json::to_string(&meta) {
                                let _ = crate::disk_cache::DiskCache::instance().save_index_meta(&json);
                            }
                            // 修复后重新读取，保证进内存的是干净数据
                            if let Ok(fresh) = crate::disk_cache::DiskCache::instance().load_global_index() {
                                if !fresh.is_empty() {
                                    entries = fresh;
                                }
                            }
                        }
                        Err(e) => eprintln!("[GlobalIndex] 畸形路径修复失败: {e}"),
                    }
                }
                *self.meta.write() = meta;
                // 一次性批量写入，避免每条一次写锁 + clone
                self.upsert_batch_internal(entries);
                self.update_ready_state();
            }
            _ => {
                *self.state.write() = IndexState::NotLoaded;
            }
        }
    }

    pub fn state(&self) -> IndexState {
        self.state.read().clone()
    }

    fn update_ready_state(&self) {
        let fc = self.file_count.load(Ordering::Relaxed);
        let dc = self.dir_count.load(Ordering::Relaxed);

        let meta = self.meta.read();
        *self.state.write() = IndexState::Ready(ReadyData {
            file_count: fc,
            dir_count: dc,
            drive_count: meta.drive_count,
            failed_drives: meta.failed_drives.clone(),
            all_drives: meta.all_drives.clone(),
            partial: !meta.full_build,
            usn_last_sync: meta.usn.last_sync_at,
            usn_stale_drives: meta.usn.stale_drives.clone(),
        });
    }

    /// 持久化索引元数据（重启后可恢复"是否全盘构建"的语义）
    fn persist_meta(&self) {
        let meta = self.meta.read().clone();
        if let Ok(json) = serde_json::to_string(&meta) {
            let _ = crate::disk_cache::DiskCache::instance().save_index_meta(&json);
        }
    }

    /// 添加或替换一条索引。entries / by_path / name_index 在同一锁临界区内更新。
    /// 锁顺序：永远先 entries 再 by_path 再 name_index，避免死锁。
    fn upsert_internal(&self, entry: IndexEntry) {
        let entry = repair_entry_path(entry);
        let first_char = bucket_char(&entry.name_lower);
        let hash = path_hash(&entry.path);
        let is_dir = entry.is_dir;

        let mut entries = self.entries.write();
        let mut by_path = self.by_path.write();
        let mut name_index = self.name_index.write();

        // 命中校验：哈希命中且路径一致才算更新（128 位下冲突可忽略）
        let existing = by_path.get(&hash).copied().filter(|&idx| {
            entries
                .get(idx as usize)
                .map(|e| e.path == entry.path)
                .unwrap_or(false)
        });

        match existing {
            Some(idx) => {
                let old = &entries[idx as usize];
                if old.is_dir != is_dir {
                    self.bump_count(old.is_dir, -1);
                    self.bump_count(is_dir, 1);
                }
                let old_char = bucket_char(&old.name_lower);
                entries[idx as usize] = entry;
                if old_char != first_char {
                    // 换桶：从旧桶摘掉，加入新桶（不做 contains 线性扫描）
                    if let Some(bucket) = name_index.get_mut(&old_char) {
                        bucket.retain(|&i| i != idx);
                    }
                    name_index.entry(first_char).or_default().push(idx);
                }
                // 同桶更新：桶内已有该下标，无需处理
            }
            None => {
                let idx = entries.len() as u32;
                entries.push(entry);
                by_path.insert(hash, idx);
                name_index.entry(first_char).or_default().push(idx);
                self.bump_count(is_dir, 1);
            }
        }
    }

    fn bump_count(&self, is_dir: bool, delta: isize) {
        let counter = if is_dir { &self.dir_count } else { &self.file_count };
        if delta >= 0 {
            counter.fetch_add(delta as usize, Ordering::Relaxed);
        } else {
            counter.fetch_sub((-delta) as usize, Ordering::Relaxed);
        }
    }

    /// 批量 upsert：整批在同一个锁临界区内完成（按值消费，避免逐条 clone）
    fn upsert_batch_internal(&self, batch: Vec<IndexEntry>) {
        let mut entries = self.entries.write();
        let mut by_path = self.by_path.write();
        let mut name_index = self.name_index.write();

        for entry in batch {
            let entry = repair_entry_path(entry);
            let first_char = bucket_char(&entry.name_lower);
            let hash = path_hash(&entry.path);
            let is_dir = entry.is_dir;

            let existing = by_path.get(&hash).copied().filter(|&idx| {
                entries
                    .get(idx as usize)
                    .map(|e| e.path == entry.path)
                    .unwrap_or(false)
            });

            match existing {
                Some(idx) => {
                    let old = &entries[idx as usize];
                    if old.is_dir != is_dir {
                        self.bump_count(old.is_dir, -1);
                        self.bump_count(is_dir, 1);
                    }
                    let old_char = bucket_char(&old.name_lower);
                    entries[idx as usize] = entry;
                    if old_char != first_char {
                        if let Some(bucket) = name_index.get_mut(&old_char) {
                            bucket.retain(|&i| i != idx);
                        }
                        name_index.entry(first_char).or_default().push(idx);
                    }
                }
                None => {
                    let idx = entries.len() as u32;
                    entries.push(entry);
                    by_path.insert(hash, idx);
                    name_index.entry(first_char).or_default().push(idx);
                    self.bump_count(is_dir, 1);
                }
            }
        }
    }

    /// 移除指定路径的索引（swap_remove 保持 arena 紧凑）
    fn remove_path_internal(&self, path: &str) {
        let mut entries = self.entries.write();
        let mut by_path = self.by_path.write();
        let mut name_index = self.name_index.write();

        let hash = path_hash(path);
        // 正常路径：哈希命中且路径一致；哈希冲突时线性兜底（实际不会发生）
        let idx = match by_path.get(&hash).copied() {
            Some(idx)
                if entries
                    .get(idx as usize)
                    .map(|e| e.path == path)
                    .unwrap_or(false) =>
            {
                idx
            }
            _ => match entries.iter().position(|e| e.path == path) {
                Some(i) => i as u32,
                None => return,
            },
        };

        let removed_is_dir = entries[idx as usize].is_dir;
        let removed_char = bucket_char(&entries[idx as usize].name_lower);
        by_path.remove(&hash);

        let last = entries.len() - 1;
        if idx as usize != last {
            entries.swap_remove(idx as usize);
            // 被换过来的条目：同步 by_path 与 name_index
            let moved_hash = path_hash(&entries[idx as usize].path);
            let moved_char = bucket_char(&entries[idx as usize].name_lower);
            by_path.insert(moved_hash, idx);
            if let Some(bucket) = name_index.get_mut(&moved_char) {
                for slot in bucket.iter_mut() {
                    if *slot == last as u32 {
                        *slot = idx;
                        break;
                    }
                }
            }
        } else {
            entries.pop();
        }

        if let Some(bucket) = name_index.get_mut(&removed_char) {
            bucket.retain(|&i| i != idx);
        }
        self.bump_count(removed_is_dir, -1);
    }

    /// 按前缀移除索引（仅该路径本身及其子路径）
    fn remove_prefix_internal(&self, prefix: &str) {
        let paths: Vec<String> = {
            let entries = self.entries.read();
            entries
                .iter()
                .filter(|e| is_same_or_child(prefix, &e.path))
                .map(|e| e.path.clone())
                .collect()
        };
        for path in paths {
            self.remove_path_internal(&path);
        }
    }


    /// 清空所有索引数据。
    fn clear_internal(&self) {
        self.entries.write().clear();
        self.by_path.write().clear();
        self.name_index.write().clear();
        self.file_count.store(0, Ordering::Relaxed);
        self.dir_count.store(0, Ordering::Relaxed);
    }


    /// 准备开始构建索引（设置 Loading 状态）
    pub fn set_loading(&self) {
        *self.state.write() = IndexState::Loading { drive: String::new(), scanned: 0 };
        self.clear_internal();
        *self.meta.write() = IndexMeta::default();
    }

    /// 标记索引构建失败（例如启动时无管理员权限读取 MFT）
    pub fn set_failed(&self, reason: String) {
        *self.state.write() = IndexState::Failed { reason };
    }

    /// 直接追加 MFT 全卷扫描结果，避免先转成 `Item` 再转成 `IndexEntry` 的中间分配。
    pub fn append_mft_files(&self, drive: char, mft_files: &[crate::fs::MftFileInfo]) {
        let batch: Vec<IndexEntry> = mft_files
            .iter()
            .map(|f| {
                // 存储态不保留 name / ext（结果与诊断时由 path 派生），
                // 只保留匹配所需的 name_lower，省掉百万级 String 分配。
                IndexEntry {
                    path: normalize_abs_path(drive, &f.path),
                    name_lower: f.name.to_lowercase(),
                    size: f.size as i64,
                    is_dir: f.is_dir,
                    mtime: f.mtime,
                }
            })
            .collect();
        self.upsert_batch_internal(batch);

        let total = self.entries.read().len();
        *self.state.write() = IndexState::Loading {
            drive: drive.to_string(),
            scanned: total,
        };
    }

    /// 逐盘追加 scan_directory 结果（回退路径，已含完整字段）
    pub fn append_scan(&self, drive: char, items: &[crate::scan::Item]) {
        let batch: Vec<IndexEntry> = items
            .iter()
            .map(|item| IndexEntry {
                path: normalize_abs_path(drive, item.path.as_str()),
                name_lower: item.name.to_lowercase(),
                size: item.size,
                is_dir: item.is_dir,
                mtime: item.mtime,
            })
            .collect();
        self.upsert_batch_internal(batch);

        let total = self.entries.read().len();
        *self.state.write() = IndexState::Loading {
            drive: drive.to_string(),
            scanned: total,
        };
    }

    /// 所有盘扫描完毕后标记为就绪，并后台持久化到 SQLite。
    /// `ok_drives` 为成功建索引的盘，`failed_drives` 为失败/跳过的 NTFS 盘。
    pub fn finish_building(&self, ok_drives: &[char], failed_drives: &[char]) {
        {
            let mut meta = self.meta.write();
            meta.drive_count = ok_drives.len();
            meta.all_drives = ok_drives.iter().map(|c| c.to_string()).collect();
            meta.failed_drives = failed_drives.iter().map(|c| c.to_string()).collect();
            meta.full_build = true;
        }
        self.persist_meta();
        self.update_ready_state();

        crate::diag::breadcrumb(&format!(
            "索引持久化：开始写出 {} 条（可用内存 {}）",
            self.entries_len(),
            crate::diag::memory_text()
        ));
        // 流式持久化：后台 SQLite 写入，前台分批发送，避免整表 clone
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            match crate::disk_cache::DiskCache::instance().save_global_index_stream(rx) {
                Ok(_) => eprintln!("[GlobalIndex] 已持久化全局索引"),
                Err(e) => eprintln!("[GlobalIndex] 持久化失败: {}", e),
            }
        });

        const CHUNK_SIZE: usize = 5000;
        {
            let entries = self.entries.read();
            let mut chunk = Vec::with_capacity(CHUNK_SIZE);
            for entry in entries.iter() {
                chunk.push(entry.clone());
                if chunk.len() >= CHUNK_SIZE {
                    let _ = tx.send(std::mem::take(&mut chunk));
                }
            }
            if !chunk.is_empty() {
                let _ = tx.send(chunk);
            }
        }
        drop(tx);
    }

    /// 将主界面某次扫描的结果追加到全局索引（复用已验证可用的 scan_dir 结果）。
    pub fn add_items(&self, scan_path: &str, items: &[crate::scan::Item]) {
        // 扫描根统一成规范形式（正斜杠 / 盘符大写 / 无末尾分隔符）：
        // 早期版本把 "C:\Users\me" 这类根直接与已经是绝对路径的条目拼接，
        // 生成 "C:\Users\me/C:/Users/me/xxx" 脏路径（历史上一度占索引三成）。
        let path_base =
            normalize_index_path(scan_path.trim_end_matches(|c| c == '/' || c == '\\')).into_owned();

        // 先移除该路径下已有的条目，避免重复（内存 + 磁盘同步移除）
        let prefix = format!("{}/", path_base);
        self.remove_prefix_internal(&prefix);
        let _ = crate::disk_cache::DiskCache::instance().remove_global_index_by_prefix(&prefix);

        let batch: Vec<IndexEntry> = items
            .iter()
            .map(|item| IndexEntry {
                path: index_path_for(&path_base, item.path.as_str()),
                name_lower: item.name.to_lowercase(),
                size: item.size,
                is_dir: item.is_dir,
                mtime: item.mtime,
            })
            .collect();

        // 增量持久化到磁盘，避免全量重建
        let _ = crate::disk_cache::DiskCache::instance().upsert_global_index_entries(&batch);

        self.upsert_batch_internal(batch);

        // 状态处理：
        // - 正在建索引：只更新进度，等 finish_building 统一置 Ready；
        // - 其它情况：置 Ready，但保留 full_build 标记，
        //   前端会提示"部分目录"，用户仍可点刷新重建全盘索引。
        let is_loading = matches!(*self.state.read(), IndexState::Loading { .. });
        if is_loading {
            let total = self.entries_len();
            *self.state.write() = IndexState::Loading {
                drive: String::new(),
                scanned: total,
            };
        } else {
            self.update_ready_state();
        }
        self.persist_meta();
    }

    /// 更新或插入单条条目（供 USN 增量同步使用）
    pub fn upsert(&self, entry: IndexEntry) {
        self.upsert_internal(entry);
        // 保持 Ready 状态计数准确
        if matches!(*self.state.read(), IndexState::Ready(..)) {
            self.update_ready_state();
        }
    }

    /// 按绝对路径移除条目（供 USN 增量同步使用）
    pub fn remove_by_path(&self, path: &str) {
        self.remove_path_internal(path);
        if matches!(*self.state.read(), IndexState::Ready(..)) {
            self.update_ready_state();
        }
    }

    /// 批量更新或插入条目（USN 增量同步）：整批一次锁临界区 + 一次状态刷新
    pub fn upsert_batch(&self, entries: Vec<IndexEntry>) {
        if entries.is_empty() {
            return;
        }
        self.upsert_batch_internal(entries);
        if matches!(*self.state.read(), IndexState::Ready(..)) {
            self.update_ready_state();
        }
    }

    /// 批量按绝对路径移除条目（USN 增量同步）：一次锁临界区 + 一次状态刷新
    pub fn remove_paths_batch(&self, paths: &[String]) {
        if paths.is_empty() {
            return;
        }
        for path in paths {
            self.remove_path_internal(path);
        }
        if matches!(*self.state.read(), IndexState::Ready(..)) {
            self.update_ready_state();
        }
    }

    /// 返回索引中的条目总数（诊断用：若 >0 但搜索无结果，说明匹配逻辑有问题）
    pub fn entries_len(&self) -> usize {
        self.entries.read().len()
    }

    /// 返回前 n 个条目名称样本（诊断：搜索无结果时确认 name 字段是否正常）
    pub fn sample_names(&self, n: usize) -> Vec<String> {
        self.entries
            .read()
            .iter()
            .take(n)
            .map(|e| name_from_path(&e.path))
            .collect()
    }

    /// 支持 Everything 式过滤语法与相关性排序的搜索（返回前 limit 条）。
    /// 过滤语法：ext:zip size:>100MB type:file dir:xxx name:xxx mtime:>7d NOT .tmp
    pub fn search_with_filter(&self, query: &str, limit: usize) -> Vec<IndexEntry> {
        self.search_with_filter_paged(query, limit, 0).0
    }

    /// 分页搜索：返回 `(结果, 命中总数)`。
    ///
    /// 实现方式是"取前 limit+offset 条再丢弃前 offset 条"——搜索本身是 top-K 收集，
    /// 没有可随机访问的全序结果；offset 通常很小（前端按页加载），代价可接受。
    pub fn search_with_filter_paged(
        &self,
        query: &str,
        limit: usize,
        offset: usize,
    ) -> (Vec<IndexEntry>, usize) {
        let (mut all, total) = self.search_internal(query, limit.saturating_add(offset));
        let page = if offset >= all.len() {
            Vec::new()
        } else {
            all.split_off(offset)
        };
        (page, total)
    }

    fn search_internal(&self, query: &str, limit: usize) -> (Vec<IndexEntry>, usize) {
        if limit == 0 {
            return (Vec::new(), 0);
        }
        let filters = parse_search_filter(query);
        // 过滤条件为空（例如输入只有 AND/OR 或未识别的空 token）：
        // 直接返回空结果，绝不能退化成"全量按大小返回前 N 条"。
        if filters.is_empty() {
            return (Vec::new(), 0);
        }

        // 文本条件用于相关性排序：取第一个正向文本条件。
        let q_lower = filters
            .iter()
            .find_map(|f| match &f.kind {
                SearchFilterKind::Text(t) if !f.negate => Some(t.to_lowercase()),
                _ => None,
            })
            .unwrap_or_default();

        // 首字符分桶只在前缀语义下成立：`contains` 命中的名字，首字符可以是任意字符。
        // 历史 bug：3 个字符以上的查询被限制在"查询首字符"桶内，于是搜
        // `qm1-methodology` 永远找不到 `EQM1-METHODOLOGY.md`（E != Q）。
        // 现在只有 `前缀*`（Prefix）能提供首字符约束，其余一律全量并行过滤
        // （75 万条目实测 10-30ms，不值得为了这点开销去换漏结果）。
        let bucket = filters.iter().find_map(|f| match &f.kind {
            SearchFilterKind::Prefix(p) if !f.negate && !p.is_empty() => p.chars().next(),
            _ => None,
        });

        let entries = self.entries.read();

        let matches = |e: &IndexEntry| -> bool { apply_filters(e, &filters) };

        // 每线程维护一个大小为 limit 的 top-K 堆，最后归并；
        // 只会 clone 最终 ≤limit 条，而不是克隆全部命中。
        let topk = if let Some(first_char) = bucket {
            // 前缀条件提供首字符约束：候选只可能落在该桶内。
            // 收集的是 &IndexEntry 引用（几百 KB），不 clone 候选路径。
            let name_index = self.name_index.read();
            let bucket_topk = match name_index.get(&first_char) {
                Some(bucket) => bucket
                    .par_iter()
                    .fold(
                        || TopK::new(limit),
                        |mut acc, &idx| {
                            if let Some(e) = entries.get(idx as usize) {
                                if matches(e) {
                                    acc.push(e, relevance_score(e, &q_lower));
                                }
                            }
                            acc
                        },
                    )
                    .reduce(|| TopK::new(limit), TopK::merge),
                None => TopK::new(limit),
            };
            drop(name_index);
            bucket_topk
        } else {
            // 无首字符约束（包含匹配 / 扩展名 / 体积 / 时间等）：全量并行过滤
            let values: Vec<&IndexEntry> = entries.iter().collect();
            values
                .par_iter()
                .fold(
                    || TopK::new(limit),
                    |mut acc, e| {
                        if matches(e) {
                            acc.push(e, relevance_score(e, &q_lower));
                        }
                        acc
                    },
                )
                .reduce(|| TopK::new(limit), TopK::merge)
        };

        let total = topk.total();
        let results = topk.into_entries();

        drop(entries);
        (results, total)
    }
}

/// 搜索过滤条件
#[derive(Debug, Clone)]
pub struct SearchFilter {
    pub kind: SearchFilterKind,
    pub negate: bool,
}

#[derive(Debug, Clone)]
pub enum SearchFilterKind {
    Text(String),
    /// 含路径分隔符的查询：按整条路径做包含匹配（用户常直接粘贴完整路径）
    PathText(String),
    Name(String),
    Ext(String),
    Prefix(String),
    Suffix(String),
    Dir(String),
    Type { is_dir: bool },
    Size { op: FilterOp, bytes: i64 },
    Mtime { op: FilterOp, seconds: i64 },
}

#[derive(Debug, Clone, Copy)]
pub enum FilterOp {
    Gt,
    Gte,
    Lt,
    Lte,
    Eq,
    Ne,
}

fn parse_op(s: &str) -> Option<FilterOp> {
    match s {
        ">" => Some(FilterOp::Gt),
        ">=" => Some(FilterOp::Gte),
        "<" => Some(FilterOp::Lt),
        "<=" => Some(FilterOp::Lte),
        "=" | "==" => Some(FilterOp::Eq),
        "!=" | "<>" => Some(FilterOp::Ne),
        _ => None,
    }
}

fn parse_size(value: &str) -> Option<(FilterOp, i64)> {
    let s = value.trim();
    if s.is_empty() { return None; }
    let (op_str, rest) = if s.starts_with(">=") {
        (">=", &s[2..])
    } else if s.starts_with("<=") {
        ("<=", &s[2..])
    } else if s.starts_with("!=") {
        ("!=", &s[2..])
    } else if s.starts_with('=') {
        ("=", &s[1..])
    } else if s.starts_with('>') {
        (">", &s[1..])
    } else if s.starts_with('<') {
        ("<", &s[1..])
    } else {
        (">=", s)
    };
    let op = parse_op(op_str)?;
    let rest = rest.trim();

    let mut num_end = rest.len();
    let mut unit = "B";
    for (i, c) in rest.char_indices() {
        if !c.is_ascii_digit() && c != '.' {
            num_end = i;
            unit = &rest[i..];
            break;
        }
    }
    if num_end == 0 { return None; }
    let num: f64 = rest[..num_end].trim().parse().ok()?;
    let multiplier = match unit.trim().to_uppercase().as_str() {
        "B" => 1i64,
        "KB" => 1024i64,
        "MB" => 1024i64 * 1024,
        "GB" => 1024i64 * 1024 * 1024,
        "TB" => 1024i64 * 1024 * 1024 * 1024,
        _ => 1i64,
    };
    Some((op, (num * multiplier as f64) as i64))
}

fn parse_mtime(value: &str) -> Option<(FilterOp, i64)> {
    let s = value.trim();
    if s.is_empty() { return None; }
    let (op_str, rest) = if s.starts_with(">=") {
        (">=", &s[2..])
    } else if s.starts_with("<=") {
        ("<=", &s[2..])
    } else if s.starts_with("!=") {
        ("!=", &s[2..])
    } else if s.starts_with('=') {
        ("=", &s[1..])
    } else if s.starts_with('>') {
        (">", &s[1..])
    } else if s.starts_with('<') {
        ("<", &s[1..])
    } else {
        ("<=", s)
    };
    let op = parse_op(op_str)?;
    let rest = rest.trim();

    let mut num_end = rest.len();
    let mut unit = "d";
    for (i, c) in rest.char_indices() {
        if !c.is_ascii_digit() && c != '.' {
            num_end = i;
            unit = &rest[i..];
            break;
        }
    }
    if num_end == 0 { return None; }
    let num: f64 = rest[..num_end].trim().parse().ok()?;
    let multiplier = match unit.trim().to_lowercase().as_str() {
        "s" => 1i64,
        "m" => 60i64,
        "h" => 60i64 * 60,
        "d" => 24i64 * 60 * 60,
        "w" => 7i64 * 24 * 60 * 60,
        "mo" => 30i64 * 24 * 60 * 60,
        "y" => 365i64 * 24 * 60 * 60,
        _ => 24i64 * 60 * 60,
    };
    Some((op, (num * multiplier as f64) as i64))
}

/// 解析通配符/后缀等简写语法。
/// 支持 *.pdf / .pdf → Ext，prefix* → Prefix，*suffix → Suffix，
/// *mid* → Text（包含）。
fn parse_wildcard_filter(value: &str) -> Option<SearchFilterKind> {
    if value.starts_with("*.") && value.len() > 2 && !value[2..].contains('*') {
        Some(SearchFilterKind::Ext(value[2..].to_string()))
    } else if value.starts_with('.') && value.len() > 1 && !value.contains('*') {
        Some(SearchFilterKind::Ext(value[1..].to_string()))
    } else if value.starts_with('*') && value.ends_with('*') && value.len() > 2 {
        Some(SearchFilterKind::Text(value[1..value.len() - 1].to_string()))
    } else if value.starts_with('*') && value.len() > 1 {
        Some(SearchFilterKind::Suffix(value[1..].to_string()))
    } else if value.ends_with('*') && value.len() > 1 {
        Some(SearchFilterKind::Prefix(value[..value.len() - 1].to_string()))
    } else {
        None
    }
}

fn split_filter_tokens(input: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut in_quote = false;
    for c in input.chars() {
        if c == '"' {
            in_quote = !in_quote;
        } else if c.is_whitespace() && !in_quote {
            if !current.is_empty() {
                tokens.push(std::mem::take(&mut current));
            }
        } else {
            current.push(c);
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

pub fn parse_search_filter(input: &str) -> Vec<SearchFilter> {
    let mut filters = Vec::new();
    let mut text_parts = Vec::new();
    let mut negate_next = false;

    for raw in split_filter_tokens(input) {
        let word = raw.trim();
        if word.is_empty() { continue; }

        let upper = word.to_uppercase();
        if upper == "NOT" {
            negate_next = true;
            continue;
        }
        if upper == "AND" || upper == "OR" {
            negate_next = false;
            continue;
        }

        // `!xxx` 与 `NOT xxx` 等价（界面与文档一直宣传 `!tmp` 这类写法，
        // 但早期只实现了 NOT，导致 `!tmp` 被当成字面量 "!tmp" 去匹配、结果恒为空）
        let (word, bang) = match word.strip_prefix('!') {
            Some(rest) if !rest.is_empty() => (rest, true),
            _ => (word, false),
        };

        // 先按 `key:value` 解析；只有"已知键名"才走字段过滤，未识别的
        // （包括 `C:\x\y` 这类带盘符的路径）落到下面的纯文本分支。
        if let Some((key, value)) = word.split_once(':') {
            let key_lower = key.to_lowercase();
            let negate = negate_next || bang;
            let kind = match key_lower.as_str() {
                "ext" => Some(SearchFilterKind::Ext(value.to_lowercase())),
                "name" => Some(SearchFilterKind::Name(value.to_lowercase())),
                "dir" => Some(SearchFilterKind::Dir(normalize_query_path(&value.to_lowercase()))),
                "type" => {
                    let v = value.to_lowercase();
                    let is_dir = v == "dir" || v == "folder";
                    Some(SearchFilterKind::Type { is_dir })
                }
                "size" => parse_size(value).map(|(op, bytes)| SearchFilterKind::Size { op, bytes }),
                "mtime" => parse_mtime(value).map(|(op, seconds)| SearchFilterKind::Mtime { op, seconds }),
                // 未识别的 `key:value` 不再被静默丢弃（丢弃会让过滤条件变空，
                // 进而退化成"返回全量中最大的若干项"），而是按纯文本处理。
                _ => None,
            };
            if let Some(kind) = kind {
                negate_next = false;
                filters.push(SearchFilter { kind, negate });
                continue;
            }
        }

        let value = word.to_lowercase();
        let negate = negate_next || bang;
        negate_next = false;
        // 含路径分隔符的查询按路径匹配：Windows 文件名不允许出现 `/` 或 `\\`，
        // 这类输入只可能是路径（把完整路径或路径片段粘进搜索框是很常见的用法）。
        if let Some(kind) = path_query_kind(&value) {
            filters.push(SearchFilter { kind, negate });
        } else if let Some(kind) = parse_wildcard_filter(&value) {
            filters.push(SearchFilter { kind, negate });
        } else if negate {
            filters.push(SearchFilter {
                kind: SearchFilterKind::Text(value),
                negate: true,
            });
        } else {
            text_parts.push(value);
        }
    }

    if !text_parts.is_empty() {
        filters.insert(0, SearchFilter {
            kind: SearchFilterKind::Text(text_parts.join(" ")),
            negate: false,
        });
    }

    filters
}

fn apply_filters(entry: &IndexEntry, filters: &[SearchFilter]) -> bool {
    for f in filters {
        let matched = match &f.kind {
            SearchFilterKind::Text(t) => entry.name_lower.contains(t),
            SearchFilterKind::PathText(p) => contains_ignore_case(&entry.path, p),
            SearchFilterKind::Name(n) => entry.name_lower.contains(n),
            SearchFilterKind::Prefix(p) => entry.name_lower.starts_with(p),
            SearchFilterKind::Suffix(s) => entry.name_lower.ends_with(s),
            // ext 不再单独存储：从 name_lower 现算（零分配）
            SearchFilterKind::Ext(e) => {
                !entry.is_dir
                    && entry
                        .name_lower
                        .rsplit_once('.')
                        .map(|(_, ext)| ext.eq_ignore_ascii_case(e))
                        .unwrap_or(false)
            }
            // 用无分配的 ASCII 快速路径，避免每条目一次 String 分配
            SearchFilterKind::Dir(d) => contains_ignore_case(&entry.path, d),
            SearchFilterKind::Type { is_dir } => entry.is_dir == *is_dir,
            SearchFilterKind::Size { op, bytes } => compare_op(entry.size, *op, *bytes),
            SearchFilterKind::Mtime { op, seconds } => {
                let now = chrono::Utc::now().timestamp();
                let age = now - entry.mtime;
                compare_op(age, *op, *seconds)
            }
        };
        if matched == f.negate {
            return false;
        }
    }
    true
}

fn compare_op(a: i64, op: FilterOp, b: i64) -> bool {
    match op {
        FilterOp::Gt => a > b,
        FilterOp::Gte => a >= b,
        FilterOp::Lt => a < b,
        FilterOp::Lte => a <= b,
        FilterOp::Eq => a == b,
        FilterOp::Ne => a != b,
    }
}

/// 本地（当前目录）过滤：与全局搜索共用同一套 Everything 式语法。
///
/// 与 `apply_filters` 的区别：直接作用于 `Item` 的字段，不需要构造 `IndexEntry`；
/// 且纯文本条件同时匹配"文件名或完整路径"（与 README 的本地过滤说明一致）。
pub fn item_matches_filters(
    name: &str,
    path: &str,
    size: i64,
    is_dir: bool,
    mtime: i64,
    filters: &[SearchFilter],
) -> bool {
    for f in filters {
        let matched = match &f.kind {
            SearchFilterKind::Text(t) => {
                contains_ignore_case(name, t) || contains_ignore_case(path, t)
            }
            SearchFilterKind::PathText(p) => contains_ignore_case(path, p),
            SearchFilterKind::Name(n) => contains_ignore_case(name, n),
            SearchFilterKind::Prefix(p) => starts_with_ignore_case_ci(name, p),
            SearchFilterKind::Suffix(sfx) => ends_with_ignore_case_ci(name, sfx),
            SearchFilterKind::Ext(e) => !is_dir && extension_matches(name, e),
            SearchFilterKind::Dir(d) => contains_ignore_case(path, d),
            SearchFilterKind::Type { is_dir: want_dir } => is_dir == *want_dir,
            SearchFilterKind::Size { op, bytes } => compare_op(size, *op, *bytes),
            SearchFilterKind::Mtime { op, seconds } => {
                let now = chrono::Utc::now().timestamp();
                compare_op(now - mtime, *op, *seconds)
            }
        };
        if matched == f.negate {
            return false;
        }
    }
    true
}

/// 大小写不敏感的包含匹配（ASCII 零分配快速路径 + 非 ASCII 回退）
fn contains_ignore_case(haystack: &str, needle_lower: &str) -> bool {
    if needle_lower.is_empty() {
        return true;
    }
    if haystack.is_ascii() && needle_lower.is_ascii() {
        let h = haystack.as_bytes();
        let n = needle_lower.as_bytes();
        if n.len() > h.len() {
            return false;
        }
        return h.windows(n.len()).any(|w| w.eq_ignore_ascii_case(n));
    }
    haystack.to_lowercase().contains(needle_lower)
}

fn starts_with_ignore_case_ci(haystack: &str, prefix_lower: &str) -> bool {
    if haystack
        .get(..prefix_lower.len())
        .is_some_and(|h| h.eq_ignore_ascii_case(prefix_lower))
    {
        return true;
    }
    !haystack.is_ascii() && haystack.to_lowercase().starts_with(prefix_lower)
}

fn ends_with_ignore_case_ci(haystack: &str, suffix_lower: &str) -> bool {
    if haystack.len() >= suffix_lower.len()
        && haystack
            .get(haystack.len() - suffix_lower.len()..)
            .is_some_and(|h| h.eq_ignore_ascii_case(suffix_lower))
    {
        return true;
    }
    !haystack.is_ascii() && haystack.to_lowercase().ends_with(suffix_lower)
}

fn extension_matches(name: &str, ext_lower: &str) -> bool {
    match name.rsplit_once('.') {
        Some((_, ext)) => ext.eq_ignore_ascii_case(ext_lower),
        None => false,
    }
}

/// 搜索结果 top-K 收集器。
///
/// BinaryHeap 的 Ord 定义为"越差越大"，因此堆顶始终是当前最差候选：
/// 堆未满时直接压入；已满时只在"新候选优于堆顶"时替换，单次 O(log k)。
struct TopK<'a> {
    limit: usize,
    heap: BinaryHeap<Candidate<'a>>,
    /// 命中总数（用于"共 N 项命中，已显示前 M 项"）
    count: usize,
}

#[derive(Clone, Copy)]
struct Candidate<'a> {
    entry: &'a IndexEntry,
    score: i64,
}

impl<'a> Candidate<'a> {
    /// 与最终排序一致的"更优"判断：
    /// 相关性分数降序 → name_lower 升序 → path 升序
    fn better_than(&self, other: &Self) -> bool {
        self.score > other.score
            || (self.score == other.score
                && (self.entry.name_lower < other.entry.name_lower
                    || (self.entry.name_lower == other.entry.name_lower
                        && self.entry.path < other.entry.path)))
    }
}

impl PartialEq for Candidate<'_> {
    fn eq(&self, other: &Self) -> bool {
        !self.better_than(other) && !other.better_than(self)
    }
}
impl Eq for Candidate<'_> {}
impl PartialOrd for Candidate<'_> {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for Candidate<'_> {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        if self.better_than(other) {
            std::cmp::Ordering::Less
        } else if other.better_than(self) {
            std::cmp::Ordering::Greater
        } else {
            std::cmp::Ordering::Equal
        }
    }
}

impl<'a> TopK<'a> {
    fn new(limit: usize) -> Self {
        Self {
            limit,
            heap: BinaryHeap::with_capacity(limit.min(1024)),
            count: 0,
        }
    }

    fn push(&mut self, entry: &'a IndexEntry, score: i64) {
        self.count += 1;
        let candidate = Candidate { entry, score };
        if self.heap.len() < self.limit {
            self.heap.push(candidate);
        } else if let Some(worst) = self.heap.peek() {
            if candidate.better_than(worst) {
                self.heap.pop();
                self.heap.push(candidate);
            }
        }
    }

    fn merge(mut self, other: Self) -> Self {
        self.count += other.count;
        for candidate in other.heap {
            if self.heap.len() < self.limit {
                self.heap.push(candidate);
            } else if let Some(worst) = self.heap.peek() {
                if candidate.better_than(worst) {
                    self.heap.pop();
                    self.heap.push(candidate);
                }
            }
        }
        self
    }

    /// 命中总数
    fn total(&self) -> usize {
        self.count
    }

    /// 升序出堆（最优在前），此时才 clone 结果条目并补上文件名
    fn into_entries(self) -> Vec<IndexEntry> {
        self.heap
            .into_sorted_vec()
            .into_iter()
            // 存储态没有 name：序列化给前端时由 path 派生（见 IndexEntry 的 Serialize）
            .map(|c| c.entry.clone())
            .collect()
    }
}

fn relevance_score(entry: &IndexEntry, query_lower: &str) -> i64 {
    if query_lower.is_empty() {
        return entry.size;
    }
    let base = if entry.name_lower == query_lower {
        3i64 << 60
    } else if entry.name_lower.starts_with(query_lower) {
        2i64 << 60
    } else {
        1i64 << 60
    };
    base + entry.size
}

/// 规范化路径为绝对路径（统一正斜杠，并确保含盘符前缀 C:/...）
pub(crate) fn normalize_abs_path(drive: char, path: &str) -> String {
    let p = path.replace('\\', "/");
    let vol_prefix = format!("{}:/", drive);
    let vol_alt = format!("{}:", drive);
    if p.starts_with(&vol_prefix) || p.starts_with(&vol_alt) {
        p
    } else if p.is_empty() {
        vol_prefix
    } else {
        format!("{}{}", vol_prefix, p)
    }
}

/// 判断 `key` 是否为 `base` 本身或其子路径（路径分隔符已统一为 `/`）。
fn is_same_or_child(base: &str, key: &str) -> bool {
    if key.eq_ignore_ascii_case(base) {
        return true;
    }
    let child_prefix = if base.ends_with('/') {
        base.to_string()
    } else {
        format!("{}/", base)
    };
    key.get(..child_prefix.len())
        .is_some_and(|head| head.eq_ignore_ascii_case(&child_prefix))
}

/// 索引路径规范化：统一正斜杠、折叠重复分隔符、盘符大写、去掉末尾分隔符。
///
/// 只做分隔符与盘符层面的规范化：索引里存的是展示给用户的路径，
/// 卷内真实大小写由文件系统枚举给出，保持原样。
/// 已经是规范形式时原样借用，避免每次写入索引都多一次分配。
pub fn normalize_index_path(path: &str) -> std::borrow::Cow<'_, str> {
    let bytes = path.as_bytes();
    let has_drive = bytes.len() >= 2 && bytes[1] == b':';
    let clean = !bytes.contains(&b'\\')
        && !path.contains("//")
        && !path.ends_with('/')
        && (!has_drive || bytes[0].is_ascii_uppercase());
    if clean {
        return std::borrow::Cow::Borrowed(path);
    }

    let mut out = String::with_capacity(path.len() + 4);
    let mut prev_slash = false;
    for (i, ch) in path.chars().enumerate() {
        let c = if ch == '\\' { '/' } else { ch };
        if c == '/' {
            if prev_slash {
                continue;
            }
            prev_slash = true;
        } else {
            prev_slash = false;
        }
        if i == 0 && has_drive && c.is_ascii_lowercase() {
            out.push(c.to_ascii_uppercase());
        } else {
            out.push(c);
        }
    }
    while out.len() > 3 && out.ends_with('/') {
        out.pop();
    }
    std::borrow::Cow::Owned(out)
}

/// 判断（已规范化的）路径是否为绝对路径：`C:/...` 或 `/...`。
fn is_absolute_index_path(path: &str) -> bool {
    let b = path.as_bytes();
    (b.len() >= 3 && b[1] == b':' && b[2] == b'/') || b.first() == Some(&b'/')
}

/// 计算某个扫描条目在索引里的绝对路径。
///
/// `item.path` 在 CLI/管道场景下可能是扫描根下的相对路径，主界面扫描结果则已经
/// 是绝对路径，因此必须按"是否绝对路径 / 是否落在扫描根下"判断，而不能靠字符串
/// 前缀比较：扫描根写成 `C:\Users\me` 这类反斜杠形式时比较必然失败，
/// 于是拼出 `C:\Users\me/C:/Users/me/xxx` 脏路径（既污染搜索又制造重复项）。
pub fn index_path_for(scan_path: &str, item_path: &str) -> String {
    let base =
            normalize_index_path(scan_path.trim_end_matches(|c| c == '/' || c == '\\')).into_owned();
    if item_path.is_empty() {
        return base;
    }
    let rel = normalize_index_path(item_path);
    let joined = if is_absolute_index_path(&rel) || is_same_or_child(&base, &rel) {
        rel.into_owned()
    } else {
        format!("{}/{}", base, rel.trim_start_matches('/'))
    };
    normalize_index_path(&joined).into_owned()
}

/// 修复历史版本拼坏的索引路径（形如 `C:\root/C:/root/xxx`）。
///
/// Windows 文件名里不允许出现 `:`，所以第一个 `/<盘符>:/` 之后的一定是真实绝对路径。
/// 返回修好的路径；不需要修复时返回 None。
pub fn repair_corrupt_path(path: &str) -> Option<String> {
    let bytes = path.as_bytes();
    if bytes.len() < 6 {
        return None;
    }
    let mut start = None;
    for i in 0..bytes.len() - 3 {
        if bytes[i] == b'/'
            && bytes[i + 1].is_ascii_alphabetic()
            && bytes[i + 2] == b':'
            && bytes[i + 3] == b'/'
        {
            start = Some(i + 1);
            break;
        }
    }
    let fixed = normalize_index_path(&path[start?..]).into_owned();
    if is_absolute_index_path(&fixed) {
        Some(fixed)
    } else {
        None
    }
}

/// 入库兜底：路径里出现反斜杠说明上游漏了规范化（历史脏数据或新增调用点）。
/// 命中概率极低，因此只有真的含 `\` 时才走慢路径。
fn repair_entry_path(mut entry: IndexEntry) -> IndexEntry {
    if entry.path.as_bytes().contains(&b'\\') {
        entry.path = normalize_index_path(&entry.path).into_owned();
    }
    entry
}

/// 查询里的路径片段规范化（与索引里的写法对齐：正斜杠、无重复分隔符、无末尾分隔符）。
fn normalize_query_path(value_lower: &str) -> String {
    let mut out = String::with_capacity(value_lower.len());
    let mut prev_slash = false;
    for ch in value_lower.chars() {
        let c = if ch == '\\' { '/' } else { ch };
        if c == '/' {
            if prev_slash {
                continue;
            }
            prev_slash = true;
        } else {
            prev_slash = false;
        }
        out.push(c);
    }
    while out.len() > 1 && out.ends_with('/') {
        out.pop();
    }
    out
}

/// 含路径分隔符的查询：Windows 文件名不允许出现 `/` 与 `\`，
/// 因此这类输入只可能是路径，按"整条路径包含"匹配（用户常直接粘贴完整路径）。
fn path_query_kind(value_lower: &str) -> Option<SearchFilterKind> {
    if !value_lower.contains('/') && !value_lower.contains('\\') {
        return None;
    }
    // 通配符在路径查询里退化为"包含"语义（`*a/b*`、`a/b*` 都按包含处理）
    let cleaned = normalize_query_path(value_lower).replace('*', "");
    if cleaned.is_empty() {
        return None;
    }
    Some(SearchFilterKind::PathText(cleaned))
}

// ─── 索引构建锁 ────────────────────────────────────────────

/// NTFS 目录属性位（USN 记录里的 attributes 用同一位）
const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x0000_0010;
/// 单次同步允许"目录改名重挂子树"的最大条目数：超过就交给全量重建
const MAX_REPATH_ENTRIES: usize = 20_000;
/// 首次追赶时允许的历史积压上限。
///
/// 增量应用约 1.67ms/条（每条要随机读 MFT 解析 FRN→路径），而全盘 MFT 重建
/// 只要 2.4-3.5s——也就是说积压超过约 2000 条时，直接让用户重建索引反而更快。
/// 因此超过这个量就播种到当前位置并标记"建议重建"，不做无谓的追赶。
const MAX_CATCHUP_RECORDS: i64 = 2_000;
/// 增量同步每次读取并处理的记录条数上限。
///
/// 每条变更要随机读 MFT 解析 FRN→路径（跨目录约 5-8ms），200 条 ≈ 1-1.6s，
/// 配合时间预算可以保证一次同步不会长时间占用后台线程。
const CHUNK_RECORDS: usize = 200;
/// 锁文件被视为残留的时限（秒）
const BUILD_LOCK_STALE_SECS: u64 = 30 * 60;

/// 每个盘的 USN 检查点与同步状态（随 IndexMeta 一起持久化）。
#[derive(Debug, Clone, Default, Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct UsnSyncState {
    /// 盘符（"C"）→ 检查点；只有全盘构建过的盘才会有
    pub checkpoints: HashMap<String, crate::fs::UsnCheckpoint>,
    /// 增量窗口已失效、只能靠重建索引恢复的盘
    pub stale_drives: Vec<String>,
    /// 最近一次同步完成时间（Unix 秒，0 = 尚未同步）
    pub last_sync_at: i64,
    /// 累计应用的变更条数
    pub applied_total: u64,
}

/// 一次增量同步的结果（诊断与界面提示用）
#[derive(Debug, Clone, Default)]
pub struct UsnSyncReport {
    /// 本次应用的变更条数
    pub applied: usize,
    /// 删除的条目数
    pub removals: usize,
    /// 新增/更新的条目数
    pub upserts: usize,
    /// 目录改名重挂的条目数
    pub repathed: usize,
    /// 增量窗口失效、需要重建索引的盘
    pub stale: Vec<char>,
    /// 本次因预算耗尽而仍有积压
    pub backlog: bool,
    pub elapsed_ms: u64,
}

impl UsnSyncReport {
    pub fn summary(&self) -> String {
        if self.applied == 0 && self.stale.is_empty() && !self.backlog {
            return format!("索引已是最新（{}ms）", self.elapsed_ms);
        }
        let mut parts = vec![format!(
            "增量同步：应用 {} 条（新增/更新 {}，删除 {}）",
            self.applied, self.upserts, self.removals
        )];
        if self.repathed > 0 {
            parts.push(format!("目录改名重挂 {} 条", self.repathed));
        }
        if !self.stale.is_empty() {
            parts.push(format!("{:?} 增量窗口失效，建议重建索引", self.stale));
        }
        if self.backlog {
            parts.push(String::from("仍有积压，稍后继续"));
        }
        parts.push(format!("{}ms", self.elapsed_ms));
        parts.join("，")
    }
}

/// 索引构建锁：进程内原子标志 + 跨进程锁文件。
///
/// 构建未完成时再次触发构建，不仅会互相覆盖结果，峰值内存还会翻倍
/// （低内存机器上这是闪退的直接诱因），因此必须互斥。
pub struct IndexBuildGuard<'a> {
    index: &'a GlobalIndex,
    lock_path: Option<std::path::PathBuf>,
}

impl std::fmt::Debug for IndexBuildGuard<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IndexBuildGuard")
            .field("lock_path", &self.lock_path)
            .finish()
    }
}

impl Drop for IndexBuildGuard<'_> {
    fn drop(&mut self) {
        if let Some(path) = self.lock_path.take() {
            let _ = std::fs::remove_file(path);
        }
        self.index.building.store(false, Ordering::SeqCst);
    }
}

/// 锁文件路径（`FLASHDIR_BUILD_LOCK` 可覆盖，便于自测）。
fn build_lock_path() -> std::path::PathBuf {
    if let Ok(custom) = std::env::var("FLASHDIR_BUILD_LOCK") {
        if !custom.trim().is_empty() {
            return std::path::PathBuf::from(custom);
        }
    }
    let mut path = std::env::var_os("USERPROFILE")
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(std::path::PathBuf::from))
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    path.push(".flashdir");
    path.push("index-build.lock");
    path
}

#[cfg(target_os = "windows")]
fn process_alive(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle == 0 {
            return false;
        }
        CloseHandle(handle);
        true
    }
}

#[cfg(not(target_os = "windows"))]
fn process_alive(_pid: u32) -> bool {
    false
}

/// 获取跨进程构建锁。成功返回锁文件路径（Drop 时删除）。
fn acquire_build_lock_file() -> Result<Option<std::path::PathBuf>, String> {
    use std::io::Write;
    let path = build_lock_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    for attempt in 0..2 {
        match std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&path)
        {
            Ok(mut file) => {
                let _ = writeln!(file, "{}", std::process::id());
                return Ok(Some(path));
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                let pid = std::fs::read_to_string(&path)
                    .ok()
                    .and_then(|text| text.trim().parse::<u32>().ok());
                let alive = pid.map(process_alive).unwrap_or(false);
                let fresh = std::fs::metadata(&path)
                    .and_then(|meta| meta.modified())
                    .ok()
                    .and_then(|time| time.elapsed().ok())
                    .map(|age| age.as_secs() < BUILD_LOCK_STALE_SECS)
                    .unwrap_or(false);
                if attempt == 0 && (!alive || !fresh) {
                    let _ = std::fs::remove_file(&path);
                    continue;
                }
                return Err(match pid {
                    Some(pid) if alive => format!(
                        "另一个 FlashDir 实例（pid {pid}）正在构建索引，请等它完成后重试"
                    ),
                    _ => String::from("索引构建锁被占用（疑似残留锁文件），请稍后重试"),
                });
            }
            Err(e) => return Err(format!("无法创建索引构建锁: {e}")),
        }
    }
    Err(String::from("无法获取索引构建锁"))
}

// ─── USN 增量同步 ──────────────────────────────────────────

/// 本次同步产生的磁盘操作（内存已经即时生效，磁盘统一在最后落库）
#[derive(Default)]
struct DeltaSink {
    upserts: Vec<IndexEntry>,
    /// 需要删除的确切路径（文件删除、目录子树的每个条目、改名前的旧路径）。
    /// 用确切路径而不是前缀：`DELETE ... LIKE 'prefix/%'` 是整表扫描，
    /// 几十个目录删除就能让一次同步卡二十秒。
    removals: Vec<String>,
    /// 目录删除/改名涉及的前缀，仅用于在内存里一次性收集命中路径
    dir_prefixes: Vec<String>,
}

impl DeltaSink {
    fn flush(&self) {
        let cache = crate::disk_cache::DiskCache::instance();
        if !self.removals.is_empty() {
            let _ = cache.remove_global_index_by_paths(&self.removals);
        }
        if !self.upserts.is_empty() {
            let _ = cache.upsert_global_index_entries(&self.upserts);
        }
    }
}

/// 卷内相对路径 → 索引里的绝对路径（`C:/Users/...`）
fn full_index_path(drive: char, rel: &str) -> String {
    let rel = rel.trim_matches('/');
    if rel.is_empty() {
        format!("{drive}:/")
    } else {
        normalize_index_path(&format!("{drive}:/{rel}")).into_owned()
    }
}

/// 路径自身或任一上层祖先是否命中前缀集合（用于批量移除）
fn path_has_ancestor_in(path: &str, set: &std::collections::HashSet<&str>) -> bool {
    if set.contains(path) {
        return true;
    }
    let mut end = path.len();
    while let Some(pos) = path[..end].rfind('/') {
        if set.contains(&path[..pos]) {
            return true;
        }
        end = pos;
    }
    false
}

/// 拼接父目录与子项名字（索引统一用正斜杠）
fn join_child(parent: &str, name: &str) -> String {
    let parent = parent.trim_end_matches('/');
    if parent.is_empty() {
        name.to_string()
    } else {
        format!("{parent}/{name}")
    }
}

/// 首次追赶用的检查点：从 Journal 仍可读的最早位置开始；
/// 积压过大（超过 `MAX_CATCHUP_RECORDS`）则返回 skipped=true，由调用方提示重建。
fn catchup_checkpoint(drive: char) -> Option<(crate::fs::UsnCheckpoint, bool)> {
    let mut fresh = crate::fs::get_checkpoint(drive)?;
    let journal = crate::fs::UsnJournal::open(drive).ok()?;
    let (lowest_valid_usn, next_usn) = journal.window().ok()?;
    if next_usn - lowest_valid_usn > MAX_CATCHUP_RECORDS {
        return Some((fresh, true));
    }
    fresh.next_usn = lowest_valid_usn;
    Some((fresh, false))
}

impl GlobalIndex {
    /// 本进程内是否正在构建索引
    pub fn is_building(&self) -> bool {
        self.building.load(Ordering::SeqCst)
    }

    /// 获取构建权（进程内原子 + 跨进程锁文件）。失败时不要继续构建。
    pub fn try_begin_build(&self) -> Result<IndexBuildGuard<'_>, String> {
        if self
            .building
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return Err(String::from("索引正在构建中，请等当前任务完成"));
        }
        match acquire_build_lock_file() {
            Ok(lock_path) => Ok(IndexBuildGuard {
                index: self,
                lock_path,
            }),
            Err(e) => {
                self.building.store(false, Ordering::SeqCst);
                Err(e)
            }
        }
    }

    /// 构建开始前播种检查点：记住"构建开始时刻"的 Journal 位置，
    /// 这样构建期间发生的变更也会被下一次增量同步捞回来，而不是永久漏掉。
    pub fn seed_usn_checkpoints(&self, drives: &[char]) {
        let mut seeded: Vec<String> = Vec::new();
        {
            let mut meta = self.meta.write();
            for &drive in drives {
                let key = drive.to_string();
                if let Some(checkpoint) = crate::fs::get_checkpoint(drive) {
                    meta.usn.checkpoints.insert(key.clone(), checkpoint);
                    meta.usn.stale_drives.retain(|d| d != &key);
                    seeded.push(key);
                }
            }
        }
        if !seeded.is_empty() {
            crate::diag::log_line(&format!("[USN] 已播种检查点 {:?}", seeded));
            self.persist_meta();
        }
    }

    /// 同步状态快照（界面与诊断用）
    pub fn usn_state(&self) -> UsnSyncState {
        self.meta.read().usn.clone()
    }

    /// 应用 USN 增量：把"索引构建之后新建/改名/删除的文件"补进索引。
    ///
    /// `budget` 是本次最多处理多少条变更，`time_budget` 是总时间上限；
    /// 任一到达就返回（剩余积压留给下一次），因此可以安全地周期性调用。
    pub fn sync_usn(&self, budget: usize, time_budget: std::time::Duration) -> UsnSyncReport {
        let started = std::time::Instant::now();
        let deadline = started + time_budget;
        let mut report = UsnSyncReport::default();

        let (mut checkpoints, full_build, all_drives) = {
            let meta = self.meta.read();
            (
                meta.usn.checkpoints.clone(),
                meta.full_build,
                meta.all_drives.clone(),
            )
        };

        // 候选盘：已有检查点的盘；全盘索引在缺检查点时从 Journal 最早可读处追赶
        let mut drives: Vec<char> = if checkpoints.is_empty() {
            if full_build {
                all_drives.iter().filter_map(|d| d.chars().next()).collect()
            } else {
                Vec::new()
            }
        } else {
            checkpoints.keys().filter_map(|k| k.chars().next()).collect()
        };
        drives.sort_unstable();
        drives.dedup();
        if drives.is_empty() {
            report.elapsed_ms = started.elapsed().as_millis() as u64;
            return report;
        }

        let mut sink = DeltaSink::default();
        let mut parent_cache: HashMap<u64, Option<String>> = HashMap::new();

        for drive in drives {
            if report.applied >= budget || std::time::Instant::now() >= deadline {
                report.backlog = true;
                break;
            }
            let key = drive.to_string();
            let mut checkpoint = match checkpoints.get(&key).cloned() {
                Some(cp) => cp,
                None => match catchup_checkpoint(drive) {
                    Some((cp, skipped)) => {
                        if skipped {
                            crate::diag::log_line(&format!(
                                "[USN] {drive}: 历史积压超过 {MAX_CATCHUP_RECORDS} 条，跳过追赶"
                            ));
                            report.stale.push(drive);
                        }
                        cp
                    }
                    None => continue,
                },
            };

            let mut scanner = match crate::fs::MftScanner::open(drive) {
                Ok(s) => s,
                Err(e) => {
                    crate::diag::log_line(&format!("[USN] 打开 {drive} 盘 MFT 失败: {e}"));
                    continue;
                }
            };
            scanner.set_cancel_id(crate::cancel::begin());

            match self.apply_drive_delta(
                drive,
                &mut checkpoint,
                &mut scanner,
                budget,
                deadline,
                &mut report,
                &mut sink,
                &mut parent_cache,
            ) {
                Ok(()) => {
                    checkpoints.insert(key, checkpoint);
                }
                Err(()) => {
                    // 增量窗口失效：重新播种到当前位置，并标记该盘需要重建索引
                    report.stale.push(drive);
                    match crate::fs::get_checkpoint(drive) {
                        Some(fresh) => {
                            checkpoints.insert(key, fresh);
                        }
                        None => {
                            checkpoints.remove(&key);
                        }
                    }
                }
            }
        }

        // 目录删除/改名的前缀移除在这里一次性完成（内存），随后统一落库
        self.remove_prefixes_batch(&sink.dir_prefixes.clone(), &mut sink, &mut report);
        sink.flush();

        if report.applied > 0 || !report.stale.is_empty() || report.backlog {
            let mut meta = self.meta.write();
            meta.usn.checkpoints = checkpoints;
            for drive in &report.stale {
                let key = drive.to_string();
                meta.usn.stale_drives.retain(|d| d != &key);
                meta.usn.stale_drives.push(key);
            }
            meta.usn.last_sync_at = chrono::Utc::now().timestamp();
            meta.usn.applied_total += report.applied as u64;
            drop(meta);
            self.persist_meta();
            self.update_ready_state();
        }

        report.elapsed_ms = started.elapsed().as_millis() as u64;
        report
    }
}
impl GlobalIndex {
    /// 消费某个盘的增量，直到追平 / 预算耗尽 / 窗口失效。
    /// Err(()) 表示增量窗口失效（调用方负责标记 stale 并重新播种检查点）。
    #[allow(clippy::too_many_arguments)]
    fn apply_drive_delta(
        &self,
        drive: char,
        checkpoint: &mut crate::fs::UsnCheckpoint,
        scanner: &mut crate::fs::MftScanner,
        budget: usize,
        deadline: std::time::Instant,
        report: &mut UsnSyncReport,
        sink: &mut DeltaSink,
        parent_cache: &mut HashMap<u64, Option<String>>,
    ) -> Result<(), ()> {
        // 预算按"已消费的记录数"计算，而不是"相关变更数"：
        // Journal 里大量记录与索引无关（临时文件、系统活动），
        // 只统计相关变更会让一次同步读取几十批记录、卡住好几秒。
        let mut consumed = 0usize;
        loop {
            if consumed >= budget
                || report.applied >= budget
                || std::time::Instant::now() >= deadline
            {
                report.backlog = true;
                return Ok(());
            }
            let delta =
                match crate::fs::read_incremental_changes(drive, checkpoint, checkpoint.next_usn, CHUNK_RECORDS) {
                    Ok(delta) => delta,
                    Err(crate::fs::UsnReadError::WindowExpired)
                    | Err(crate::fs::UsnReadError::JournalReset)
                    | Err(crate::fs::UsnReadError::VolumeChanged) => return Err(()),
                    Err(crate::fs::UsnReadError::Io(e)) => {
                        if e.kind() == std::io::ErrorKind::InvalidData {
                            return Err(());
                        }
                        crate::diag::log_line(&format!("[USN] {drive}: 读取增量失败: {e}"));
                        return Ok(());
                    }
                };

            // 注意：`read_changes_since` 的批量参数只决定缓冲区大小，内核一次可能
            // 返回上千条记录。这里按条数精确截断，并把续读位置设为"第一条未处理记录"
            // 的 USN（USN 单调递增，下次从该位置读会重新拿到这条记录），
            // 这样单次同步的耗时才真正可控。
            let mut changes = delta.changes;
            let next_usn = if changes.len() > CHUNK_RECORDS {
                let resume = changes[CHUNK_RECORDS].usn;
                changes.truncate(CHUNK_RECORDS);
                resume
            } else {
                delta.next_usn
            };
            consumed += changes.len();
            let mut pending_dir_renames: HashMap<u64, String> = HashMap::new();
            for change in &changes {
                self.apply_one_change(
                    drive,
                    scanner,
                    change,
                    report,
                    sink,
                    parent_cache,
                    &mut pending_dir_renames,
                );
            }
            // 没等到 NEW_NAME 的目录改名：旧前缀下的条目已指向不存在的路径，整体移除
            for (_, old_path) in std::mem::take(&mut pending_dir_renames) {
                self.record_removal(&old_path, true, sink, report);
            }

            checkpoint.next_usn = next_usn;
            if changes.is_empty() {
                return Ok(());
            }
        }
    }

    /// FRN → 当前绝对路径（带缓存：同一父目录会被反复询问）
    fn path_of(
        &self,
        drive: char,
        scanner: &mut crate::fs::MftScanner,
        frn: u64,
        cache: &mut HashMap<u64, Option<String>>,
    ) -> Option<String> {
        if let Some(hit) = cache.get(&frn) {
            return hit.clone();
        }
        let resolved = match scanner.resolve_frn_path(frn) {
            Ok(Some(rel)) => Some(full_index_path(drive, &rel)),
            _ => None,
        };
        cache.insert(frn, resolved.clone());
        resolved
    }

    /// 从索引中移除条目；目录按前缀整体移除（子树里的路径已全部失效）
    fn record_removal(
        &self,
        path: &str,
        is_dir: bool,
        sink: &mut DeltaSink,
        report: &mut UsnSyncReport,
    ) {
        let path = path.trim_end_matches('/');
        if path.is_empty() {
            return;
        }
        if is_dir {
            // 目录（含子树）统一登记，最后用一次全量扫描批量移除：
            // 单条 remove_prefix_internal 是 O(全量条目)，几十个目录删除就能让一次
            // 同步卡十几秒（实测 200 条记录 22s），这里换成 O(n×深度) 的单次扫描。
            if !sink.dir_prefixes.iter().any(|p| p == path) {
                sink.dir_prefixes.push(path.to_string());
            }
        } else {
            let before = self.entries_len();
            self.remove_by_path(path);
            if self.entries_len() < before {
                report.removals += 1;
                sink.removals.push(path.to_string());
            }
        }
    }

    /// 批量按前缀移除（目录删除）：一次扫描收集命中路径，再逐条 O(1) 移除。
    fn remove_prefixes_batch(
        &self,
        prefixes: &[String],
        sink: &mut DeltaSink,
        report: &mut UsnSyncReport,
    ) {
        if prefixes.is_empty() {
            return;
        }
        let wanted: std::collections::HashSet<&str> =
            prefixes.iter().map(|p| p.as_str()).collect();
        let hits: Vec<String> = {
            let entries = self.entries.read();
            entries
                .iter()
                .filter(|entry| path_has_ancestor_in(&entry.path, &wanted))
                .map(|entry| entry.path.clone())
                .collect()
        };
        report.removals += hits.len();
        for path in &hits {
            self.remove_path_internal(path);
        }
        sink.removals.extend(hits);
    }

    /// 目录改名：把旧前缀下的条目整体改挂到新前缀。
    /// 条目过多时放弃（返回 false），交由重建索引处理。
    fn repath_subtree(
        &self,
        old_path: &str,
        new_path: &str,
        sink: &mut DeltaSink,
        report: &mut UsnSyncReport,
    ) -> bool {
        let old_prefix = old_path.trim_end_matches('/');
        let new_prefix = new_path.trim_end_matches('/');
        if old_prefix.is_empty() || old_prefix == new_prefix {
            return true;
        }
        // 目录本身不在索引里就没有子树要重挂（否则下面的全量扫描白跑一遍）
        {
            let entries = self.entries.read();
            let by_path = self.by_path.read();
            let known = by_path
                .get(&path_hash(old_prefix))
                .copied()
                .is_some_and(|idx| {
                    entries
                        .get(idx as usize)
                        .is_some_and(|entry| entry.path == old_prefix)
                });
            if !known {
                return true;
            }
        }
        let moved: Vec<IndexEntry> = {
            let entries = self.entries.read();
            entries
                .iter()
                .filter(|e| is_same_or_child(old_prefix, &e.path))
                .take(MAX_REPATH_ENTRIES + 1)
                .cloned()
                .collect()
        };
        if moved.len() > MAX_REPATH_ENTRIES {
            crate::diag::log_line(&format!(
                "[USN] 目录改名涉及条目超过 {MAX_REPATH_ENTRIES} 条，跳过重挂: {old_prefix}"
            ));
            return false;
        }

        self.remove_prefix_internal(old_prefix);
        for entry in moved {
            let old_entry_path = entry.path.clone();
            let Some(suffix) = entry.path.get(old_prefix.len()..) else {
                continue;
            };
            let path = format!("{new_prefix}{suffix}");
            let name = path.rsplit('/').next().unwrap_or("").to_string();
            let rewritten = IndexEntry {
                path,
                name_lower: name.to_lowercase(),
                size: entry.size,
                is_dir: entry.is_dir,
                mtime: entry.mtime,
            };
            self.upsert_internal(rewritten.clone());
            sink.upserts.push(rewritten);
            sink.removals.push(old_entry_path);
            report.repathed += 1;
        }
        true
    }
}
impl GlobalIndex {
    /// 应用单条 USN 变更
    #[allow(clippy::too_many_arguments)]
    fn apply_one_change(
        &self,
        drive: char,
        scanner: &mut crate::fs::MftScanner,
        change: &crate::fs::UsnChangeRecord,
        report: &mut UsnSyncReport,
        sink: &mut DeltaSink,
        parent_cache: &mut HashMap<u64, Option<String>>,
        pending_dir_renames: &mut HashMap<u64, String>,
    ) {
        use crate::fs::{
            USN_REASON_BASIC_INFO_CHANGE, USN_REASON_DATA_EXTEND, USN_REASON_DATA_OVERWRITE,
            USN_REASON_DATA_TRUNCATION, USN_REASON_FILE_CREATE, USN_REASON_FILE_DELETE,
            USN_REASON_RENAME_NEW_NAME, USN_REASON_RENAME_OLD_NAME,
        };

        let reason = change.reason;
        let interesting = reason
            & (USN_REASON_FILE_DELETE
                | USN_REASON_RENAME_OLD_NAME
                | USN_REASON_RENAME_NEW_NAME
                | USN_REASON_FILE_CREATE
                | USN_REASON_DATA_OVERWRITE
                | USN_REASON_DATA_EXTEND
                | USN_REASON_DATA_TRUNCATION
                | USN_REASON_BASIC_INFO_CHANGE);
        if interesting == 0 {
            return;
        }
        let is_dir = change.attributes & FILE_ATTRIBUTE_DIRECTORY != 0;
        report.applied += 1;

        // 删除与"改名旧名"：按 父目录当前路径 + 记录里的名字 还原旧路径
        if reason & (USN_REASON_FILE_DELETE | USN_REASON_RENAME_OLD_NAME) != 0 {
            if let Some(old_path) = self
                .path_of(drive, scanner, change.parent_ref, parent_cache)
                .map(|parent| join_child(&parent, &change.name))
            {
                if is_dir && reason & USN_REASON_RENAME_OLD_NAME != 0 {
                    // 目录改名：等 NEW_NAME 记录做整体重挂，先不动索引
                    pending_dir_renames.insert(change.file_ref, old_path);
                    return;
                }
                self.record_removal(&old_path, is_dir, sink, report);
            }
            if reason & USN_REASON_FILE_DELETE != 0 {
                return;
            }
        }

        // 新建 / 改名后 / 数据与基本信息变更：按 FRN 解析"当前"路径后写回
        let Some(current) = self.path_of(drive, scanner, change.file_ref, parent_cache) else {
            return;
        };
        let metadata = match std::fs::metadata(&current) {
            Ok(metadata) => metadata,
            Err(_) => {
                // 路径已不存在（删除记录缺失或竞态）：按删除处理
                self.record_removal(&current, is_dir, sink, report);
                return;
            }
        };
        let is_dir_now = metadata.is_dir();
        if is_dir_now && reason & USN_REASON_RENAME_NEW_NAME != 0 {
            if let Some(old_path) = pending_dir_renames.remove(&change.file_ref) {
                if self.repath_subtree(&old_path, &current, sink, report) {
                    return;
                }
            }
        }
        let name = current.rsplit('/').next().unwrap_or("").to_string();
        let size = if is_dir_now { 0 } else { metadata.len() as i64 };
        let mtime = metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(change.timestamp / 10_000_000);
        let entry = IndexEntry {
            path: current,
            name_lower: name.to_lowercase(),
            size,
            is_dir: is_dir_now,
            mtime,
        };
        self.upsert_internal(entry.clone());
        sink.upserts.push(entry);
        report.upserts += 1;
    }
}
static GLOBAL_INDEX: OnceLock<GlobalIndex> = OnceLock::new();

pub fn instance() -> &'static GlobalIndex {
    GLOBAL_INDEX.get_or_init(GlobalIndex::new)
}

/// 创建一个空实例，仅用于测试。
#[cfg(test)]
pub fn empty_instance_for_test() -> GlobalIndex {
    GlobalIndex::new()
}

// ─── NTFS 盘枚举 ──────────────────────────────────────────

#[cfg(target_os = "windows")]
pub fn list_ntfs_drives() -> Vec<char> {
    use windows_sys::Win32::Storage::FileSystem::{GetLogicalDrives, GetVolumeInformationW};

    let mut drives = Vec::new();
    let mask = unsafe { GetLogicalDrives() };
    if mask == 0 {
        return drives;
    }
    for i in 0..26u32 {
        if (mask & (1 << i)) == 0 {
            continue;
        }
        let letter = (b'A' + i as u8) as char;
        let root = format!("{}:\\", letter);
        let root_wide: Vec<u16> = root.encode_utf16().chain(std::iter::once(0)).collect();
        let mut fs_name = [0u16; 16];
        let ok = unsafe {
            GetVolumeInformationW(
                root_wide.as_ptr(),
                std::ptr::null_mut(),
                0,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                fs_name.as_mut_ptr(),
                fs_name.len() as u32,
            )
        };
        if ok != 0 {
            let end = fs_name.iter().position(|&c| c == 0).unwrap_or(fs_name.len());
            let fs_str = String::from_utf16_lossy(&fs_name[..end]);
            if fs_str.eq_ignore_ascii_case("NTFS") {
                drives.push(letter);
            }
        }
    }
    drives
}

#[cfg(not(target_os = "windows"))]
pub fn list_ntfs_drives() -> Vec<char> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    /// 回归：粘贴完整路径或路径片段必须能搜到。
    ///
    /// 历史缺陷：纯文本条件只与文件名比较，于是把完整路径粘进搜索框恒为 0 条结果
    /// （Windows 文件名不允许出现 `/`、`\`，所以含分隔符的输入只能按路径匹配）。
    #[test]
    fn search_by_absolute_path_and_fragment() {
        let entry = IndexEntry {
            path: "C:/project/CTX-Audit/harness-private/EQM1-METHODOLOGY.md".to_string(),
            name_lower: "eqm1-methodology.md".to_string(),
            size: 59097,
            is_dir: false,
            mtime: 0,
        };
        let hit = |q: &str| apply_filters(&entry, &parse_search_filter(q));

        assert!(hit("eqm1-methodology.md"), "按文件名");
        assert!(hit("EQM1-METHODOLOGY"), "大小写不敏感");
        assert!(hit("*methodology*"), "通配符包含");
        assert!(hit(r"C:\project\CTX-Audit\harness-private\EQM1-METHODOLOGY.md"), "反斜杠完整路径");
        assert!(hit("C:/project/CTX-Audit/harness-private/EQM1-METHODOLOGY.md"), "正斜杠完整路径");
        assert!(hit("harness-private/EQM1-METHODOLOGY.md"), "路径片段");
        assert!(hit(r"harness-private\EQM1-METHODOLOGY.md"), "反斜杠路径片段");
        assert!(hit("harness-private/EQM1*"), "带通配符的路径片段");
        assert!(hit(r"dir:C:\project\CTX-Audit\harness-private"), "dir: 反斜杠路径");
        assert!(hit("dir:project"), "dir: 路径包含");

        assert!(!hit("harness-private/nope.md"), "不相关路径不命中");
        assert!(!hit("other/EQM1-METHODOLOGY.md"), "路径片段不匹配");
        // 纯词只匹配文件名，避免 "windows" 命中 C:/Windows 下所有文件
        assert!(!hit("project"), "纯词不匹配路径");
    }

    /// 回归：包含匹配不能被"首字符分桶"截断。
    ///
    /// 用户输入 `qm1-methodology.md` 想找 `EQM1-METHODOLOGY.md`：名字首字符是 E，
    /// 查询首字符是 Q，旧实现（>2 字符只扫查询首字符桶）必然 0 条。
    /// 现在只有 `前缀*` 条件才会走分桶快路径。
    #[test]
    fn search_matches_substring_anywhere_in_name() {
        let idx = empty_instance_for_test();
        idx.upsert(IndexEntry {
            path: "C:/project/CTX-Audit/harness-private/EQM1-METHODOLOGY.md".to_string(),
            name_lower: "eqm1-methodology.md".to_string(),
            size: 59097,
            is_dir: false,
            mtime: 0,
        });
        for q in [
            "qm1-methodology",
            "qm1-methodology.md",
            "methodology",
            "-methodology",
            "1-method",
            "*methodology*",
            "eqm1-method",
            "eqm1-method*",
            "*.md",
        ] {
            assert_eq!(idx.search_with_filter(q, 10).len(), 1, "查询 {q:?} 应命中 1 条");
        }
        assert_eq!(idx.search_with_filter("qm2-methodology", 10).len(), 0, "无关查询不该命中");
        assert_eq!(idx.search_with_filter("not-here*", 10).len(), 0, "前缀不匹配时不命中");
    }

    /// 回归：扫描根写成反斜杠形式时，不能再拼出 `C:\root/C:/root/xxx` 脏路径。
    #[test]
    fn index_path_never_mixes_separators() {
        let abs = "C:/project/CTX-Audit/harness-private/EQM1-METHODOLOGY.md";
        for root in [
            r"C:\project\CTX-Audit\harness-private",
            "C:/project/CTX-Audit/harness-private",
            r"C:\project\CTX-Audit\harness-private/",
            "c:/project//CTX-Audit/harness-private",
        ] {
            assert_eq!(index_path_for(root, abs), abs, "绝对路径输入，扫描根 {root:?}");
        }
        for root in [r"C:\project\CTX-Audit", "C:/project/CTX-Audit"] {
            assert_eq!(
                index_path_for(root, "harness-private/EQM1-METHODOLOGY.md"),
                abs,
                "相对路径输入，扫描根 {root:?}"
            );
            assert_eq!(index_path_for(root, r"harness-private\EQM1-METHODOLOGY.md"), abs);
        }
        // 相对路径拼接后也必须是规范形式
    assert_eq!(index_path_for(r"C:\project\CTX-Audit\harness-private", "sub\\x.md"), "C:/project/CTX-Audit/harness-private/sub/x.md");
        assert_eq!(normalize_index_path(r"C:\Windows\System32"), "C:/Windows/System32");
        assert_eq!(normalize_index_path("c:/Windows//System32/"), "C:/Windows/System32");
        assert!(matches!(
            normalize_index_path("C:/Windows/System32"),
            std::borrow::Cow::Borrowed(_)
        ));
    }

    /// 回归：历史畸形索引路径能被还原（存量脏行里有一成没有对应的正确行，直接删会丢数据）。
    #[test]
    fn repair_historical_corrupt_path() {
        assert_eq!(
            repair_corrupt_path(r"C:\project/C:/project/CTX-Audit/harness-private/EQM1-METHODOLOGY.md")
                .as_deref(),
            Some("C:/project/CTX-Audit/harness-private/EQM1-METHODOLOGY.md")
        );
        assert_eq!(
            repair_corrupt_path(r"C:\Users\me/C:/Users/me/Desktop/flashdir.exe").as_deref(),
            Some("C:/Users/me/Desktop/flashdir.exe")
        );
        // 正常路径不该被误改
        assert_eq!(repair_corrupt_path("C:/Windows/System32/drivers/etc/hosts"), None);
        assert_eq!(repair_corrupt_path("C:/Users/me/Documents/a:b.txt"), None);
    }

    /// 构建锁：进程内重复获取必须失败；跨进程锁文件要区分"真的在构建"与"残留"。
    #[test]
    fn build_lock_guards_against_concurrent_builds() {
        let idx = empty_instance_for_test();
        let lock = std::env::temp_dir().join("flashdir-build-lock-test.lock");
        let _ = std::fs::remove_file(&lock);
        std::env::set_var("FLASHDIR_BUILD_LOCK", &lock);

        // 1) 进程内互斥：构建未完成时再次获取必须失败
        let guard = idx.try_begin_build().expect("首次获取构建权应成功");
        assert!(idx.is_building());
        assert!(idx.try_begin_build().is_err(), "构建未完成时不应再次获取");
        drop(guard);
        assert!(!idx.is_building(), "释放后标志应复位");
        assert!(!lock.exists(), "释放时应删除锁文件");

        // 2) 跨进程：锁文件里的 pid 仍存活 → 拒绝
        std::fs::write(&lock, format!("{}", std::process::id())).unwrap();
        let err = idx.try_begin_build().expect_err("别的进程持锁时必须拒绝");
        assert!(err.contains("正在构建索引"), "实际: {err}");
        assert!(!idx.is_building(), "被拒绝时不应留下本进程的构建标志");

        // 3) 跨进程：pid 已不存在（残留锁）→ 接管
        std::fs::write(&lock, "4294967294").unwrap();
        let guard = idx.try_begin_build().expect("残留锁应被接管");
        drop(guard);
        assert!(!lock.exists(), "接管后释放应删除锁文件");
        std::env::remove_var("FLASHDIR_BUILD_LOCK");
    }
    #[test]
    fn usn_path_helpers_and_report() {
        assert_eq!(full_index_path('C', "Users/me/x.txt"), "C:/Users/me/x.txt");
        assert_eq!(full_index_path('D', ""), "D:/");
        assert_eq!(join_child("C:/a", "b"), "C:/a/b");
        assert_eq!(join_child("C:/", "b"), "C:/b");
        assert_eq!(join_child("", "b"), "b");

        let report = UsnSyncReport {
            applied: 3,
            upserts: 2,
            removals: 1,
            elapsed_ms: 12,
            ..Default::default()
        };
        let text = report.summary();
        assert!(text.contains("应用 3 条"), "实际: {text}");
        assert!(text.contains("新增/更新 2"), "实际: {text}");
        assert!(UsnSyncReport::default().summary().contains("已是最新"));
    }

    /// 检查点随元数据持久化：重启后能从上次位置继续增量同步
    #[test]
    fn usn_state_round_trips_in_meta() {
        let mut meta = IndexMeta::default();
        meta.usn.checkpoints.insert(
            String::from("C"),
            crate::fs::UsnCheckpoint {
                volume_serial: 42,
                journal_id: 7,
                next_usn: 123_456,
                created_at: 1_700_000_000,
            },
        );
        meta.usn.stale_drives.push(String::from("D"));
        meta.usn.last_sync_at = 1_700_000_111;

        let json = serde_json::to_string(&meta).unwrap();
        let back: IndexMeta = serde_json::from_str(&json).unwrap();
        assert_eq!(
            back.usn.checkpoints.get("C").map(|c| c.next_usn),
            Some(123_456)
        );
        assert_eq!(back.usn.stale_drives, vec![String::from("D")]);
        assert_eq!(back.usn.last_sync_at, 1_700_000_111);
        assert_eq!(back.usn.applied_total, 0);
    }

    /// 诊断：过滤表达式解析与匹配（本地过滤 / 在此目录内过滤共用）
    #[test]
    fn diag_filter_parse_and_match() {
        for expr in ["windows", "dir:Windows", "ext:exe", "size:>100MB", "!tmp", "type:dir", "*.pdf", "report*.pdf", "name:abc"] {
            let f = parse_search_filter(expr);
            eprintln!("[diag] 解析 {:>14} → {} 个过滤器 {:?}", expr, f.len(), f);
        }
        let items = [
            ("report.pdf", "C:/docs/report.pdf", 1024i64, false, 0i64),
            ("tmp.txt", "C:/tmp/tmp.txt", 10, false, 0),
            ("docs", "C:/docs", 0, true, 0),
        ];
        for expr in ["report", "!tmp", "dir:docs", "type:dir", "ext:pdf", "size:>100B"] {
            let f = parse_search_filter(expr);
            let hits: Vec<&str> = items
                .iter()
                .filter(|(n, p, sz, d, m)| item_matches_filters(n, p, *sz, *d, *m, &f))
                .map(|(_, p, ..)| *p)
                .collect();
            eprintln!("[diag] 匹配 {:>12} → {:?}", expr, hits);
        }
    }

    use super::*;

    #[test]
    fn test_parse_search_filter_text_and_ext() {
        let filters = parse_search_filter("report ext:pdf");
        assert_eq!(filters.len(), 2);
        assert!(matches!(&filters[0].kind, SearchFilterKind::Text(t) if t == "report"));
        assert!(matches!(&filters[1].kind, SearchFilterKind::Ext(e) if e == "pdf"));
        assert!(!filters[0].negate);
    }

    #[test]
    fn test_parse_search_filter_wildcard() {
        let filters = parse_search_filter("*.pdf");
        assert_eq!(filters.len(), 1);
        assert!(matches!(&filters[0].kind, SearchFilterKind::Ext(e) if e == "pdf"));

        let filters = parse_search_filter(".pdf");
        assert_eq!(filters.len(), 1);
        assert!(matches!(&filters[0].kind, SearchFilterKind::Ext(e) if e == "pdf"));

        let filters = parse_search_filter("report*");
        assert_eq!(filters.len(), 1);
        assert!(matches!(&filters[0].kind, SearchFilterKind::Prefix(p) if p == "report"));

        let filters = parse_search_filter("*2024");
        assert_eq!(filters.len(), 1);
        assert!(matches!(&filters[0].kind, SearchFilterKind::Suffix(s) if s == "2024"));

        let filters = parse_search_filter("NOT *.tmp");
        assert_eq!(filters.len(), 1);
        assert!(matches!(&filters[0].kind, SearchFilterKind::Ext(e) if e == "tmp" && filters[0].negate));
    }

    #[test]
    fn test_parse_search_filter_size() {
        let filters = parse_search_filter("size:>100MB");
        assert_eq!(filters.len(), 1);
        assert!(
            matches!(&filters[0].kind, SearchFilterKind::Size { op, bytes } if matches!(op, FilterOp::Gt) && *bytes == 100 * 1024 * 1024)
        );
    }

    #[test]
    fn test_parse_search_filter_mtime_and_dir() {
        let filters = parse_search_filter("dir:\"Program Files\" mtime:>7d");
        assert_eq!(filters.len(), 2);
        assert!(matches!(&filters[0].kind, SearchFilterKind::Dir(d) if d == "program files"));
        assert!(
            matches!(&filters[1].kind, SearchFilterKind::Mtime { op, seconds } if matches!(op, FilterOp::Gt) && *seconds == 7 * 24 * 60 * 60)
        );
    }

    #[test]
    fn test_bang_prefix_negation() {
        // `!tmp` 必须解析为"否定 Text(tmp)"，而不是字面量 "!tmp"
        let f = parse_search_filter("!tmp");
        assert_eq!(f.len(), 1);
        assert!(f[0].negate);
        assert!(matches!(&f[0].kind, SearchFilterKind::Text(t) if t == "tmp"));

        // 与 NOT 等价
        let g = parse_search_filter("NOT tmp");
        assert_eq!(g.len(), 1);
        assert!(g[0].negate);

        // 组合：ext:zip !tmp
        let h = parse_search_filter("ext:zip !tmp");
        assert_eq!(h.len(), 2);
        assert!(!h[0].negate);
        assert!(h[1].negate);
        assert!(item_matches_filters("a.zip", "C:/x/a.zip", 10, false, 0, &h));
        assert!(!item_matches_filters("a.tmp", "C:/x/a.tmp", 10, false, 0, &h));

        // 单独的 `!` 不做否定（保持字面量，避免误吞）
        let k = parse_search_filter("!");
        assert_eq!(k.len(), 1);
        assert!(!k[0].negate);
    }

    #[test]
    fn test_parse_search_filter_negate() {
        let filters = parse_search_filter("NOT .tmp");
        assert_eq!(filters.len(), 1);
        assert!(matches!(&filters[0].kind, SearchFilterKind::Ext(e) if e == "tmp"));
        assert!(filters[0].negate);
    }

    #[test]
    fn test_apply_filters() {
        let entry = IndexEntry {
            path: "C:/docs/report.pdf".to_string(),
            name_lower: "report.pdf".to_string(),
            size: 1024 * 1024,
            is_dir: false,
            mtime: 0,
        };
        let filters = vec![
            SearchFilter {
                kind: SearchFilterKind::Text("report".to_string()),
                negate: false,
            },
            SearchFilter {
                kind: SearchFilterKind::Ext("pdf".to_string()),
                negate: false,
            },
        ];
        assert!(apply_filters(&entry, &filters));

        let neg = vec![SearchFilter {
            kind: SearchFilterKind::Ext("zip".to_string()),
            negate: true,
        }];
        assert!(apply_filters(&entry, &neg));
    }

    #[test]
    fn test_apply_filters_wildcard() {
        let entry = IndexEntry {
            path: "C:/docs/report_2024.pdf".to_string(),
            name_lower: "report_2024.pdf".to_string(),
            size: 1024,
            is_dir: false,
            mtime: 0,
        };

        assert!(apply_filters(
            &entry,
            &[SearchFilter {
                kind: SearchFilterKind::Ext("pdf".to_string()),
                negate: false,
            }]
        ));
        assert!(!apply_filters(
            &entry,
            &[SearchFilter {
                kind: SearchFilterKind::Ext("zip".to_string()),
                negate: false,
            }]
        ));
        assert!(apply_filters(
            &entry,
            &[SearchFilter {
                kind: SearchFilterKind::Prefix("report".to_string()),
                negate: false,
            }]
        ));
        assert!(apply_filters(
            &entry,
            &[SearchFilter {
                kind: SearchFilterKind::Suffix("2024.pdf".to_string()),
                negate: false,
            }]
        ));
        assert!(!apply_filters(
            &entry,
            &[SearchFilter {
                kind: SearchFilterKind::Suffix("2023.pdf".to_string()),
                negate: false,
            }]
        ));

        // NOT *.tmp 应排除该文件
        assert!(apply_filters(
            &entry,
            &[SearchFilter {
                kind: SearchFilterKind::Ext("tmp".to_string()),
                negate: true,
            }]
        ));
    }

    /// 分页 + 命中总数：前端"查看更多"依赖这两个能力
    #[test]
    fn test_search_paging_and_total_count() {
        let idx = empty_instance_for_test();
        for i in 0..25 {
            let name = format!("report_{:02}.pdf", i);
            idx.upsert(IndexEntry {
                path: format!("C:/docs/{}", name),
                name_lower: name.to_lowercase(),
                size: 1024 + i as i64,
                is_dir: false,
                mtime: 0,
            });
        }
        for i in 0..5 {
            let name = format!("other_{}.txt", i);
            idx.upsert(IndexEntry {
                path: format!("C:/docs/{}", name),
                name_lower: name.to_lowercase(),
                size: 10,
                is_dir: false,
                mtime: 0,
            });
        }

        let (page1, total1) = idx.search_with_filter_paged("report", 10, 0);
        assert_eq!(total1, 25, "命中总数应为 25（与 limit 无关）");
        assert_eq!(page1.len(), 10);

        let (page2, total2) = idx.search_with_filter_paged("report", 10, 10);
        assert_eq!(total2, 25);
        assert_eq!(page2.len(), 10);

        let (page3, _) = idx.search_with_filter_paged("report", 10, 20);
        assert_eq!(page3.len(), 5, "最后一页只剩 5 条");

        // 页间不重复
        let p1: Vec<&str> = page1.iter().map(|e| e.path.as_str()).collect();
        let p2: Vec<&str> = page2.iter().map(|e| e.path.as_str()).collect();
        assert!(p1.iter().all(|p| !p2.contains(p)), "分页结果不应重叠");

        // 越界返回空
        let (empty, total4) = idx.search_with_filter_paged("report", 10, 100);
        assert!(empty.is_empty());
        assert_eq!(total4, 25);

        // 单页便捷接口仍可用
        assert_eq!(idx.search_with_filter("report", 10).len(), 10);
    }

    #[test]
    fn test_search_with_filter_suffix_syntax() {
        let idx = empty_instance_for_test();
        for name in ["report_2024.pdf", "report_2023.pdf", "notes.txt", "archive.zip"] {
            idx.upsert(IndexEntry {
                path: format!("C:/docs/{}", name),
                name_lower: name.to_string(),
                size: 1024,
                is_dir: false,
                mtime: 0,
            });
        }

        let r = idx.search_with_filter("*.pdf", 10);
        assert_eq!(r.len(), 2);
        assert!(r.iter().all(|e| e.path.ends_with(".pdf")));

        let r = idx.search_with_filter("*2024.pdf", 10);
        assert_eq!(r.len(), 1);
        assert!(r[0].path.ends_with("report_2024.pdf"));

        let r = idx.search_with_filter("report*", 10);
        assert_eq!(r.len(), 2);
        assert!(r.iter().all(|e| e.name_lower.starts_with("report")));

        let r = idx.search_with_filter("NOT *.pdf", 10);
        assert_eq!(r.len(), 2);
        assert!(r.iter().all(|e| !e.path.ends_with(".pdf")));
    }

    #[test]
    fn test_global_index_upsert_and_search() {
        let idx = empty_instance_for_test();
        idx.upsert(IndexEntry {
            path: "C:/a.txt".to_string(),
            name_lower: "a.txt".to_string(),
            size: 100,
            is_dir: false,
            mtime: 0,
        });
        idx.upsert(IndexEntry {
            path: "C:/ab.txt".to_string(),
            name_lower: "ab.txt".to_string(),
            size: 200,
            is_dir: false,
            mtime: 0,
        });
        let r = idx.search_with_filter("a", 10);
        assert_eq!(r.len(), 2);
        // ab.txt 是前缀匹配，相关性高于 a.txt 的包含匹配
        assert!(r[0].path.ends_with("ab.txt"));

        idx.remove_by_path("C:/a.txt");
        let r2 = idx.search_with_filter("a", 10);
        assert_eq!(r2.len(), 1);
    }

    /// 真实数据基准：从 ~/.flashdir/cache_v2.db 加载持久化索引（百万级），
    /// 测量索引构建与各类查询语法下的延迟。
    /// 运行：cargo test --release --lib -- --ignored --nocapture bench_real_global_search
    #[test]
    #[ignore = "benchmark: 依赖本机持久化索引"]
    fn bench_real_global_search() {
        use std::time::Instant;

        let entries = match crate::disk_cache::DiskCache::instance().load_global_index() {
            Ok(e) if !e.is_empty() => e,
            other => {
                eprintln!("[bench] 无法加载持久化索引: {:?}", other.map(|e| e.len()));
                return;
            }
        };
        eprintln!("[bench] SQLite 读出索引条目: {}", entries.len());

        let idx = GlobalIndex::new();
        let t = Instant::now();
        idx.upsert_batch_internal(entries);
        eprintln!("[bench] 构建内存索引(HashMap+首字符桶): {:?}", t.elapsed());

        let queries = [
            "report",       // 长文本 → 首字符桶
            "a",            // 短文本(<=2) → 全量并行扫描
            "ab",
            "*.pdf",        // 纯 filter → 全量扫描
            "size:>1GB",    // 纯 filter
            "NOT *.tmp",    // 纯否定 → 全量
            "node_modules",
            "ext:zip size:>10MB",
            "dir:windows",
            "readme.md",
        ];
        for _round in 0..2 {
            for q in queries {
                let t = Instant::now();
                let r = idx.search_with_filter(q, 500);
                eprintln!(
                    "[bench] 查询 {:<22} 命中 {:>6}  耗时 {:?}",
                    q,
                    r.len(),
                    t.elapsed()
                );
            }
        }
    }

    /// 契约：响应是对象且 results 为数组、字段为 camelCase（前端据此渲染列表）
    #[test]
    fn contract_global_search_response_shape() {
        let resp = GlobalSearchResponse {
            ready: true,
            state: IndexState::NotLoaded,
            results: Vec::new(),
            total: 42,
            truncated: false,
            index_size: Some(42),
            sample_names: Some(vec!["a.txt".to_string()]),
        };
        let json = serde_json::to_value(&resp).expect("序列化失败");
        assert_eq!(json["ready"], serde_json::json!(true));
        assert!(json["results"].is_array(), "results 必须是数组");
        assert_eq!(json["total"], serde_json::json!(42), "命中总数必须返回");
        assert_eq!(json["truncated"], serde_json::json!(false));
        assert_eq!(json["indexSize"], serde_json::json!(42));
        assert!(json["sampleNames"].is_array());
    }

    #[test]
    fn test_is_same_or_child() {
        assert!(is_same_or_child("C:/foo", "C:/foo"));
        assert!(is_same_or_child("C:/foo", "C:/foo/bar"));
        assert!(is_same_or_child("C:/foo", "c:/FOO/BAR"));
        assert!(!is_same_or_child("C:/foo", "C:/foobar"));
        assert!(!is_same_or_child("C:/foo", "C:/bar"));
        assert!(is_same_or_child("C:/", "C:/Windows"));
        assert!(is_same_or_child("C:/", "C:/"));
    }
}

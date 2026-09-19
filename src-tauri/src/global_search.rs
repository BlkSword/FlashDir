// 全局文件搜索索引管理器
//
// 复用 fs::try_mft_scan 扫描所有 NTFS 卷，构建常驻内存索引，
// 支持按文件名毫秒级跨盘搜索（Everything 式）。索引构建一次后常驻，
// 后续搜索仅为内存过滤；刷新通过 global_search_ensure_index / refresh 全量重建。

use std::collections::{BinaryHeap, HashMap};
use std::sync::mpsc;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};
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
        }
    }


    /// 从 SQLite 磁盘缓存异步恢复持久化索引。
    /// 应在后台任务中调用，避免阻塞启动路径。
    pub fn load_persisted(&self) {
        if !matches!(self.state(), IndexState::NotLoaded) {
            return;
        }
        self.set_loading();
        match crate::disk_cache::DiskCache::instance().load_global_index() {
            Ok(entries) if !entries.is_empty() => {
                eprintln!("[GlobalIndex] 从磁盘恢复 {} 条索引", entries.len());
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
        let path_base = scan_path.trim_end_matches('/').trim_end_matches('\\');
        let path_base_lower = path_base.to_lowercase();

        // 先移除该路径下已有的条目，避免重复（内存 + 磁盘同步移除）
        let prefix = format!("{}/", path_base);
        self.remove_prefix_internal(&prefix);
        let _ = crate::disk_cache::DiskCache::instance().remove_global_index_by_prefix(&prefix);

        let batch: Vec<IndexEntry> = items
            .iter()
            .map(|item| {
                let abs_path = if item.path.as_str().to_lowercase().starts_with(&path_base_lower)
                    || item.path.starts_with('/')
                {
                    item.path.to_string()
                } else {
                    format!("{}/{}", path_base, item.path.as_str())
                };
                IndexEntry {
                    path: abs_path,
                    name_lower: item.name.to_lowercase(),
                    size: item.size,
                    is_dir: item.is_dir,
                    mtime: item.mtime,
                }
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

    /// 支持 Everything 式过滤语法与相关性排序的搜索。
    /// 过滤语法：ext:zip size:>100MB type:file dir:xxx name:xxx mtime:>7d NOT .tmp
    pub fn search_with_filter(&self, query: &str, limit: usize) -> Vec<IndexEntry> {
        if limit == 0 {
            return Vec::new();
        }
        let filters = parse_search_filter(query);
        // 过滤条件为空（例如输入只有 AND/OR 或未识别的空 token）：
        // 直接返回空结果，绝不能退化成"全量按大小返回前 N 条"。
        if filters.is_empty() {
            return Vec::new();
        }

        // 只把"正向文本条件"用于首字符分桶。
        // 早期实现会取到 `NOT foo` 里的 foo，导致结果被限制在 f 桶内。
        let text = filters.iter().find_map(|f| match &f.kind {
            SearchFilterKind::Text(t) if !f.negate => Some(t.as_str()),
            _ => None,
        });
        let q_lower = text.map(|t| t.to_lowercase()).unwrap_or_default();
        let has_text = !q_lower.is_empty();

        let entries = self.entries.read();

        let matches = |e: &IndexEntry| -> bool {
            (!has_text || e.name_lower.contains(&q_lower)) && apply_filters(e, &filters)
        };

        // 每线程维护一个大小为 limit 的 top-K 堆，最后归并；
        // 只会 clone 最终 ≤limit 条，而不是克隆全部命中。
        let results = if !has_text || q_lower.chars().count() <= 2 {
            // 无文本条件（*.pdf / size:>1GB 等）或短查询：全量并行过滤
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
                .into_entries()
        } else if let Some(first_char) = q_lower.chars().next() {
            // 长文本：只扫首字符桶。
            // 注意这里收集的是 &String 引用（几百 KB），不再 clone 每个候选路径。
            let name_index = self.name_index.read();
            let results = match name_index.get(&first_char) {
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
                    .reduce(|| TopK::new(limit), TopK::merge)
                    .into_entries(),
                None => Vec::new(),
            };
            drop(name_index);
            results
        } else {
            Vec::new()
        };

        drop(entries);
        results
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

        if let Some((key, value)) = word.split_once(':') {
            let key = key.to_lowercase();
            let negate = negate_next;
            negate_next = false;
            let kind = match key.as_str() {
                "ext" => Some(SearchFilterKind::Ext(value.to_lowercase())),
                "name" => Some(SearchFilterKind::Name(value.to_lowercase())),
                "dir" => Some(SearchFilterKind::Dir(value.to_lowercase())),
                "type" => {
                    let v = value.to_lowercase();
                    let is_dir = v == "dir" || v == "folder";
                    Some(SearchFilterKind::Type { is_dir })
                }
                "size" => parse_size(value).map(|(op, bytes)| SearchFilterKind::Size { op, bytes }),
                "mtime" => parse_mtime(value).map(|(op, seconds)| SearchFilterKind::Mtime { op, seconds }),
                // 未识别的 `key:value` 不再被静默丢弃（丢弃会让过滤条件变空，
                // 进而退化成"返回全量中最大的若干项"），而是按纯文本处理。
                _ => Some(SearchFilterKind::Text(word.to_lowercase())),
            };
            if let Some(kind) = kind {
                filters.push(SearchFilter { kind, negate });
            }
        } else {
            let value = word.to_lowercase();
            let negate = negate_next;
            negate_next = false;
            if let Some(kind) = parse_wildcard_filter(&value) {
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
        }
    }

    fn push(&mut self, entry: &'a IndexEntry, score: i64) {
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

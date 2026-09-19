// 全局文件搜索索引管理器
//
// 复用 fs::try_mft_scan 扫描所有 NTFS 卷，构建常驻内存索引，
// 支持按文件名毫秒级跨盘搜索（Everything 式）。索引构建一次后常驻，
// 后续搜索仅为内存过滤；刷新通过 global_search_ensure_index / refresh 全量重建。

use std::collections::{HashMap, HashSet};
use std::sync::mpsc;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};
use parking_lot::RwLock;
use rayon::prelude::*;
use serde::Serialize;

/// 索引中的一项（绝对路径）
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexEntry {
    pub path: String,
    pub name: String,
    /// 小写文件名（搜索用，避免每次搜索对全量 name 做 to_lowercase）
    #[serde(skip)]
    pub name_lower: String,
    /// 缓存的小写扩展名（不含点），用于加速 `ext:` 过滤
    #[serde(skip)]
    pub ext: String,
    pub size: i64,
    pub is_dir: bool,
    /// 文件修改时间（Windows FILETIME 转换而来的 Unix 时间戳，目录为 0）
    pub mtime: i64,
}

/// 从文件名提取小写扩展名（不含点）；无扩展名返回空字符串。
fn extension_of(name: &str) -> String {
    name.rsplit_once('.')
        .map(|(_, ext)| ext.to_lowercase())
        .unwrap_or_default()
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
    /// 以绝对路径为 key 的条目存储，保证去重
    entries: RwLock<HashMap<String, IndexEntry>>,
    /// 文件名首字符分桶索引：char -> set of path keys
    name_index: RwLock<HashMap<char, HashSet<String>>>,
    state: RwLock<IndexState>,
    meta: RwLock<IndexMeta>,
    /// 增量维护的文件/目录计数，避免每次状态刷新都 O(n) 全量重数
    file_count: AtomicUsize,
    dir_count: AtomicUsize,
}

impl GlobalIndex {
    fn new() -> Self {
        GlobalIndex {
            entries: RwLock::new(HashMap::new()),
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

    /// 添加或替换一条索引。entries 与 name_index 在同一把锁临界区内更新，
    /// 保证并发 search() 不会观察到两半不一致的中间态。
    /// 锁顺序：永远先 entries 后 name_index，避免死锁。
    fn upsert_internal(&self, entry: IndexEntry) {
        let first_char = entry.name_lower.chars().next().unwrap_or('\0');

        let mut entries = self.entries.write();
        let mut name_index = self.name_index.write();

        let old_entry = entries.insert(entry.path.clone(), entry.clone());

        if let Some(old) = &old_entry {
            let old_char = old.name_lower.chars().next().unwrap_or('\0');
            if old_char != first_char {
                if let Some(set) = name_index.get_mut(&old_char) {
                    set.remove(&old.path);
                }
            }
        }
        name_index
            .entry(first_char)
            .or_insert_with(HashSet::new)
            .insert(entry.path.clone());

        // 增量维护计数
        match &old_entry {
            None => self.bump_count(entry.is_dir, 1),
            Some(old) if old.is_dir != entry.is_dir => {
                self.bump_count(old.is_dir, -1);
                self.bump_count(entry.is_dir, 1);
            }
            _ => {}
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

    /// 批量 upsert：整批在同一个锁临界区内完成，且按值消费避免逐条 clone。
    /// 建索引 / 追加扫描结果时使用（逐条 upsert_internal 意味着每条目两次写锁 + 一次克隆）。
    fn upsert_batch_internal(&self, batch: Vec<IndexEntry>) {
        // 锁顺序与 upsert_internal 一致：先 entries 后 name_index
        let mut entries = self.entries.write();
        let mut name_index = self.name_index.write();

        for entry in batch {
            let first_char = entry.name_lower.chars().next().unwrap_or('\0');
            let is_dir = entry.is_dir;
            let path = entry.path.clone();

            let old_entry = entries.insert(path.clone(), entry);

            if let Some(old) = &old_entry {
                let old_char = old.name_lower.chars().next().unwrap_or('\0');
                if old_char != first_char {
                    if let Some(set) = name_index.get_mut(&old_char) {
                        set.remove(&old.path);
                    }
                }
            }
            name_index
                .entry(first_char)
                .or_insert_with(HashSet::new)
                .insert(path);

            match &old_entry {
                None => self.bump_count(is_dir, 1),
                Some(old) if old.is_dir != is_dir => {
                    self.bump_count(old.is_dir, -1);
                    self.bump_count(is_dir, 1);
                }
                _ => {}
            }
        }
    }

    /// 移除指定路径的索引。
    fn remove_path_internal(&self, path: &str) {
        // 锁顺序与 upsert_internal 一致：先 entries 后 name_index
        let mut entries = self.entries.write();
        let mut name_index = self.name_index.write();

        if let Some(old) = entries.remove(path) {
            let old_char = old.name_lower.chars().next().unwrap_or('\0');
            if let Some(set) = name_index.get_mut(&old_char) {
                set.remove(path);
            }
            self.bump_count(old.is_dir, -1);
        }
    }

    /// 按前缀移除索引（用于 USN 增量失败时重建某路径，或移除某盘）。
    /// 只移除该路径本身及其子路径，避免 `C:/foo` 误伤 `C:/foobar`。
    fn remove_prefix_internal(&self, prefix: &str) {
        let paths_to_remove: Vec<String> = {
            let entries = self.entries.read();
            entries
                .keys()
                .filter(|k| is_same_or_child(prefix, k))
                .cloned()
                .collect()
        };
        for path in paths_to_remove {
            self.remove_path_internal(&path);
        }
    }

    /// 清空所有索引数据。
    fn clear_internal(&self) {
        self.entries.write().clear();
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
                let name = f.name.clone();
                IndexEntry {
                    path: normalize_abs_path(drive, &f.path),
                    name: name.clone(),
                    name_lower: name.to_lowercase(),
                    ext: extension_of(&name),
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
            .map(|item| {
                let name = item.name.to_string();
                IndexEntry {
                    path: normalize_abs_path(drive, item.path.as_str()),
                    name: name.clone(),
                    name_lower: name.to_lowercase(),
                    ext: extension_of(&name),
                    size: item.size,
                    is_dir: item.is_dir,
                    mtime: item.mtime,
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
            for entry in entries.values() {
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
                let name = item.name.to_string();
                IndexEntry {
                    path: abs_path,
                    name: name.clone(),
                    name_lower: name.to_lowercase(),
                    ext: extension_of(&name),
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
        {
            // 锁顺序与 upsert_internal 一致：先 entries 后 name_index
            let mut entries = self.entries.write();
            let mut name_index = self.name_index.write();
            for path in paths {
                if let Some(old) = entries.remove(path) {
                    let old_char = old.name_lower.chars().next().unwrap_or('\0');
                    if let Some(set) = name_index.get_mut(&old_char) {
                        set.remove(path.as_str());
                    }
                    self.bump_count(old.is_dir, -1);
                }
            }
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
        self.entries.read().values().take(n).map(|e| e.name.clone()).collect()
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

        let entries = self.entries.read();

        let mut candidates: Vec<IndexEntry> = if q_lower.is_empty() {
            // 无文本条件：全量并行过滤（例如 *.pdf / size:>1GB 等纯 filter 查询）
            let values: Vec<&IndexEntry> = entries.values().collect();
            values
                .par_iter()
                .filter_map(|e| {
                    if apply_filters(e, &filters) {
                        Some((*e).clone())
                    } else {
                        None
                    }
                })
                .collect()
        } else if q_lower.chars().count() <= 2 {
            let values: Vec<&IndexEntry> = entries.values().collect();
            values
                .par_iter()
                .filter_map(|e| {
                    if e.name_lower.contains(&q_lower) && apply_filters(e, &filters) {
                        Some((*e).clone())
                    } else {
                        None
                    }
                })
                .collect()
        } else {
            let name_index = self.name_index.read();
            let first_char = q_lower.chars().next().unwrap_or('\0');
            let candidate_keys: Vec<String> = name_index
                .get(&first_char)
                .map(|set| set.iter().cloned().collect())
                .unwrap_or_default();
            drop(name_index);

            candidate_keys
                .par_iter()
                .filter_map(|key| {
                    entries.get(key).and_then(|e| {
                        if e.name_lower.contains(&q_lower) && apply_filters(e, &filters) {
                            Some(e.clone())
                        } else {
                            None
                        }
                    })
                })
                .collect()
        };

        drop(entries);

        // 按相关性排序：完全匹配 > 前缀匹配 > 包含匹配，同级按大小降序；
        // 同分按名称/路径字典序兜底，保证同一查询多次搜索结果顺序稳定
        let cmp = |a: &IndexEntry, b: &IndexEntry| {
            let sa = relevance_score(a, &q_lower);
            let sb = relevance_score(b, &q_lower);
            sb.cmp(&sa)
                .then_with(|| a.name_lower.cmp(&b.name_lower))
                .then_with(|| a.path.cmp(&b.path))
        };

        // 候选远多于 limit 时先做 O(n) 部分选择，避免对百万级候选做全量排序
        if candidates.len() > limit {
            candidates.select_nth_unstable_by(limit, cmp);
            candidates.truncate(limit);
        }
        candidates.sort_unstable_by(cmp);

        candidates
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
            SearchFilterKind::Ext(e) => !entry.is_dir && entry.ext == *e,
            SearchFilterKind::Dir(d) => entry.path.to_lowercase().contains(d),
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
            name: "report.pdf".to_string(),
            name_lower: "report.pdf".to_string(),
            ext: "pdf".to_string(),
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
            name: "report_2024.pdf".to_string(),
            name_lower: "report_2024.pdf".to_string(),
            ext: "pdf".to_string(),
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
                name: name.to_string(),
                name_lower: name.to_string(),
                ext: name.rsplit_once('.').map(|(_, e)| e.to_lowercase()).unwrap_or_default(),
                size: 1024,
                is_dir: false,
                mtime: 0,
            });
        }

        let r = idx.search_with_filter("*.pdf", 10);
        assert_eq!(r.len(), 2);
        assert!(r.iter().all(|e| e.name.ends_with(".pdf")));

        let r = idx.search_with_filter("*2024.pdf", 10);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].name, "report_2024.pdf");

        let r = idx.search_with_filter("report*", 10);
        assert_eq!(r.len(), 2);
        assert!(r.iter().all(|e| e.name.starts_with("report")));

        let r = idx.search_with_filter("NOT *.pdf", 10);
        assert_eq!(r.len(), 2);
        assert!(r.iter().all(|e| !e.name.ends_with(".pdf")));
    }

    #[test]
    fn test_global_index_upsert_and_search() {
        let idx = empty_instance_for_test();
        idx.upsert(IndexEntry {
            path: "C:/a.txt".to_string(),
            name: "a.txt".to_string(),
            name_lower: "a.txt".to_string(),
            ext: "txt".to_string(),
            size: 100,
            is_dir: false,
            mtime: 0,
        });
        idx.upsert(IndexEntry {
            path: "C:/ab.txt".to_string(),
            name: "ab.txt".to_string(),
            name_lower: "ab.txt".to_string(),
            ext: "txt".to_string(),
            size: 200,
            is_dir: false,
            mtime: 0,
        });
        let r = idx.search_with_filter("a", 10);
        assert_eq!(r.len(), 2);
        // ab.txt 是前缀匹配，相关性高于 a.txt 的包含匹配
        assert_eq!(r[0].name, "ab.txt");

        idx.remove_by_path("C:/a.txt");
        let r2 = idx.search_with_filter("a", 10);
        assert_eq!(r2.len(), 1);
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

// 全局文件搜索索引管理器
//
// 复用 fs::try_mft_scan 扫描所有 NTFS 卷，构建常驻内存索引，
// 支持按文件名毫秒级跨盘搜索（Everything 式）。索引构建一次后常驻，
// 后续搜索仅为内存过滤；刷新走全量重建（复用 build_index）。

use std::collections::{HashMap, HashSet};
use parking_lot::RwLock;
use rayon::prelude::*;
use serde::Serialize;
use tauri::Emitter;

/// 索引中的一项（绝对路径）
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexEntry {
    pub path: String,
    pub name: String,
    /// 小写文件名（搜索用，避免每次搜索对全量 name 做 to_lowercase）
    #[serde(skip)]
    pub name_lower: String,
    pub size: i64,
    pub is_dir: bool,
    /// 文件修改时间（Windows FILETIME 转换而来的 Unix 时间戳，目录为 0）
    pub mtime: i64,
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

/// 流式进度事件载荷
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ProgressPayload {
    drive: String,
    scanned: usize,
    phase: String,
}

#[derive(Debug, Clone, Default)]
struct IndexMeta {
    drive_count: usize,
    failed_drives: Vec<String>,
    all_drives: Vec<String>,
}

pub struct GlobalIndex {
    /// 以绝对路径为 key 的条目存储，保证去重
    entries: RwLock<HashMap<String, IndexEntry>>,
    /// 文件名首字符分桶索引：char -> set of path keys
    name_index: RwLock<HashMap<char, HashSet<String>>>,
    state: RwLock<IndexState>,
    meta: RwLock<IndexMeta>,
}

impl GlobalIndex {
    fn new() -> Self {
        let index = GlobalIndex {
            entries: RwLock::new(HashMap::new()),
            name_index: RwLock::new(HashMap::new()),
            state: RwLock::new(IndexState::NotLoaded),
            meta: RwLock::new(IndexMeta::default()),
        };

        // 尝试从 SQLite 磁盘缓存恢复持久化索引，实现“启动即用”
        if let Ok(entries) = crate::disk_cache::DiskCache::instance().load_global_index() {
            if !entries.is_empty() {
                eprintln!("[GlobalIndex] 从磁盘恢复 {} 条索引", entries.len());
                for entry in entries {
                    index.upsert_internal(entry);
                }
                index.update_ready_state();
            }
        }

        index
    }

    pub fn state(&self) -> IndexState {
        self.state.read().clone()
    }

    fn update_ready_state(&self) {
        let entries = self.entries.read();
        let fc = entries.values().filter(|e| !e.is_dir).count();
        let dc = entries.len() - fc;
        drop(entries);

        let meta = self.meta.read();
        *self.state.write() = IndexState::Ready(ReadyData {
            file_count: fc,
            dir_count: dc,
            drive_count: meta.drive_count,
            failed_drives: meta.failed_drives.clone(),
            all_drives: meta.all_drives.clone(),
        });
    }

    /// 添加或替换一条索引。内部统一维护 entries 与 name_index。
    /// 注意：绝不在持有 name_index 锁时去获取 entries 锁，避免死锁。
    fn upsert_internal(&self, entry: IndexEntry) {
        let first_char = entry.name_lower.chars().next().unwrap_or('\0');

        // 先更新 entries，返回旧条目
        let old_entry = {
            let mut entries = self.entries.write();
            entries.insert(entry.path.clone(), entry.clone())
        };

        // 再更新 name_index
        let mut name_index = self.name_index.write();
        if let Some(old) = old_entry {
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
    }

    /// 移除指定路径的索引。
    fn remove_path_internal(&self, path: &str) {
        let old = {
            let mut entries = self.entries.write();
            entries.remove(path)
        };
        if let Some(old) = old {
            let old_char = old.name_lower.chars().next().unwrap_or('\0');
            let mut name_index = self.name_index.write();
            if let Some(set) = name_index.get_mut(&old_char) {
                set.remove(path);
            }
        }
    }

    /// 按前缀移除索引（用于 USN 增量失败时重建某路径，或移除某盘）。
    fn remove_prefix_internal(&self, prefix: &str) {
        let prefix_lower = prefix.to_lowercase();
        let paths_to_remove: Vec<String> = {
            let entries = self.entries.read();
            entries
                .keys()
                .filter(|k| k.to_lowercase().starts_with(&prefix_lower))
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
    }

    /// 按文件名搜索（大小写不敏感，包含匹配），取前 limit 条。
    pub fn search(&self, query: &str, limit: usize) -> Vec<IndexEntry> {
        let q = query.trim();
        if q.is_empty() || limit == 0 {
            return Vec::new();
        }
        let q_lower = q.to_lowercase();

        let entries = self.entries.read();

        // 短查询（<=2 字符）分桶效果差，直接用全量并行扫描
        let results: Vec<IndexEntry> = if q_lower.chars().count() <= 2 {
            let values: Vec<&IndexEntry> = entries.values().collect();
            values
                .par_iter()
                .filter_map(|e| {
                    if e.name_lower.contains(&q_lower) {
                        Some((*e).clone())
                    } else {
                        None
                    }
                })
                .take_any(limit)
                .collect()
        } else {
            // 按首字符分桶，仅扫描候选桶
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
                        if e.name_lower.contains(&q_lower) {
                            Some(e.clone())
                        } else {
                            None
                        }
                    })
                })
                .take_any(limit)
                .collect()
        };

        results
    }

    /// 准备开始构建索引（设置 Loading 状态）
    pub fn set_loading(&self) {
        *self.state.write() = IndexState::Loading { drive: String::new(), scanned: 0 };
        self.clear_internal();
        *self.meta.write() = IndexMeta::default();
    }

    /// 批量追加 MFT 全卷扫描结果（轻量路径，跳过聚合/format/排序）
    pub fn extend_entries(&self, drive: char, mft_files: &[crate::fs::MftFileInfo]) {
        for f in mft_files {
            let name = f.name.clone();
            let path = normalize_abs_path(drive, &f.path);
            self.upsert_internal(IndexEntry {
                path,
                name: name.clone(),
                name_lower: name.to_lowercase(),
                size: f.size as i64,
                is_dir: f.is_dir,
                mtime: 0,
            });
        }
    }

    /// 逐盘追加 scan_directory 结果（回退路径，已含完整字段）
    pub fn append_scan(&self, drive: char, items: &[crate::scan::Item]) {
        for item in items {
            let name = item.name.to_string();
            self.upsert_internal(IndexEntry {
                path: normalize_abs_path(drive, item.path.as_str()),
                name: name.clone(),
                name_lower: name.to_lowercase(),
                size: item.size,
                is_dir: item.is_dir,
                mtime: 0,
            });
        }

        let total = self.entries.read().len();
        *self.state.write() = IndexState::Loading {
            drive: drive.to_string(),
            scanned: total,
        };
    }

    /// 所有盘扫描完毕后标记为就绪，并后台持久化到 SQLite
    pub fn finish_building(&self, drives: &[char]) {
        let mut meta = self.meta.write();
        meta.drive_count = drives.len();
        meta.all_drives = drives.iter().map(|c| c.to_string()).collect();
        drop(meta);
        self.update_ready_state();

        // 后台线程把全量索引持久化到 SQLite，不阻塞状态返回
        let entries: Vec<IndexEntry> = self.entries.read().values().cloned().collect();
        std::thread::spawn(move || {
            match crate::disk_cache::DiskCache::instance().save_global_index_batch(&entries) {
                Ok(_) => eprintln!("[GlobalIndex] 已持久化 {} 条索引", entries.len()),
                Err(e) => eprintln!("[GlobalIndex] 持久化失败: {}", e),
            }
        });
    }

    /// 将主界面某次扫描的结果追加到全局索引（复用已验证可用的 scan_dir 结果，
    /// 避免 MFT 在 build_index 的 spawn_blocking 上下文出现的 name 解析异常）。
    pub fn add_items(&self, scan_path: &str, items: &[crate::scan::Item]) {
        let path_base = scan_path.trim_end_matches('/').trim_end_matches('\\');
        let path_base_lower = path_base.to_lowercase();

        // 先移除该路径下已有的条目，避免重复
        self.remove_prefix_internal(&format!("{}/", path_base));

        for item in items {
            let abs_path = if item.path.as_str().to_lowercase().starts_with(&path_base_lower)
                || item.path.starts_with('/')
            {
                item.path.to_string()
            } else {
                format!("{}/{}", path_base, item.path.as_str())
            };
            let name = item.name.to_string();
            self.upsert_internal(IndexEntry {
                path: abs_path,
                name: name.clone(),
                name_lower: name.to_lowercase(),
                size: item.size,
                is_dir: item.is_dir,
                mtime: 0,
            });
        }

        // 保留已有的盘符元数据，仅更新计数
        self.update_ready_state();
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

    /// 按前缀移除条目（供 USN 增量同步或重建使用）
    pub fn remove_by_prefix(&self, prefix: &str) {
        self.remove_prefix_internal(prefix);
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

    /// 构建全盘索引（同步、耗时较长，调用者须在 spawn_blocking 中运行）
    pub fn build_index(&self, app: &tauri::AppHandle) {
        *self.state.write() = IndexState::Loading { drive: String::new(), scanned: 0 };
        self.clear_internal();

        let drives = list_ntfs_drives();
        if drives.is_empty() {
            *self.state.write() = IndexState::Failed {
                reason: "未检测到可扫描的 NTFS 卷（全局搜索需要管理员权限读取 MFT）".to_string(),
            };
            return;
        }

        let mut failed_drives = Vec::new();
        let mut ok_drives = Vec::new();
        let mut total_scanned = 0usize;

        for drive in &drives {
            *self.state.write() = IndexState::Loading {
                drive: drive.to_string(),
                scanned: total_scanned,
            };
            let _ = app.emit(
                "global-search-progress",
                ProgressPayload {
                    drive: drive.to_string(),
                    scanned: total_scanned,
                    phase: "scanning".to_string(),
                },
            );

            let root = format!("{}:\\", drive);
            match crate::fs::try_mft_scan(&root) {
                Some(result) => {
                    total_scanned += result.files.len() + result.dir_count;
                    self.extend_entries(*drive, &result.files);
                    ok_drives.push(*drive);
                }
                None => {
                    failed_drives.push(drive.to_string());
                    let _ = app.emit(
                        "global-search-progress",
                        ProgressPayload {
                            drive: drive.to_string(),
                            scanned: total_scanned,
                            phase: "skipped（需管理员或非 NTFS）".to_string(),
                        },
                    );
                }
            }
        }

        if ok_drives.is_empty() {
            *self.state.write() = IndexState::Failed {
                reason: "所有卷都无法扫描（需要管理员权限读取 MFT）".to_string(),
            };
            return;
        }

        {
            let mut meta = self.meta.write();
            meta.drive_count = ok_drives.len();
            meta.failed_drives = failed_drives;
            meta.all_drives = drives.iter().map(|c| c.to_string()).collect();
        }
        self.update_ready_state();

        let _ = app.emit(
            "global-search-progress",
            ProgressPayload {
                drive: String::new(),
                scanned: total_scanned,
                phase: "done".to_string(),
            },
        );
    }

    /// 支持 Everything 式过滤语法与相关性排序的搜索。
    /// 过滤语法：ext:zip size:>100MB type:file dir:xxx name:xxx mtime:>7d NOT .tmp
    pub fn search_with_filter(&self, query: &str, limit: usize) -> Vec<IndexEntry> {
        let filters = parse_search_filter(query);
        let text = filters.iter().find_map(|f| match &f.kind {
            SearchFilterKind::Text(t) => Some(t.as_str()),
            _ => None,
        });
        let q_lower = text.map(|t| t.to_lowercase()).unwrap_or_default();

        let entries = self.entries.read();

        let mut candidates: Vec<IndexEntry> = if q_lower.is_empty() {
            // 无文本条件：全量扫描（filter 仅命中少量结果时可能较慢，实际中少见）
            entries.values().cloned().collect()
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

        // 按相关性排序：完全匹配 > 前缀匹配 > 包含匹配，同级按大小降序
        candidates.sort_unstable_by(|a, b| {
            let sa = relevance_score(a, &q_lower);
            let sb = relevance_score(b, &q_lower);
            sb.cmp(&sa)
        });

        candidates.into_iter().take(limit).collect()
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
                _ => None,
            };
            if let Some(kind) = kind {
                filters.push(SearchFilter { kind, negate });
            }
        } else {
            let value = word.to_lowercase();
            if negate_next {
                filters.push(SearchFilter {
                    kind: SearchFilterKind::Text(value),
                    negate: true,
                });
                negate_next = false;
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
            SearchFilterKind::Ext(e) => {
                if entry.is_dir {
                    false
                } else {
                    entry
                        .name
                        .to_lowercase()
                        .rsplit_once('.')
                        .map(|(_, ext)| ext == e)
                        .unwrap_or(false)
                }
            }
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

lazy_static::lazy_static! {
    static ref GLOBAL_INDEX: GlobalIndex = GlobalIndex::new();
}

pub fn instance() -> &'static GlobalIndex {
    &GLOBAL_INDEX
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
        assert!(matches!(&filters[0].kind, SearchFilterKind::Text(t) if t == ".tmp"));
        assert!(filters[0].negate);
    }

    #[test]
    fn test_apply_filters() {
        let entry = IndexEntry {
            path: "C:/docs/report.pdf".to_string(),
            name: "report.pdf".to_string(),
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
    fn test_global_index_upsert_and_search() {
        let idx = GlobalIndex::new();
        idx.upsert(IndexEntry {
            path: "C:/a.txt".to_string(),
            name: "a.txt".to_string(),
            name_lower: "a.txt".to_string(),
            size: 100,
            is_dir: false,
            mtime: 0,
        });
        idx.upsert(IndexEntry {
            path: "C:/ab.txt".to_string(),
            name: "ab.txt".to_string(),
            name_lower: "ab.txt".to_string(),
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
}

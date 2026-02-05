use anyhow;
use crossbeam::queue::SegQueue;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::fs;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TimingInfo {
    pub scan_phase: f64,
    pub compute_phase: f64,
    pub format_phase: f64,
    pub total: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Item {
    pub path: String,
    pub name: String,
    pub size: i64,
    #[serde(rename = "sizeFormatted")]
    pub size_formatted: String,
    #[serde(rename = "isDir")]
    pub is_dir: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanResult {
    pub items: Vec<Item>,
    pub total_size: i64,
    pub total_size_formatted: String,
    pub scan_time: f64,
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timing: Option<TimingInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryItem {
    pub path: String,
    #[serde(with = "chrono::serde::ts_seconds")]
    pub scan_time: chrono::DateTime<chrono::Utc>,
    pub total_size: i64,
    pub size_format: String,
    pub items: Vec<Item>,
}

/// 轻量级历史记录摘要，用于前端列表展示
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryItemSummary {
    pub path: String,
    #[serde(with = "chrono::serde::ts_seconds")]
    pub scan_time: chrono::DateTime<chrono::Utc>,
    pub total_size: i64,
    pub size_format: String,
    pub item_count: usize,
}

impl From<&HistoryItem> for HistoryItemSummary {
    fn from(item: &HistoryItem) -> Self {
        Self {
            path: item.path.clone(),
            scan_time: item.scan_time,
            total_size: item.total_size,
            size_format: item.size_format.clone(),
            item_count: item.items.len(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct CacheEntry {
    pub result: ScanResult,
    pub dir_mtime: chrono::DateTime<chrono::Local>,
}

pub struct ScanCache {
    cache: dashmap::DashMap<String, CacheEntry>,
    max_entries: usize,
    max_size_bytes: usize,
    current_size: dashmap::DashMap<String, usize>,
}

impl ScanCache {
    pub fn new(max_entries: usize, max_size_mb: usize) -> Self {
        ScanCache {
            cache: dashmap::DashMap::new(),
            max_entries,
            max_size_bytes: max_size_mb * 1024 * 1024,
            current_size: dashmap::DashMap::new(),
        }
    }

    pub fn get(&self, path: &str) -> Option<CacheEntry> {
        self.cache.get(path).map(|entry| entry.clone())
    }

    pub fn insert(&self, path: String, result: ScanResult) {
        let entry_size = self.estimate_size(&result);

        if self.cache.len() >= self.max_entries
            || self.get_total_size() + entry_size > self.max_size_bytes
        {
            self.evict_oldest();
        }

        self.current_size.insert(path.clone(), entry_size);
        self.cache.insert(
            path,
            CacheEntry {
                result,
                dir_mtime: chrono::Local::now(),
            },
        );
    }

    fn estimate_size(&self, result: &ScanResult) -> usize {
        result.items.iter().map(|item| {
            std::mem::size_of::<Item>()
                + item.path.capacity()
                + item.name.capacity()
                + item.size_formatted.capacity()
        }).sum::<usize>()
    }

    fn get_total_size(&self) -> usize {
        self.current_size.iter().map(|entry| *entry.value()).sum()
    }

    fn evict_oldest(&self) {
        let mut entries: Vec<_> = self.cache.iter().collect();
        entries.sort_by(|a, b| a.value().dir_mtime.cmp(&b.value().dir_mtime));
        if let Some(entry) = entries.first() {
            let key = entry.key().clone();
            self.current_size.remove(&key);
            self.cache.remove(&key);
        }
    }

    pub fn invalidate(&self, path: &str) {
        let keys_to_remove: Vec<String> = self
            .cache
            .iter()
            .filter(|entry| entry.key().starts_with(path))
            .map(|entry| entry.key().clone())
            .collect();
        for key in keys_to_remove {
            self.current_size.remove(&key);
            self.cache.remove(&key);
        }
    }
}

lazy_static::lazy_static! {
    static ref SCAN_CACHE: ScanCache = ScanCache::new(30, 200);
}

pub fn format_size(bytes: i64) -> String {
    if bytes < 1024 {
        return format!("{} B", bytes);
    }
    let kb = bytes as f64 / 1024.0;
    if kb < 1024.0 {
        return format!("{:.1} KB", kb);
    }
    let mb = kb / 1024.0;
    if mb < 1024.0 {
        return format!("{:.1} MB", mb);
    }
    let gb = mb / 1024.0;
    format!("{:.1} GB", gb)
}

pub async fn scan_directory(path: &str, force_refresh: bool) -> Result<ScanResult, anyhow::Error> {
    let start_time = std::time::Instant::now();

    if path.trim().is_empty() {
        return Err(anyhow::anyhow!("路径不能为空"));
    }

    let path_buf = PathBuf::from(path);

    let metadata = match fs::metadata(&path_buf).await {
        Ok(m) => m,
        Err(e) => return Err(anyhow::anyhow!("无法访问路径: {}", e)),
    };

    if !metadata.is_dir() {
        return Err(anyhow::anyhow!("不是目录"));
    }

    let canonical_path = match fs::canonicalize(&path_buf).await {
        Ok(p) => p,
        Err(e) => return Err(anyhow::anyhow!("路径规范化失败: {}", e)),
    };

    let root_dir = canonical_path.to_string_lossy().replace('\\', "/");

    let mtime = match metadata.modified() {
        Ok(m) => m,
        Err(_) => std::time::SystemTime::UNIX_EPOCH,
    };
    let mtime_datetime: chrono::DateTime<chrono::Local> = mtime.into();

    if !force_refresh {
        if let Some(cached) = SCAN_CACHE.get(&root_dir) {
            if cached.dir_mtime >= mtime_datetime {
                let mut result = cached.result.clone();
                result.scan_time = 0.0;
                return Ok(result);
            }
        }
    }

    SCAN_CACHE.invalidate(&root_dir);

    let canonical_path_clone = canonical_path.clone();

    let output = tokio::task::spawn_blocking(move || {
        scan_directory_parallel_v3(&canonical_path_clone)
    })
    .await??;

    let scan_time = start_time.elapsed().as_secs_f64();

    let result = ScanResult {
        items: output.items,
        total_size: output.total_size,
        total_size_formatted: format_size(output.total_size),
        scan_time,
        path: path.to_string(),
        timing: Some(output.timing),
    };

    SCAN_CACHE.insert(root_dir.clone(), result.clone());

    Ok(result)
}

/// 扫描结果，包含详细时间信息
struct ScanOutput {
    items: Vec<Item>,
    total_size: i64,
    timing: TimingInfo,
}

fn scan_directory_parallel_v3(root_path: &Path) -> Result<ScanOutput, anyhow::Error> {
    use rayon::prelude::*;
    use std::fs;

    let total_start = std::time::Instant::now();

    let dirs_to_scan = Arc::new(SegQueue::new());
    let items = Arc::new(SegQueue::new());
    let file_entries = Arc::new(dashmap::DashMap::with_capacity(4096));

    dirs_to_scan.push(root_path.to_path_buf());

    let num_threads = rayon::current_num_threads().max(8);
    rayon::ThreadPoolBuilder::new()
        .num_threads(num_threads)
        .build_global()
        .ok();

    let scan_start = std::time::Instant::now();

    rayon::scope(|s| {
        for _ in 0..num_threads {
            let dirs_to_scan = Arc::clone(&dirs_to_scan);
            let items = Arc::clone(&items);
            let file_entries = Arc::clone(&file_entries);
            let root_path = root_path.to_path_buf();

            s.spawn(move |_| {
                loop {
                    let dir_path = match dirs_to_scan.pop() {
                        Some(d) => d,
                        None => break,
                    };

                    if let Ok(entries) = fs::read_dir(&dir_path) {
                        for entry in entries.filter_map(Result::ok) {
                            let entry_path = entry.path();

                            let ft = match entry.file_type() {
                                Ok(ft) => ft,
                                Err(_) => continue,
                            };

                            if ft.is_symlink() {
                                continue;
                            }

                            let is_dir = ft.is_dir();

                            let rel_path = match entry_path.strip_prefix(&root_path) {
                                Ok(p) => p.to_string_lossy().replace('\\', "/"),
                                Err(_) => continue,
                            };

                            let name = entry_path
                                .file_name()
                                .and_then(|n| n.to_str())
                                .unwrap_or("?")
                                .to_string();

                            let size = if is_dir {
                                0
                            } else {
                                entry.metadata()
                                    .map(|m| m.len() as i64)
                                    .unwrap_or(0)
                            };

                            if !is_dir {
                                file_entries.insert(rel_path.clone(), size);
                            } else {
                                dirs_to_scan.push(entry_path);
                            }

                            items.push(Item {
                                path: rel_path,
                                name,
                                size,
                                size_formatted: String::new(),
                                is_dir,
                            });
                        }
                    }
                }
            });
        }
    });

    let scan_phase = scan_start.elapsed().as_secs_f64();

    let compute_start = std::time::Instant::now();

    let mut items_vec: Vec<Item> = Vec::new();
    while let Some(item) = items.pop() {
        items_vec.push(item);
    }

    // 并行计算目录大小
    let file_entries_vec: Vec<(String, i64)> = file_entries
        .iter()
        .map(|entry| (entry.key().clone(), *entry.value()))
        .collect();

    let actual_total_size: i64 = file_entries_vec.iter().map(|(_, size)| *size).sum();

    let dir_sizes: HashMap<String, i64> = file_entries_vec
        .par_iter()
        .fold(
            || HashMap::with_capacity(16),
            |mut acc, (file_path, file_size)| {
                let mut pos = 0;
                while let Some(slash_pos) = &file_path[pos..].find('/') {
                    let abs_pos = pos + slash_pos;
                    let parent_path = &file_path[..abs_pos];
                    *acc.entry(parent_path.to_string()).or_insert(0) += file_size;
                    pos = abs_pos + 1;
                }
                *acc.entry(String::new()).or_insert(0) += file_size;
                acc
            },
        )
        .reduce(
            || HashMap::new(),
            |mut acc, other| {
                for (k, v) in other {
                    *acc.entry(k).or_insert(0) += v;
                }
                acc
            },
        );

    let compute_phase = compute_start.elapsed().as_secs_f64();

    let format_start = std::time::Instant::now();

    items_vec.par_iter_mut().for_each(|item| {
        if item.is_dir {
            item.size = dir_sizes.get(&item.path).copied().unwrap_or(0);
        }
        item.size_formatted = format_size(item.size);
    });

    items_vec.sort_unstable_by(|a, b| b.size.cmp(&a.size));

    let format_phase = format_start.elapsed().as_secs_f64();
    let total = total_start.elapsed().as_secs_f64();

    Ok(ScanOutput {
        items: items_vec,
        total_size: actual_total_size,
        timing: TimingInfo {
            scan_phase,
            compute_phase,
            format_phase,
            total,
        },
    })
}

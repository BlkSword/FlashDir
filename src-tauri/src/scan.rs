use anyhow;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use tokio::fs;

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

    let items = tokio::task::spawn_blocking(move || {
        scan_directory_fast(&canonical_path_clone)
    })
    .await??;

    let total_size: i64 = items.iter().map(|item| item.size).sum();
    let scan_time = start_time.elapsed().as_secs_f64();

    let result = ScanResult {
        items,
        total_size,
        total_size_formatted: format_size(total_size),
        scan_time,
        path: path.to_string(),
    };

    SCAN_CACHE.insert(root_dir.clone(), result.clone());

    Ok(result)
}

fn scan_directory_fast(root_path: &Path) -> Result<Vec<Item>, anyhow::Error> {
    use std::fs;

    let mut items = Vec::with_capacity(1024);
    let mut dirs_to_scan = VecDeque::with_capacity(256);
    dirs_to_scan.push_back(root_path.to_path_buf());

    while let Some(current_dir) = dirs_to_scan.pop_front() {
        let entries = match fs::read_dir(&current_dir) {
            Ok(e) => e,
            Err(_) => continue,
        };

        for entry in entries.flatten() {
            let path = entry.path();

            let file_type = match entry.file_type() {
                Ok(ft) => ft,
                Err(_) => continue,
            };

            let is_dir = file_type.is_dir();

            let size = if is_dir {
                0
            } else {
                match entry.metadata() {
                    Ok(m) => m.len() as i64,
                    Err(_) => 0,
                }
            };

            let rel_path = match path.strip_prefix(root_path) {
                Ok(p) => {
                    let path_str = p.to_string_lossy();
                    let estimated_len = path_str.len() + 10;
                    let mut result = String::with_capacity(estimated_len);
                    for (i, part) in p.components().enumerate() {
                        if i > 0 {
                            result.push('/');
                        }
                        result.push_str(part.as_os_str().to_string_lossy().as_ref());
                    }
                    result
                }
                Err(_) => path.to_string_lossy().replace('\\', "/"),
            };

            // 获取文件名
            let name = path.file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("?")
                .to_string();

            items.push(Item {
                path: rel_path,
                name,
                size,
                size_formatted: format_size(size),
                is_dir,
            });

            if is_dir {
                dirs_to_scan.push_back(path);
            }
        }
    }

    items.sort_unstable_by(|a, b| b.size.cmp(&a.size));

    let mut dir_sizes: std::collections::HashMap<String, i64> = std::collections::HashMap::new();

    for item in &items {
        if !item.is_dir {
            let mut current_path = item.path.as_str();
            loop {
                dir_sizes.entry(current_path.to_string())
                    .and_modify(|s| *s += item.size)
                    .or_insert(item.size);

                if let Some(pos) = current_path.rfind('/') {
                    current_path = &current_path[..pos];
                } else {
                    dir_sizes.entry(String::new())
                        .and_modify(|s| *s += item.size)
                        .or_insert(item.size);
                    break;
                }
            }
        }
    }

    for item in &mut items {
        if item.is_dir {
            if let Some(&total_size) = dir_sizes.get(&item.path) {
                item.size = total_size;
                item.size_formatted = format_size(total_size);
            }
        }
    }

    items.sort_unstable_by(|a, b| b.size.cmp(&a.size));

    Ok(items)
}

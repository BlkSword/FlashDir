use anyhow;
use serde::{Deserialize, Serialize};
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
        result.items.len() * (100 + std::mem::size_of::<Item>())
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

// 简化、可靠的扫描函数
fn scan_directory_fast(root_path: &Path) -> Result<Vec<Item>, anyhow::Error> {
    use std::fs;

    // 预分配容量以减少重新分配（根据目录大小预估）
    let mut items = Vec::with_capacity(1024);
    let mut dirs_to_scan = Vec::with_capacity(256);
    dirs_to_scan.push(root_path.to_path_buf());

    // 使用 BFS 遍历目录
    while let Some(current_dir) = dirs_to_scan.pop() {
        let entries = match fs::read_dir(&current_dir) {
            Ok(e) => e,
            Err(_) => continue,
        };

        for entry in entries.flatten() {
            let path = entry.path();
            let metadata = match fs::metadata(&path) {
                Ok(m) => m,
                Err(_) => continue,
            };

            let is_dir = metadata.is_dir();
            let size = metadata.len() as i64;

            // 计算相对路径
            let rel_path = path.strip_prefix(root_path)
                .unwrap_or(&path);
            let rel_path_str = rel_path.to_string_lossy().replace('\\', "/");

            // 获取名称（直接使用 &str 避免克隆）
            let name = path.file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("?")
                .to_string();

            items.push(Item {
                path: rel_path_str,
                name,
                size,
                size_formatted: format_size(size),
                is_dir,
            });

            // 如果是目录，添加到待扫描列表
            if is_dir {
                dirs_to_scan.push(path);
            }
        }
    }

    // 按大小排序（稳定的排序）
    items.sort_by(|a, b| b.size.cmp(&a.size));

    Ok(items)
}

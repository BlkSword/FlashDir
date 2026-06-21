// 扫描核心模块 - 优化版
// 集成：性能监控、磁盘缓存、bincode 序列化、Windows 原生 I/O

use anyhow;
use crossbeam::channel::{unbounded, Sender, Receiver};
use lru::LruCache;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use smartstring::SmartString;
use std::collections::HashMap;
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tauri::Emitter;
use tokio::fs;

use crate::perf::PerformanceMonitor;
use crate::disk_cache::DiskCache;
use std::sync::atomic::{AtomicBool, Ordering};

pub type CompactString = SmartString<smartstring::Compact>;

/// 测试/诊断开关：强制禁用 MFT 快速路径，回退到目录遍历。
static DISABLE_MFT: AtomicBool = AtomicBool::new(false);

pub fn set_disable_mft(disable: bool) {
    DISABLE_MFT.store(disable, Ordering::Relaxed);
}

fn is_mft_disabled() -> bool {
    DISABLE_MFT.load(Ordering::Relaxed)
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
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
    pub path: CompactString,
    pub name: CompactString,
    pub size: i64,
    #[serde(rename = "sizeFormatted")]
    pub size_formatted: CompactString,
    #[serde(rename = "isDir")]
    pub is_dir: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanResult {
    pub items: Vec<Item>,
    pub total_size: i64,
    pub total_size_formatted: CompactString,
    pub scan_time: f64,
    pub path: CompactString,
    pub mft_available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timing: Option<TimingInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub perf_metrics: Option<ScanPerfMetrics>,
}

/// 扫描性能指标
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanPerfMetrics {
    pub io_phase_ms: u64,
    pub compute_phase_ms: u64,
    pub serialize_phase_ms: u64,
    pub cache_read_time_ms: u64,
    pub files_scanned: usize,
    pub dirs_scanned: usize,
    pub io_throughput_mbps: f64,
    pub memory_peak_mb: f64,
    pub threads_used: usize,
    pub cache_hit: bool,
    pub cache_source: Option<String>, // "memory" | "disk" | None
}

#[derive(Debug, Clone)]
pub struct ArcScanResult {
    pub items: Arc<Vec<Item>>,
    pub total_size: i64,
    pub total_size_formatted: Arc<str>,
    pub scan_time: f64,
    pub path: Arc<str>,
    pub mft_available: bool,
    pub timing: Option<TimingInfo>,
}

impl From<ArcScanResult> for ScanResult {
    fn from(result: ArcScanResult) -> Self {
        Self {
            items: Arc::unwrap_or_clone(result.items),
            total_size: result.total_size,
            total_size_formatted: CompactString::from(result.total_size_formatted.as_ref()),
            scan_time: result.scan_time,
            path: CompactString::from(result.path.as_ref()),
            mft_available: result.mft_available,
            timing: result.timing,
            perf_metrics: None,
        }
    }
}

impl From<&ArcScanResult> for ScanResult {
    fn from(result: &ArcScanResult) -> Self {
        Self {
            items: result.items.as_ref().clone(),
            total_size: result.total_size,
            total_size_formatted: CompactString::from(result.total_size_formatted.as_ref()),
            scan_time: result.scan_time,
            path: CompactString::from(result.path.as_ref()),
            mft_available: result.mft_available,
            timing: result.timing.clone(),
            perf_metrics: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryItem {
    pub path: CompactString,
    #[serde(with = "chrono::serde::ts_seconds")]
    pub scan_time: chrono::DateTime<chrono::Utc>,
    pub total_size: i64,
    pub size_format: CompactString,
    pub item_count: usize,
}

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
            path: item.path.to_string(),
            scan_time: item.scan_time,
            total_size: item.total_size,
            size_format: item.size_format.to_string(),
            item_count: item.item_count,
        }
    }
}

#[derive(Debug, Clone)]
pub struct CacheEntry {
    pub result: ArcScanResult,
    pub dir_mtime: chrono::DateTime<chrono::Local>,
    pub size: usize,
}

pub struct ScanCache {
    cache: Mutex<LruCache<String, CacheEntry>>,
    max_size_bytes: usize,
}

impl ScanCache {
    pub fn new(max_entries: usize, max_size_mb: usize) -> Self {
        ScanCache {
            cache: Mutex::new(LruCache::new(NonZeroUsize::new(max_entries).unwrap())),
            max_size_bytes: max_size_mb * 1024 * 1024,
        }
    }

    pub fn get(&self, path: &str) -> Option<CacheEntry> {
        let mut cache = self.cache.lock();
        cache.get(path).cloned()
    }

    pub fn insert(&self, path: String, result: ScanResult) {
        let arc_result = ArcScanResult {
            items: Arc::new(result.items),
            total_size: result.total_size,
            total_size_formatted: Arc::from(result.total_size_formatted.as_str()),
            scan_time: result.scan_time,
            path: Arc::from(result.path.as_str()),
            mft_available: result.mft_available,
            timing: result.timing,
        };

        let entry_size = Self::estimate_size(&arc_result);
        let mut cache = self.cache.lock();

        let current_total: usize = cache.iter().map(|(_, e)| e.size).sum();
        if current_total + entry_size > self.max_size_bytes {
            while cache.iter().map(|(_, e)| e.size).sum::<usize>() + entry_size > self.max_size_bytes
                && !cache.is_empty()
            {
                cache.pop_lru();
            }
        }

        cache.put(
            path,
            CacheEntry {
                result: arc_result,
                dir_mtime: chrono::Local::now(),
                size: entry_size,
            },
        );
    }

    fn estimate_size(result: &ArcScanResult) -> usize {
        result.items.iter().map(|item| {
            std::mem::size_of::<Item>()
                + item.path.len()
                + item.name.len()
                + item.size_formatted.len()
        }).sum::<usize>()
            + std::mem::size_of::<Arc<Vec<Item>>>()
    }

    pub fn invalidate(&self, path: &str) {
        let mut cache = self.cache.lock();
        let keys_to_remove: Vec<String> = cache
            .iter()
            .filter(|(k, _)| k.starts_with(path))
            .map(|(k, _)| k.clone())
            .collect();
        for key in keys_to_remove {
            cache.pop(&key);
        }
    }
}

lazy_static::lazy_static! {
    static ref SCAN_CACHE: ScanCache = ScanCache::new(30, 200);
    static ref SIZE_UNITS: [&'static str; 5] = ["B", "KB", "MB", "GB", "TB"];
}

/// 将任意路径规范化为内存/磁盘缓存使用的 key（canonical + 正斜杠）
fn cache_key_for(path: &str) -> Option<String> {
    let canonical = std::fs::canonicalize(path).ok()?;
    Some(normalize_path_separator(canonical.as_os_str()))
}

/// 获取内存缓存中的扫描结果 items（供 dev_analyzer 等模块复用，
/// 避免把百万级 items 再次跨 IPC 传回后端）
pub fn get_cached_items(path: &str) -> Option<Arc<Vec<Item>>> {
    let key = cache_key_for(path)?;
    SCAN_CACHE.get(&key).map(|e| Arc::clone(&e.result.items))
}

/// 自定义紧凑二进制编码扫描结果，供前端经 Tauri 原始字节通道接收，
/// 避免 serde_json 序列化百万级 items 的开销（无 key 名/引号/转义，size 用定宽整数）。
/// 前端用 DataView + TextDecoder 顺序解析。布局（小端）:
///   u32 magic=0x4644 | u8 version | u8 flags
///   i64 total_size | f64 scan_time | u32 item_count | u32 file_count | u32 dir_count
///   f64 io_ms | f64 compute_ms | f64 serialize_ms
///   u32 path_len | path_utf8                      （被扫描路径）
///   逐项: u32 path_len|path_utf8 | u32 name_len|name_utf8 | i64 size | u8 is_dir
pub fn encode_scan_result(result: &ScanResult) -> Vec<u8> {
    let item_count = result.items.len();
    let (file_count, dir_count) = result.perf_metrics.as_ref().map(|m| (m.files_scanned, m.dirs_scanned)).unwrap_or_else(|| {
        let f = result.items.iter().filter(|i| !i.is_dir).count();
        (f, item_count.saturating_sub(f))
    });

    let path_str = result.path.as_str();
    let est = result.items.iter().map(|i| i.path.len() + i.name.len() + 4 + 4 + 8 + 1).sum::<usize>()
        + path_str.len() + 64;
    let mut buf = Vec::with_capacity(est);

    // header
    buf.extend_from_slice(&0x4644u32.to_le_bytes());
    buf.push(1u8); // version
    buf.push(0u8); // flags

    // metadata
    buf.extend_from_slice(&result.total_size.to_le_bytes());
    buf.extend_from_slice(&result.scan_time.to_le_bytes());
    buf.extend_from_slice(&(item_count as u32).to_le_bytes());
    buf.extend_from_slice(&(file_count as u32).to_le_bytes());
    buf.extend_from_slice(&(dir_count as u32).to_le_bytes());

    let m = result.perf_metrics.as_ref();
    buf.extend_from_slice(&(m.map(|m| m.io_phase_ms).unwrap_or(0) as f64).to_le_bytes());
    buf.extend_from_slice(&(m.map(|m| m.compute_phase_ms).unwrap_or(0) as f64).to_le_bytes());
    buf.extend_from_slice(&(m.map(|m| m.serialize_phase_ms).unwrap_or(0) as f64).to_le_bytes());

    // 被扫描路径
    write_bin_str(&mut buf, path_str);

    // items（不传 sizeFormatted，由前端 formatSize 计算）
    for item in &result.items {
        write_bin_str(&mut buf, item.path.as_str());
        write_bin_str(&mut buf, item.name.as_str());
        buf.extend_from_slice(&item.size.to_le_bytes());
        buf.push(if item.is_dir { 1u8 } else { 0u8 });
    }

    buf
}

#[inline]
fn write_bin_str(buf: &mut Vec<u8>, s: &str) {
    buf.extend_from_slice(&(s.len() as u32).to_le_bytes());
    buf.extend_from_slice(s.as_bytes());
}

#[inline]
pub fn format_size(bytes: i64) -> CompactString {
    if bytes < 1024 {
        return CompactString::from(format!("{} B", bytes));
    }

    let mut size = bytes as f64;
    let mut unit_index = 0;

    while size >= 1024.0 && unit_index < 4 {
        size /= 1024.0;
        unit_index += 1;
    }

    if size < 10.0 {
        CompactString::from(format!("{:.2} {}", size, SIZE_UNITS[unit_index]))
    } else if size < 100.0 {
        CompactString::from(format!("{:.1} {}", size, SIZE_UNITS[unit_index]))
    } else {
        CompactString::from(format!("{:.0} {}", size, SIZE_UNITS[unit_index]))
    }
}

/// 主扫描函数 - 优化版
/// 支持可选的渐进式流式传输：通过 app_handle 分批发送扫描结果
pub async fn scan_directory(
    path: &str,
    force_refresh: bool,
    perf_monitor: Arc<PerformanceMonitor>,
    app_handle: Option<tauri::AppHandle>,
) -> Result<ScanResult, anyhow::Error> {
    let _scan_id = perf_monitor.start_scan(path);
    let start_time = std::time::Instant::now();

    if path.trim().is_empty() {
        perf_monitor.add_error("路径不能为空".to_string());
        perf_monitor.end_scan();
        return Err(anyhow::anyhow!("路径不能为空"));
    }

    let path_buf = PathBuf::from(path);

    let metadata = match fs::metadata(&path_buf).await {
        Ok(m) => m,
        Err(e) => {
            perf_monitor.add_error(format!("无法访问路径: {}", e));
            perf_monitor.end_scan();
            return Err(anyhow::anyhow!("无法访问路径: {}", e));
        }
    };

    if !metadata.is_dir() {
        perf_monitor.add_error("不是目录".to_string());
        perf_monitor.end_scan();
        return Err(anyhow::anyhow!("不是目录"));
    }

    let canonical_path = match fs::canonicalize(&path_buf).await {
        Ok(p) => p,
        Err(e) => {
            perf_monitor.add_error(format!("路径规范化失败: {}", e));
            perf_monitor.end_scan();
            return Err(anyhow::anyhow!("路径规范化失败: {}", e));
        }
    };

    let root_dir = normalize_path_separator(canonical_path.as_os_str());

    let mtime = match metadata.modified() {
        Ok(m) => m,
        Err(_) => std::time::SystemTime::UNIX_EPOCH,
    };
    let mtime_datetime: chrono::DateTime<chrono::Local> = mtime.into();
    let mtime_timestamp = mtime_datetime.timestamp();

    // 1. 检查内存缓存
    if !force_refresh {
        let cache_check_start = std::time::Instant::now();
        if let Some(cached) = SCAN_CACHE.get(&root_dir) {
            // 如果缓存来自目录遍历，但当前进程是管理员且 MFT 可用，
            // 则放弃缓存并重新扫描，以升级到 MFT 快速路径。
            let can_upgrade_to_mft = !cached.result.mft_available
                && cfg!(target_os = "windows")
                && crate::fs::is_admin()
                && crate::fs::check_mft_available(&root_dir);

            if cached.dir_mtime >= mtime_datetime && !can_upgrade_to_mft {
                let cache_read_time = cache_check_start.elapsed().as_millis() as u64;
                perf_monitor.record_cache_hit(cache_read_time);

                let mut result = ScanResult::from(&cached.result);
                result.scan_time = 0.0;
                result.perf_metrics = Some(ScanPerfMetrics {
                    io_phase_ms: 0,
                    compute_phase_ms: 0,
                    serialize_phase_ms: 0,
                    cache_read_time_ms: cache_read_time,
                    files_scanned: result.items.len(),
                    dirs_scanned: result.items.iter().filter(|i| i.is_dir).count(),
                    io_throughput_mbps: 0.0,
                    memory_peak_mb: 0.0,
                    threads_used: 0,
                    cache_hit: true,
                    cache_source: Some("memory".to_string()),
                });

                perf_monitor.end_scan();
                return Ok(result);
            } else if can_upgrade_to_mft {
                eprintln!(
                    "[Scan] 管理员+MFT 可用，放弃旧缓存并重新扫描以启用 MFT: {}",
                    root_dir
                );
            }
        }

        // 2. 检查磁盘缓存
        let disk_cache = DiskCache::instance();
        if let Some(cached_result) = disk_cache.get(&root_dir, mtime_timestamp) {
            let can_upgrade_to_mft = !cached_result.mft_available
                && cfg!(target_os = "windows")
                && crate::fs::is_admin()
                && crate::fs::check_mft_available(&root_dir);

            if !can_upgrade_to_mft {
                let cache_read_time = cache_check_start.elapsed().as_millis() as u64;
                perf_monitor.record_cache_hit(cache_read_time);

                // 同时写入内存缓存
                SCAN_CACHE.insert(root_dir.clone(), cached_result.clone());

                let mut result = cached_result;
                result.scan_time = 0.0;
                result.perf_metrics = Some(ScanPerfMetrics {
                    io_phase_ms: 0,
                    compute_phase_ms: 0,
                    serialize_phase_ms: 0,
                    cache_read_time_ms: cache_read_time,
                    files_scanned: result.items.len(),
                    dirs_scanned: result.items.iter().filter(|i| i.is_dir).count(),
                    io_throughput_mbps: 0.0,
                    memory_peak_mb: 0.0,
                    threads_used: 0,
                    cache_hit: true,
                    cache_source: Some("disk".to_string()),
                });

                perf_monitor.end_scan();
                return Ok(result);
            } else {
                eprintln!(
                    "[Scan] 管理员+MFT 可用，放弃磁盘缓存并重新扫描以启用 MFT: {}",
                    root_dir
                );
            }
        }
    }

    SCAN_CACHE.invalidate(&root_dir);

    // ── P2 优化：USN Journal 增量更新 ──
    // 在失效缓存之前，先尝试用 USN Journal 增量更新过期的缓存数据
    // 这样即使 mtime 不匹配，也能秒级刷新
    #[cfg(target_os = "windows")]
    if !force_refresh {
        if let Some(updated_result) = try_usn_incremental_update(
            &root_dir,
            &canonical_path,
            mtime_timestamp,
            &perf_monitor,
        ) {
            perf_monitor.end_scan();
            return Ok(updated_result);
        }
    }

    // USN 增量失败，失效磁盘缓存并执行全量扫描
    DiskCache::instance().invalidate(&root_dir).ok();

    // ── P1 优化：MFT 直接读取（Everything 式快速路径） ──
    // Windows 管理员权限下，直接顺序读取 NTFS $MFT
    // 失败时自动回退到目录遍历
    let canonical_path_clone = canonical_path.clone();
    let perf_monitor_for_blocking = Arc::clone(&perf_monitor);
    let app_handle_for_blocking = app_handle.map(Arc::new);

    // 尝试 MFT 直接读取，失败则回退到目录遍历
    let mft_result = try_mft_scan_path(
        &canonical_path_clone,
        &root_dir,
        &perf_monitor_for_blocking,
        app_handle_for_blocking.as_ref(),
    );

    let output = match mft_result {
        Some(mft_output) => mft_output,
        None => tokio::task::spawn_blocking(move || {
            scan_directory_optimized_v4(
                &canonical_path_clone,
                &perf_monitor_for_blocking,
                app_handle_for_blocking,
            )
        })
        .await??,
    };

    let scan_time = start_time.elapsed().as_secs_f64();

    let result = ScanResult {
        items: output.items,
        total_size: output.total_size,
        total_size_formatted: format_size(output.total_size),
        scan_time,
        path: CompactString::from(path),
        mft_available: output.mft_available,
        timing: Some(output.timing.clone()),
        perf_metrics: Some(ScanPerfMetrics {
            io_phase_ms: (output.timing.scan_phase * 1000.0) as u64,
            compute_phase_ms: (output.timing.compute_phase * 1000.0) as u64,
            serialize_phase_ms: (output.timing.format_phase * 1000.0) as u64,
            cache_read_time_ms: 0,
            files_scanned: output.file_count,
            dirs_scanned: output.dir_count,
            io_throughput_mbps: output.throughput_mbps,
            memory_peak_mb: output.memory_peak_mb,
            threads_used: output.threads_used,
            cache_hit: false,
            cache_source: None,
        }),
    };

    // 写入两级缓存
    SCAN_CACHE.insert(root_dir.clone(), result.clone());
    DiskCache::instance().insert(&root_dir, &result, mtime_timestamp).ok();

    perf_monitor.end_scan();
    Ok(result)
}

struct ScanOutput {
    items: Vec<Item>,
    total_size: i64,
    timing: TimingInfo,
    file_count: usize,
    dir_count: usize,
    throughput_mbps: f64,
    memory_peak_mb: f64,
    threads_used: usize,
    mft_available: bool,
}

/// 从绝对路径中提取盘符和 MFT volume-relative 前缀。
/// MFT 返回的路径不带盘符（如 `Users/xxx/Documents/file.txt`），而 canonical path
/// 是完整路径（如 `C:/Users/xxx` 或 `//?/C:/Users/xxx`）。本函数返回盘符与
/// volume-relative 前缀，例如 `C:/Users/xxx` -> `('C', "users/xxx/")`，`C:/` -> `('C', "")`。
fn drive_and_vol_prefix(abs_path: &str) -> Option<(char, String)> {
    let normalized = abs_path.replace('\\', "/");
    // 处理 canonicalize 产生的 \\?\C:\ 前缀（标准化后为 //?/C:/）
    let trimmed = if normalized.starts_with("//?/") {
        &normalized[4..]
    } else {
        normalized.as_str()
    };

    if trimmed.len() >= 2 && trimmed.as_bytes().get(1) == Some(&b':') {
        let drive = trimmed.as_bytes()[0] as char;
        let rest = trimmed[2..].trim_start_matches('/');
        let prefix = if rest.is_empty() {
            String::new()
        } else {
            format!("{}/", rest.to_lowercase())
        };
        Some((drive, prefix))
    } else {
        None
    }
}

/// 把 MFT 返回的 volume-relative 路径转换为绝对路径。
/// 如果路径已以盘符开头，则直接规范化；否则补全盘符前缀。
fn mft_path_to_abs(drive: char, vol_relative_path: &str) -> CompactString {
    let p = vol_relative_path.replace('\\', "/");
    let vol_prefix = format!("{}:/", drive);
    let vol_alt = format!("{}:", drive);
    if p.starts_with(&vol_prefix) || p.starts_with(&vol_alt) {
        CompactString::from(p)
    } else if p.is_empty() {
        CompactString::from(vol_prefix)
    } else {
        CompactString::from(format!("{}{}", vol_prefix, p))
    }
}

/// 轻量扫描：只做 MFT 读取 + 文件名提取（不聚合目录大小、不排序、不格式化）。
/// 供全局搜索索引构建使用。与 try_mft_scan_path 使用相同的 canonicalize 预处理，
/// 但跳过聚合/format/sort，失败返回 None，调用者应回退到完整 scan_directory。
pub fn scan_lite(path: &str) -> Option<Vec<Item>> {
    if is_mft_disabled() {
        return None;
    }

    let canonical = std::fs::canonicalize(path).ok()?;
    let root_path_str = normalize_path_separator(canonical.as_os_str());
    let mft_result = crate::fs::try_mft_scan(&root_path_str)?;
    let (drive, vol_prefix) = drive_and_vol_prefix(&root_path_str)?;

    let items: Vec<Item> = mft_result
        .files
        .into_iter()
        .filter(|f| {
            let p = f.path.to_lowercase();
            vol_prefix.is_empty() || p.starts_with(&vol_prefix)
        })
        .map(|f| Item {
            path: mft_path_to_abs(drive, &f.path),
            name: CompactString::from(f.name),
            size: f.size as i64,
            size_formatted: CompactString::new(),
            is_dir: f.is_dir,
        })
        .collect();

    Some(items)
}

/// 尝试使用 MFT 直接读取扫描（Everything 式快速路径）
/// 仅在 Windows + 管理员权限 + NTFS 卷上生效
/// 返回 None 表示不可用，调用者应回退到目录遍历
fn try_mft_scan_path(
    canonical_path: &Path,
    _root_dir: &str,
    perf_monitor: &Arc<PerformanceMonitor>,
    app_handle: Option<&Arc<tauri::AppHandle>>,
) -> Option<ScanOutput> {
    if is_mft_disabled() {
        return None;
    }

    let root_path_str = canonical_path.to_string_lossy().to_string();
    let (drive, vol_prefix) = drive_and_vol_prefix(&root_path_str)?;

    // 尝试 MFT 全卷扫描
    let mft_result = crate::fs::try_mft_scan(&root_path_str)?;

    let total_start = std::time::Instant::now();

    perf_monitor.start_io_phase();
    let scan_start = std::time::Instant::now();

    // 过滤：只保留目标目录下的文件
    // MFT 返回的路径是 volume-relative（不带盘符），需用 volume-relative 前缀匹配
    let normalized_root = vol_prefix;

    let mut items: Vec<Item> = mft_result
        .files
        .into_iter()
        .filter(|f| {
            let p = f.path.to_lowercase();
            normalized_root.is_empty() || p.starts_with(&normalized_root)
        })
        .map(|f| Item {
            path: mft_path_to_abs(drive, &f.path),
            name: CompactString::from(f.name),
            size: f.size as i64,
            size_formatted: CompactString::new(), // 下面统一格式化
            is_dir: f.is_dir,
        })
        .collect();

    let file_count = items.iter().filter(|i| !i.is_dir).count();
    let dir_count = items.iter().filter(|i| i.is_dir).count();

    let scan_phase = scan_start.elapsed();
    perf_monitor.end_io_phase();

    // 计算目录大小（聚合子文件大小到父目录）
    perf_monitor.start_compute_phase();
    let compute_start = std::time::Instant::now();

    use std::collections::HashMap;

    // 目录大小聚合：path → 下标索引 + 按下标累加，避免每层祖先都分配 CompactString
    let dir_index: HashMap<&str, usize> = items
        .iter()
        .enumerate()
        .filter(|(_, it)| it.is_dir)
        .map(|(i, it)| (it.path.as_str(), i))
        .collect();

    let mut dir_sizes: Vec<i64> = vec![0; items.len()];

    for item in items.iter() {
        if item.is_dir || item.size <= 0 {
            continue;
        }
        let file_path = item.path.as_str();
        let mut pos = 0;
        while let Some(slash_pos) = file_path[pos..].find('/') {
            let abs_pos = pos + slash_pos;
            let parent = &file_path[..abs_pos];
            if let Some(&idx) = dir_index.get(parent) {
                dir_sizes[idx] += item.size;
            }
            pos = abs_pos + 1;
        }
    }

    // 释放对 items 的借用，以便下方 iter_mut 可变借用
    drop(dir_index);

    let compute_phase = compute_start.elapsed();

    // 更新目录条目的 size 和 size_formatted
    for (i, item) in items.iter_mut().enumerate() {
        if item.is_dir {
            item.size = dir_sizes[i];
        }
        item.size_formatted = format_size(item.size);
    }

    // 按大小降序排序
    items.sort_unstable_by(|a, b| b.size.cmp(&a.size));

    let format_phase = compute_start.elapsed(); // approximate
    let total = total_start.elapsed();
    perf_monitor.end_compute_phase();

    let actual_total_size: i64 = items
        .iter()
        .filter(|i| !i.is_dir)
        .map(|i| i.size)
        .sum();

    let throughput_mbps = if scan_phase.as_secs_f64() > 0.0 {
        (actual_total_size as f64 / 1024.0 / 1024.0) / scan_phase.as_secs_f64()
    } else {
        0.0
    };

    let memory_peak_mb = (items.capacity() * std::mem::size_of::<Item>()) as f64 / 1024.0 / 1024.0;

    perf_monitor.update_memory_stats(memory_peak_mb, memory_peak_mb);
    perf_monitor.update_io_stats(file_count, dir_count, actual_total_size as u64, file_count + dir_count);

    // 流式传输（与目录遍历保持一致的行为）
    if let Some(app) = app_handle {
        for chunk in items.chunks(500) {
            let _ = app.emit("scan-batch", chunk.to_vec());
        }
    }

    eprintln!(
        "[MFT] 扫描完成: {} 文件, {} 目录, {:.2}s (filtered from {} total)",
        file_count,
        dir_count,
        total.as_secs_f64(),
        mft_result.file_count + mft_result.dir_count
    );

    // 保存 USN 检查点，供下次增量更新使用
    save_usn_checkpoint(&root_path_str);

    Some(ScanOutput {
        items,
        total_size: actual_total_size,
        timing: TimingInfo {
            scan_phase: scan_phase.as_secs_f64(),
            compute_phase: compute_phase.as_secs_f64(),
            format_phase: format_phase.as_secs_f64(),
            total: total.as_secs_f64(),
        },
        file_count,
        dir_count,
        throughput_mbps,
        memory_peak_mb,
        threads_used: 1, // MFT 扫描是单线程顺序读取
        mft_available: true,
    })
}

// ─── USN Journal 增量更新 ───────────────────────────────────

/// 保存 USN 检查点
#[cfg(target_os = "windows")]
fn save_usn_checkpoint(path: &str) {
    if let Some(drive) = crate::fs::extract_drive_letter(path) {
        if let Some(checkpoint) = crate::fs::get_checkpoint(drive) {
            let checkpoint_path = usn_checkpoint_path(drive);
            if let Some(parent) = checkpoint_path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            if let Ok(json) = serde_json::to_string(&checkpoint) {
                let _ = std::fs::write(&checkpoint_path, json);
                eprintln!(
                    "[USN] 检查点已保存: {}.{} (USN={})",
                    drive,
                    checkpoint.journal_id,
                    checkpoint.max_usn
                );
            }
        }
    }
}

#[cfg(not(target_os = "windows"))]
fn save_usn_checkpoint(_path: &str) {}

/// USN 检查点文件路径
#[cfg(target_os = "windows")]
fn usn_checkpoint_path(drive: char) -> std::path::PathBuf {
    let home = std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .unwrap_or_default();
    let mut p = std::path::PathBuf::from(home);
    p.push(".flashdir");
    p.push(format!("usn_checkpoint_{}.json", drive));
    p
}

#[cfg(not(target_os = "windows"))]
fn usn_checkpoint_path(_drive: char) -> std::path::PathBuf {
    std::path::PathBuf::new()
}

/// 尝试使用 USN Journal 增量更新缓存
/// 成功返回更新后的 ScanResult，失败返回 None（回退到全量扫描）
#[cfg(target_os = "windows")]
fn try_usn_incremental_update(
    root_dir: &str,
    _canonical_path: &std::path::Path,
    _mtime_timestamp: i64,
    _perf_monitor: &Arc<PerformanceMonitor>,
) -> Option<ScanResult> {
    let drive = crate::fs::extract_drive_letter(root_dir)?;

    // 加载之前的检查点
    let cp_path = usn_checkpoint_path(drive);
    let checkpoint: crate::fs::UsnCheckpoint = {
        let data = std::fs::read_to_string(&cp_path).ok()?;
        serde_json::from_str(&data).ok()?
    };

    // 检查点过期检查（超过 1 小时的检查点不使用增量更新）
    let now = chrono::Utc::now().timestamp();
    if now - checkpoint.created_at > 3600 {
        return None;
    }

    // 读取增量变更
    let (changes, new_checkpoint) =
        crate::fs::read_incremental_changes(drive, &checkpoint).ok()?;

    // 如果变更太多（>5000），不如全量扫描
    if changes.len() > 5000 {
        eprintln!(
            "[USN] 变更过多 ({} 条)，回退到全量扫描",
            changes.len()
        );
        return None;
    }

    if changes.is_empty() {
        eprintln!("[USN] 无变更，直接返回缓存结果");
        // 更新检查点时间戳
        let updated_cp = crate::fs::UsnCheckpoint {
            created_at: now,
            ..new_checkpoint
        };
        if let Ok(json) = serde_json::to_string(&updated_cp) {
            let _ = std::fs::write(&cp_path, json);
        }
        // 返回磁盘缓存（无需修改，mtime 已通过 USN 验证为最新）
        if let Some(cached) = DiskCache::instance().get_stale(root_dir) {
            // 重新写入内存缓存
            SCAN_CACHE.insert(root_dir.to_string(), cached.clone());
            let _ = DiskCache::instance().insert(root_dir, &cached, new_checkpoint.created_at);
            return Some(cached);
        }
        return None;
    }

    let create_count = changes.iter().filter(|c| c.reason & crate::fs::USN_REASON_FILE_CREATE != 0).count();
    let delete_count = changes.iter().filter(|c| c.reason & crate::fs::USN_REASON_FILE_DELETE != 0).count();
    let rename_count = changes.iter().filter(|c| c.reason & (crate::fs::USN_REASON_RENAME_OLD_NAME | crate::fs::USN_REASON_RENAME_NEW_NAME) != 0).count();
    let data_count = changes.iter().filter(|c| c.reason & (crate::fs::USN_REASON_DATA_OVERWRITE | crate::fs::USN_REASON_DATA_EXTEND | crate::fs::USN_REASON_DATA_TRUNCATION) != 0).count();

    eprintln!(
        "[USN] 增量更新: {} 条变更 (CREATE={}, DELETE={}, RENAME={}, DATA={})",
        changes.len(), create_count, delete_count, rename_count, data_count,
    );

    // ── 加载缓存的扫描结果 ──
    // 使用 get_stale 获取过期缓存数据（忽略 mtime 检查），因为 USN 增量会将其更新到最新
    let cached_items = {
        if let Some(cached) = DiskCache::instance().get_stale(root_dir) {
            cached.items
        } else {
            eprintln!("[USN] 磁盘缓存未命中，无法应用增量更新");
            return None;
        }
    };

    if cached_items.is_empty() {
        eprintln!("[USN] 缓存为空，无法应用增量更新");
        return None;
    }

    eprintln!(
        "[USN] 加载缓存: {} 个项目，开始应用 USN 变更",
        cached_items.len()
    );

    // ── 打开 MFT 扫描器用于 FRN → 路径解析 ──
    let scanner = match crate::fs::MftScanner::open(drive) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("[USN] 无法打开 MFT 扫描器: {}", e);
            return None;
        }
    };

    // ── 路径格式检测（必须在移动 cached_items 之前） ──
    // MFT 扫描产生 volume-relative 路径 (如 "Users/xxx/Documents/file.txt")
    // v4 遍历产生 root-relative 路径 (如 "Documents/file.txt")
    // USN FRN 解析总是产生 volume-relative 路径
    // 需要根据缓存实际格式决定是否截去前缀
    let volume_relative_prefix = {
        // root_dir 格式: "C:/Users/xxx" (forward slashes)
        let without_drive = if root_dir.len() >= 2 && root_dir.as_bytes().get(1) == Some(&b':') {
            &root_dir[2..] // 去掉 "C:"
        } else {
            root_dir
        };
        let trimmed = without_drive.trim_start_matches('/');
        if trimmed.is_empty() {
            String::new()
        } else {
            format!("{}/", trimmed)
        }
    };

    let cached_is_mft_format = {
        let first_cached = cached_items.first();
        first_cached.map_or(false, |item| {
            !volume_relative_prefix.is_empty()
                && item.path.starts_with(volume_relative_prefix.as_str())
        })
    };

    eprintln!(
        "[USN] 缓存路径格式: {}, 卷相对前缀: \"{}\"",
        if cached_is_mft_format { "MFT (volume-relative)" } else { "v4 (root-relative)" },
        volume_relative_prefix,
    );

    let normalize_to_cache_format = |vol_relative_path: &str| -> CompactString {
        if cached_is_mft_format {
            // MFT 格式：直接使用 volume-relative 路径
            CompactString::from(vol_relative_path)
        } else if !volume_relative_prefix.is_empty()
            && vol_relative_path.starts_with(&volume_relative_prefix)
        {
            // v4 格式：截去卷相对前缀，得到 root-relative 路径
            CompactString::from(&vol_relative_path[volume_relative_prefix.len()..])
        } else {
            // 后备：路径不在预期前缀下，直接使用
            CompactString::from(vol_relative_path)
        }
    };

    // ── 构建 HashMap 索引 (path → Item) ──
    use std::collections::HashMap;

    let mut items_map: HashMap<CompactString, Item> = HashMap::with_capacity(cached_items.len() + changes.len());
    for item in cached_items {
        items_map.insert(item.path.clone(), item);
    }

    // ── 路径解析缓存 (parent_ref → volume_relative_path) ──
    let mut parent_path_cache: HashMap<u64, Option<String>> = HashMap::new();

    let resolve_parent_path = |parent_ref: u64,
                                scanner: &crate::fs::MftScanner,
                                cache: &mut HashMap<u64, Option<String>>|
     -> Option<String> {
        if let Some(cached) = cache.get(&parent_ref) {
            return cached.clone();
        }
        let result = scanner.resolve_frn_path(parent_ref).ok().flatten();
        cache.insert(parent_ref, result.clone());
        result
    };

    // 辅助：把 USN 的 Windows FILETIME 转换为 Unix 秒级时间戳
    let filetime_to_unix = |ft: i64| -> i64 {
        // FILETIME 是自 1601-01-01 起的 100 纳秒间隔数
        // 与 Unix 时间戳（1970-01-01）的差值为 11644473600 秒
        (ft - 116444736000000000) / 10_000_000
    };

    // ── 应用 USN 变更 ──
    // Phase 1: 处理删除和旧名称（先移除）
    for change in &changes {
        let reason = change.reason;

        let is_delete = reason & crate::fs::USN_REASON_FILE_DELETE != 0;
        let is_rename_old = reason & crate::fs::USN_REASON_RENAME_OLD_NAME != 0;

        if !is_delete && !is_rename_old {
            continue;
        }

        if let Some(parent_path) = resolve_parent_path(change.parent_ref, &scanner, &mut parent_path_cache) {
            let vol_path = if parent_path.is_empty() {
                change.name.clone()
            } else {
                format!("{}/{}", parent_path, change.name)
            };
            let cache_key = normalize_to_cache_format(&vol_path);
            if items_map.remove(&cache_key).is_some() {
                let action = if is_delete { "DEL" } else { "RN_OLD" };
                eprintln!("  [USN-{}] 移除: {}", action, cache_key);
            }

            // 同步到全局索引与 SQLite
            let abs_path = crate::global_search::normalize_abs_path(drive, &vol_path);
            crate::global_search::instance().remove_by_path(&abs_path);
            let _ = DiskCache::instance().remove_global_index_by_path(&abs_path);
        }
    }

    // Phase 2: 处理创建、重命名新名称、数据变更
    for change in &changes {
        let reason = change.reason;

        let is_create = reason & crate::fs::USN_REASON_FILE_CREATE != 0;
        let is_rename_new = reason & crate::fs::USN_REASON_RENAME_NEW_NAME != 0;
        let is_data_change = reason & (crate::fs::USN_REASON_DATA_OVERWRITE
            | crate::fs::USN_REASON_DATA_EXTEND
            | crate::fs::USN_REASON_DATA_TRUNCATION)
            != 0;

        if !is_create && !is_rename_new && !is_data_change {
            continue;
        }

        if let Some(parent_path) = resolve_parent_path(change.parent_ref, &scanner, &mut parent_path_cache) {
            let vol_path = if parent_path.is_empty() {
                change.name.clone()
            } else {
                format!("{}/{}", parent_path, change.name)
            };
            let cache_key = normalize_to_cache_format(&vol_path);
            let abs_path = crate::global_search::normalize_abs_path(drive, &vol_path);
            let mtime = filetime_to_unix(change.timestamp);

            if is_create || is_rename_new {
                // 读取 MFT 获取文件大小和目录标志
                let (file_size, is_dir) = match scanner.read_single_record(change.file_ref) {
                    Ok(Some(record)) => (record.real_size as i64, record.is_dir),
                    _ => {
                        // 回退：用 USN attributes 判断目录标志
                        let is_dir_attr = (change.attributes & 0x10) != 0; // FILE_ATTRIBUTE_DIRECTORY
                        (0i64, is_dir_attr)
                    }
                };

                let item = Item {
                    path: cache_key.clone(),
                    name: CompactString::from(change.name.as_str()),
                    size: file_size,
                    size_formatted: format_size(file_size),
                    is_dir,
                };

                items_map.insert(cache_key.clone(), item);
                let action = if is_create { "CREATE" } else { "RN_NEW" };
                eprintln!(
                    "  [USN-{}] 添加: {} ({} bytes, dir={})",
                    action, cache_key, file_size, is_dir
                );

                // 同步到全局索引与 SQLite
                let name = change.name.clone();
                let entry = crate::global_search::IndexEntry {
                    path: abs_path.clone(),
                    name: name.clone(),
                    name_lower: name.to_lowercase(),
                    size: file_size,
                    is_dir,
                    mtime,
                };
                crate::global_search::instance().upsert(entry.clone());
                let _ = DiskCache::instance().upsert_global_index_entry(&entry);
            } else if is_data_change {
                // 更新文件大小（从 MFT 读取最新值）
                if let Some(item) = items_map.get_mut(&cache_key) {
                    if let Ok(Some(record)) = scanner.read_single_record(change.file_ref) {
                        if !item.is_dir {
                            let new_size = record.real_size as i64;
                            if new_size != item.size {
                                eprintln!(
                                    "  [USN-DATA] 更新大小: {} {} -> {} bytes",
                                    cache_key, item.size, new_size
                                );
                                item.size = new_size;
                                item.size_formatted = format_size(new_size);

                                // 同步到全局索引与 SQLite
                                let name = item.name.to_string();
                                let entry = crate::global_search::IndexEntry {
                                    path: abs_path.clone(),
                                    name: name.clone(),
                                    name_lower: name.to_lowercase(),
                                    size: new_size,
                                    is_dir: item.is_dir,
                                    mtime,
                                };
                                crate::global_search::instance().upsert(entry.clone());
                                let _ = DiskCache::instance().upsert_global_index_entry(&entry);
                            }
                        }
                    }
                }
            }
        }
    }

    // 释放 MFT 扫描器
    drop(scanner);
    drop(parent_path_cache);

    // ── 重建目录大小 ──
    // 文件大小可能已更新，需要重新聚合计入目录
    let mut new_items: Vec<Item> = items_map.into_values().collect();

    // 重新计算目录大小：为每个目录累计其子文件的字节数
    {
        use std::collections::HashMap as StdHashMap;

        let mut dir_sizes: StdHashMap<CompactString, i64> = StdHashMap::new();

        for item in &new_items {
            if !item.is_dir && item.size > 0 {
                let file_path = item.path.as_str();
                // 沿路径向上，累加到每个祖先目录
                let mut pos = 0;
                while let Some(slash_pos) = file_path[pos..].find('/') {
                    let abs_pos = pos + slash_pos;
                    let parent = &file_path[..abs_pos];
                    *dir_sizes
                        .entry(CompactString::from(parent))
                        .or_insert(0) += item.size;
                    pos = abs_pos + 1;
                }
                // 也计入根
                *dir_sizes.entry(CompactString::new()).or_insert(0) += item.size;
            }
        }

        for item in &mut new_items {
            if item.is_dir {
                item.size = dir_sizes.get(&item.path).copied().unwrap_or(0);
                item.size_formatted = format_size(item.size);
            }
        }
    }

    // 按大小降序排序
    new_items.sort_unstable_by(|a, b| b.size.cmp(&a.size));

    let actual_total_size: i64 = new_items
        .iter()
        .filter(|i| !i.is_dir)
        .map(|i| i.size)
        .sum();

    let new_file_count = new_items.iter().filter(|i| !i.is_dir).count();
    let new_dir_count = new_items.iter().filter(|i| i.is_dir).count();

    eprintln!(
        "[USN] 增量更新完成: {} 文件, {} 目录, {} ({} 变更已应用)",
        new_file_count,
        new_dir_count,
        format_size(actual_total_size),
        changes.len(),
    );

    // ── 更新检查点 ──
    let updated_cp = crate::fs::UsnCheckpoint {
        created_at: chrono::Utc::now().timestamp(),
        ..new_checkpoint
    };
    if let Ok(json) = serde_json::to_string(&updated_cp) {
        let _ = std::fs::write(&cp_path, json);
    }

    // ── 写回缓存 ──
    let result = ScanResult {
        items: new_items,
        total_size: actual_total_size,
        total_size_formatted: format_size(actual_total_size),
        scan_time: 0.0, // USN 增量更新视为即时
        path: CompactString::from(root_dir),
        mft_available: false, // USN 增量更新路径不直接依赖 MFT 直读能力标志
        timing: Some(TimingInfo {
            scan_phase: 0.0,
            compute_phase: 0.0,
            format_phase: 0.0,
            total: 0.0,
        }),
        perf_metrics: Some(ScanPerfMetrics {
            io_phase_ms: 0,
            compute_phase_ms: 0,
            serialize_phase_ms: 0,
            cache_read_time_ms: 0,
            files_scanned: 0,
            dirs_scanned: 0,
            io_throughput_mbps: 0.0,
            memory_peak_mb: 0.0,
            threads_used: 0,
            cache_hit: true,
            cache_source: Some("usn".to_string()),
        }),
    };

    // 写入两级缓存
    SCAN_CACHE.insert(root_dir.to_string(), result.clone());
    let _ = DiskCache::instance().insert(root_dir, &result, new_checkpoint.created_at);

    Some(result)
}

#[cfg(not(target_os = "windows"))]
fn try_usn_incremental_update(
    _root_dir: &str,
    _canonical_path: &std::path::Path,
    _mtime_timestamp: i64,
    _perf_monitor: &Arc<PerformanceMonitor>,
) -> Option<ScanResult> {
    None
}

/// 优化的扫描实现 v4
/// 集成：性能监控、内存优化、Windows 原生 I/O、渐进式流式传输
fn scan_directory_optimized_v4(
    root_path: &Path,
    perf_monitor: &Arc<PerformanceMonitor>,
    app_handle: Option<Arc<tauri::AppHandle>>,
) -> Result<ScanOutput, anyhow::Error> {
    use rayon::prelude::*;

    let total_start = std::time::Instant::now();

    let (dir_sender, dir_receiver): (Sender<PathBuf>, Receiver<PathBuf>) = unbounded();
    let (item_sender, item_receiver): (Sender<ItemInternal>, Receiver<ItemInternal>) = unbounded();

    dir_sender.send(root_path.to_path_buf()).unwrap();

    let cpu_count = num_cpus::get();
    let num_threads = (cpu_count * 2).min(32).max(8);
    perf_monitor.set_threads_used(num_threads);

    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(num_threads)
        .build()?;

    perf_monitor.start_io_phase();
    let scan_start = std::time::Instant::now();

    pool.scope(|s| {
        for _ in 0..num_threads {
            let dir_sender = dir_sender.clone();
            let dir_receiver = dir_receiver.clone();
            let item_sender = item_sender.clone();
            let app_handle_for_worker = app_handle.clone();

            s.spawn(move |_| {
                let mut idle_count = 0;
                // 流式传输缓冲区：每 200 条 emit 一次
                let mut stream_batch: Vec<Item> = Vec::with_capacity(200);

                loop {
                    let dir_path = match dir_receiver.try_recv() {
                        Ok(d) => {
                            idle_count = 0;
                            d
                        }
                        Err(_) => {
                            idle_count += 1;
                            if idle_count > 100 && dir_sender.is_empty() {
                                break;
                            }
                            std::thread::yield_now();
                            continue;
                        }
                    };

                    // 使用平台优化的目录遍历器
                    // Windows: FindFirstFileExW 直接读取 size/attrs，零额外 syscall
                    // 其他平台: 标准库 read_dir（Linux getdents64 已返回 d_type）
                    if let Ok(entries) = crate::fs::read_dir_entries(&dir_path) {
                        for entry in entries {
                            if entry.is_symlink {
                                continue;
                            }

                            let abs_path = normalize_path_separator_compact(entry.path.as_os_str());
                            let size = entry.size as i64;

                            if entry.is_dir {
                                let _ = dir_sender.send(entry.path);
                            }

                            let _ = item_sender.send(ItemInternal {
                                path: abs_path.clone(),
                                name: CompactString::from(entry.name.as_str()),
                                size,
                                is_dir: entry.is_dir,
                            });

                            // 渐进式流式传输
                            if let Some(app) = app_handle_for_worker.as_ref() {
                                stream_batch.push(Item {
                                    path: abs_path,
                                    name: CompactString::from(entry.name),
                                    size,
                                    size_formatted: format_size(size),
                                    is_dir: entry.is_dir,
                                });
                                if stream_batch.len() >= 200 {
                                    let _ = app.emit("scan-batch", std::mem::take(&mut stream_batch));
                                }
                            }
                        }
                    }
                }

                // 发送当前 worker 剩余的批次
                if let Some(app) = app_handle_for_worker.as_ref() {
                    if !stream_batch.is_empty() {
                        let _ = app.emit("scan-batch", std::mem::take(&mut stream_batch));
                    }
                }
            });
        }
    });

    drop(item_sender);
    drop(dir_sender);

    let scan_phase = scan_start.elapsed();
    perf_monitor.end_io_phase();
    
    perf_monitor.start_compute_phase();
    let compute_start = std::time::Instant::now();

    let internal_items: Vec<ItemInternal> = item_receiver.try_iter().collect();
    let file_count = internal_items.iter().filter(|i| !i.is_dir).count();
    let dir_count = internal_items.len() - file_count;

    let actual_total_size: i64 = internal_items
        .iter()
        .filter(|i| !i.is_dir)
        .map(|i| i.size)
        .sum();

    // 计算 I/O 吞吐量
    let throughput_mbps = if scan_phase.as_secs_f64() > 0.0 {
        (actual_total_size as f64 / 1024.0 / 1024.0) / scan_phase.as_secs_f64()
    } else {
        0.0
    };

    // 目录大小聚合：建立"目录 path → 在 internal_items 中的下标"索引，
    // 配合按下标对齐的原子累加数组，把每个文件大小沿路径向上累加到各祖先目录。
    // 旧实现为每个祖先 new 一个 CompactString（O(文件数×深度) 堆分配），这里改为仅 index 写入，零字符串分配。
    use std::sync::atomic::{AtomicI64, Ordering};

    let dir_index: HashMap<&str, usize> = internal_items
        .iter()
        .enumerate()
        .filter(|(_, it)| it.is_dir)
        .map(|(i, it)| (it.path.as_str(), i))
        .collect();

    let dir_sizes: Vec<AtomicI64> = (0..internal_items.len())
        .map(|_| AtomicI64::new(0))
        .collect();

    internal_items
        .par_iter()
        .for_each(|it| {
            if it.is_dir {
                return;
            }
            let file_path = it.path.as_str();
            let mut pos = 0;
            while let Some(slash_pos) = file_path[pos..].find('/') {
                let abs_pos = pos + slash_pos;
                let parent = &file_path[..abs_pos];
                if let Some(&idx) = dir_index.get(parent) {
                    dir_sizes[idx].fetch_add(it.size, Ordering::Relaxed);
                }
                pos = abs_pos + 1;
            }
        });

    // 释放对 internal_items 的借用，以便下方 into_par_iter 消费它
    drop(dir_index);

    let compute_phase = compute_start.elapsed();
    let format_start = std::time::Instant::now();

    // 复用 internal_items（原地转换），不再额外拷贝一份中间结构
    let mut items_vec: Vec<Item> = internal_items
        .into_par_iter()
        .enumerate()
        .map(|(i, internal)| {
            let size = if internal.is_dir {
                dir_sizes[i].load(Ordering::Relaxed)
            } else {
                internal.size
            };

            Item {
                path: internal.path,
                name: internal.name,
                size,
                size_formatted: format_size(size),
                is_dir: internal.is_dir,
            }
        })
        .collect();

    items_vec.sort_unstable_by(|a, b| b.size.cmp(&a.size));

    let format_phase = format_start.elapsed();
    let total = total_start.elapsed();

    perf_monitor.end_compute_phase();

    // 估算内存使用（internal_items 已消费进 items_vec；dir_sizes 为紧凑原子数组）
    let memory_peak_mb = (items_vec.capacity() * std::mem::size_of::<Item>()
        + dir_sizes.len() * std::mem::size_of::<AtomicI64>()) as f64
        / 1024.0
        / 1024.0;

    perf_monitor.update_memory_stats(memory_peak_mb, memory_peak_mb);
    perf_monitor.update_io_stats(file_count, dir_count, actual_total_size as u64, file_count + dir_count);

    Ok(ScanOutput {
        items: items_vec,
        total_size: actual_total_size,
        timing: TimingInfo {
            scan_phase: scan_phase.as_secs_f64(),
            compute_phase: compute_phase.as_secs_f64(),
            format_phase: format_phase.as_secs_f64(),
            total: total.as_secs_f64(),
        },
        file_count,
        dir_count,
        throughput_mbps,
        memory_peak_mb,
        threads_used: num_threads,
        mft_available: false,
    })
}

struct ItemInternal {
    path: CompactString,
    name: CompactString,
    size: i64,
    is_dir: bool,
}

#[inline]
/// 去掉 Windows canonicalize 产生的 \\?\ 或 //?/ 前缀，并统一使用正斜杠。
fn strip_unc_prefix(s: &str) -> &str {
    if s.starts_with("\\\\?\\") || s.starts_with("//?/") {
        &s[4..]
    } else {
        s
    }
}

fn normalize_path_separator(path: &std::ffi::OsStr) -> String {
    let s = path.to_string_lossy();
    let stripped = strip_unc_prefix(&s);
    if stripped.contains('\\') {
        stripped.replace('\\', "/")
    } else {
        stripped.to_string()
    }
}

#[inline]
fn normalize_path_separator_compact(path: &std::ffi::OsStr) -> CompactString {
    let s = path.to_string_lossy();
    let stripped = strip_unc_prefix(&s);
    if stripped.contains('\\') {
        CompactString::from(stripped.replace('\\', "/"))
    } else {
        CompactString::from(stripped)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_drive_and_vol_prefix() {
        assert_eq!(drive_and_vol_prefix("C:/Users/xxx"), Some(('C', "users/xxx/".to_string())));
        assert_eq!(drive_and_vol_prefix("C:/"), Some(('C', String::new())));
        assert_eq!(drive_and_vol_prefix("C:\\Users\\xxx"), Some(('C', "users/xxx/".to_string())));
        assert_eq!(drive_and_vol_prefix("//?/C:/Users/xxx"), Some(('C', "users/xxx/".to_string())));
        assert_eq!(drive_and_vol_prefix("\\\\?\\C:\\Users\\xxx"), Some(('C', "users/xxx/".to_string())));
        assert_eq!(drive_and_vol_prefix("/home/xxx"), None);
    }

    #[test]
    fn test_mft_path_to_abs() {
        assert_eq!(mft_path_to_abs('C', "Users/xxx/file.txt"), CompactString::from("C:/Users/xxx/file.txt"));
        assert_eq!(mft_path_to_abs('C', "C:/Users/xxx/file.txt"), CompactString::from("C:/Users/xxx/file.txt"));
        assert_eq!(mft_path_to_abs('C', ""), CompactString::from("C:/"));
    }
}

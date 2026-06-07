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

pub type CompactString = SmartString<smartstring::Compact>;

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
            if cached.dir_mtime >= mtime_datetime {
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
            }
        }

        // 2. 检查磁盘缓存
        let disk_cache = DiskCache::instance();
        if let Some(cached_result) = disk_cache.get(&root_dir, mtime_timestamp) {
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
        }
    }

    SCAN_CACHE.invalidate(&root_dir);
    DiskCache::instance().invalidate(&root_dir).ok();

    // ── P2 优化：USN Journal 增量更新 ──
    // 如果有之前的 MFT 扫描检查点，先尝试增量更新
    // 仅在变更较少（<5000 条）时使用，否则回退到全量 MFT
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
    let root_path_str = canonical_path.to_string_lossy();

    // 尝试 MFT 全卷扫描
    let mft_result = crate::fs::try_mft_scan(&root_path_str)?;

    let total_start = std::time::Instant::now();

    perf_monitor.start_io_phase();
    let scan_start = std::time::Instant::now();

    // 过滤：只保留目标目录下的文件
    // MFT 返回的路径使用 "/" 分隔符，需要统一
    let normalized_root = normalize_path_separator(canonical_path.as_os_str()).to_lowercase();

    let mut items: Vec<Item> = mft_result
        .files
        .into_iter()
        .filter(|f| {
            let path_lower = f.path.to_lowercase();
            path_lower.starts_with(&normalized_root)
        })
        .map(|f| Item {
            path: CompactString::from(f.path),
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

    use dashmap::DashMap;
    use std::collections::HashMap;

    let dir_sizes: HashMap<CompactString, i64> = {
        let ds = DashMap::with_capacity_and_hasher(
            (file_count / 4).max(64),
            ahash::RandomState::new(),
        );

        items
            .iter()
            .filter(|i| !i.is_dir && i.size > 0)
            .for_each(|item| {
                let file_path = item.path.as_str();
                let mut pos = 0;
                while let Some(slash_pos) = file_path[pos..].find('/') {
                    let abs_pos = pos + slash_pos;
                    let parent_path = &file_path[..abs_pos];
                    ds.entry(CompactString::from(parent_path))
                        .and_modify(|v| *v += item.size)
                        .or_insert(item.size);
                    pos = abs_pos + 1;
                }
                ds.entry(CompactString::new())
                    .and_modify(|v| *v += item.size)
                    .or_insert(item.size);
            });

        ds.iter()
            .map(|entry| (entry.key().clone(), *entry.value()))
            .collect()
    };

    let compute_phase = compute_start.elapsed();

    // 更新目录条目的 size 和 size_formatted
    for item in &mut items {
        if item.is_dir {
            item.size = dir_sizes.get(&item.path).copied().unwrap_or(0);
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
        eprintln!("[USN] 无变更，使用缓存");
        // 更新检查点时间戳
        let updated_cp = crate::fs::UsnCheckpoint {
            created_at: now,
            ..new_checkpoint
        };
        if let Ok(json) = serde_json::to_string(&updated_cp) {
            let _ = std::fs::write(&cp_path, json);
        }
        // 返回缓存（调用者已检查缓存，这里不会再走缓存路径）
        return None;
    }

    eprintln!(
        "[USN] 增量更新: {} 条变更 (CREATE={}, DELETE={}, RENAME={}, DATA={})",
        changes.len(),
        changes.iter().filter(|c| c.reason & crate::fs::USN_REASON_FILE_CREATE != 0).count(),
        changes.iter().filter(|c| c.reason & crate::fs::USN_REASON_FILE_DELETE != 0).count(),
        changes.iter().filter(|c| c.reason & (crate::fs::USN_REASON_RENAME_OLD_NAME | crate::fs::USN_REASON_RENAME_NEW_NAME) != 0).count(),
        changes.iter().filter(|c| c.reason & (crate::fs::USN_REASON_DATA_OVERWRITE | crate::fs::USN_REASON_DATA_EXTEND | crate::fs::USN_REASON_DATA_TRUNCATION) != 0).count(),
    );

    // 更新检查点
    if let Ok(json) = serde_json::to_string(&new_checkpoint) {
        let _ = std::fs::write(&cp_path, json);
    }

    // 增量更新目前回退到全量 MFT 扫描（变更列表已用于判断是否需要重扫）
    // TODO: 实际应用增量变更到缓存结果
    None
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
    let file_entries = Arc::new(dashmap::DashMap::with_capacity_and_hasher(
        4096,
        ahash::RandomState::new(),
    ));

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
            let file_entries = Arc::clone(&file_entries);
            let root_path = root_path.to_path_buf();
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

                            let rel_path = match entry.path.strip_prefix(&root_path) {
                                Ok(p) => normalize_path_separator_compact(p.as_os_str()),
                                Err(_) => continue,
                            };

                            let size = entry.size as i64;

                            if !entry.is_dir {
                                file_entries.insert(rel_path.clone(), size);
                            } else {
                                let _ = dir_sender.send(entry.path);
                            }

                            let _ = item_sender.send(ItemInternal {
                                path: rel_path.clone(),
                                name: CompactString::from(entry.name.as_str()),
                                size,
                                is_dir: entry.is_dir,
                            });

                            // 渐进式流式传输
                            if let Some(app) = app_handle_for_worker.as_ref() {
                                stream_batch.push(Item {
                                    path: rel_path,
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
    let file_count = file_entries.len();
    let dir_count = internal_items.iter().filter(|i| i.is_dir).count();
    
    let file_entries_vec: Vec<(CompactString, i64)> = file_entries
        .iter()
        .map(|entry| (entry.key().clone(), *entry.value()))
        .collect();

    let actual_total_size: i64 = file_entries_vec.iter().map(|(_, size)| *size).sum();
    
    // 计算 I/O 吞吐量
    let throughput_mbps = if scan_phase.as_secs_f64() > 0.0 {
        (actual_total_size as f64 / 1024.0 / 1024.0) / scan_phase.as_secs_f64()
    } else {
        0.0
    };

    let dir_sizes = Arc::new(dashmap::DashMap::with_capacity_and_hasher(
        (file_count / 4).max(64),
        ahash::RandomState::new(),
    ));

    file_entries_vec.par_iter().for_each(|(file_path, file_size)| {
        let mut pos = 0;
        while let Some(slash_pos) = file_path[pos..].find('/') {
            let abs_pos = pos + slash_pos;
            let parent_path = &file_path[..abs_pos];
            dir_sizes
                .entry(CompactString::from(parent_path))
                .and_modify(|v| *v += file_size)
                .or_insert(*file_size);
            pos = abs_pos + 1;
        }
        dir_sizes
            .entry(CompactString::new())
            .and_modify(|v| *v += file_size)
            .or_insert(*file_size);
    });

    let dir_sizes: HashMap<CompactString, i64> = dir_sizes
        .iter()
        .map(|entry| (entry.key().clone(), *entry.value()))
        .collect();

    let compute_phase = compute_start.elapsed();
    let format_start = std::time::Instant::now();

    let mut items_vec: Vec<Item> = internal_items
        .into_par_iter()
        .map(|internal| {
            let size = if internal.is_dir {
                dir_sizes.get(&internal.path).copied().unwrap_or(0)
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
    
    // 估算内存使用
    let memory_peak_mb = (
        items_vec.capacity() * std::mem::size_of::<Item>() +
        file_count * std::mem::size_of::<(CompactString, i64)>() +
        dir_sizes.capacity() * std::mem::size_of::<(CompactString, i64)>()
    ) as f64 / 1024.0 / 1024.0;
    
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
    })
}

struct ItemInternal {
    path: CompactString,
    name: CompactString,
    size: i64,
    is_dir: bool,
}

#[inline]
fn normalize_path_separator(path: &std::ffi::OsStr) -> String {
    let s = path.to_string_lossy();
    if s.contains('\\') {
        s.replace('\\', "/")
    } else {
        s.into_owned()
    }
}

#[inline]
fn normalize_path_separator_compact(path: &std::ffi::OsStr) -> CompactString {
    let s = path.to_string_lossy();
    if s.contains('\\') {
        CompactString::from(s.replace('\\', "/"))
    } else {
        CompactString::from(s.as_ref())
    }
}

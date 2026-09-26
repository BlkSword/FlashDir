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
use std::sync::{Arc, OnceLock};
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

/// 扫描阶段事件载荷（用于状态栏展示当前阶段）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanPhasePayload {
    pub phase: String,
    pub message: String,
    pub progress: Option<f64>,
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
    /// 修改时间（Unix 秒级时间戳，0 = 未知）
    pub mtime: i64,
    /// 访问时间（Unix 秒级时间戳，0 = 未知；系统可能延迟更新，仅作热度参考）
    pub atime: i64,
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

/// 扫描结果条目的存储形态：可能直接共享缓存中的 `Arc`（零拷贝），
/// 也可能是本次扫描/推导出的独立 `Vec`。
pub enum ScanItems {
    Shared(Arc<Vec<Item>>),
    Owned(Vec<Item>),
}

impl ScanItems {
    #[inline]
    pub fn as_slice(&self) -> &[Item] {
        match self {
            ScanItems::Shared(items) => items.as_slice(),
            ScanItems::Owned(items) => items.as_slice(),
        }
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.as_slice().len()
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.as_slice().is_empty()
    }

    /// 需要独占条目时再克隆（例如保存快照、CLI 输出等场景）。
    /// 注意这里不用 `Arc::unwrap_or_clone`（1.76 起才稳定），保持 README 声明的 MSRV。
    pub fn into_owned(self) -> Vec<Item> {
        match self {
            ScanItems::Shared(items) => match Arc::try_unwrap(items) {
                Ok(items) => items,
                Err(shared) => (*shared).clone(),
            },
            ScanItems::Owned(items) => items,
        }
    }
}

/// `scan_directory` 的"视图"返回值：命中缓存时共享 `Arc`，不再深拷贝整份 items。
/// 调用方按需 `as_slice()` 使用，或 `into_scan_result()` 取得独占结果。
pub struct ScanView {
    pub items: ScanItems,
    pub total_size: i64,
    pub scan_time: f64,
    pub path: CompactString,
    pub mft_available: bool,
    pub timing: Option<TimingInfo>,
    /// "memory" / "disk" / "memory-derived" / "disk-derived" / "usn" / "usn-unchanged"
    pub cache_source: Option<String>,
    pub cache_read_time_ms: u64,
    pub cache_hit: bool,
    pub perf_metrics: Option<ScanPerfMetrics>,
}

impl ScanView {
    #[inline]
    pub fn items(&self) -> &[Item] {
        self.items.as_slice()
    }

    /// 由独立扫描结果构造视图（不额外拷贝）
    pub fn from_result(result: ScanResult, cache_source: &str) -> Self {
        Self {
            total_size: result.total_size,
            scan_time: result.scan_time,
            path: result.path,
            mft_available: result.mft_available,
            timing: result.timing,
            items: ScanItems::Owned(result.items),
            cache_source: Some(cache_source.to_string()),
            cache_read_time_ms: 0,
            cache_hit: true,
            perf_metrics: result.perf_metrics,
        }
    }

    /// 由缓存条目构造视图（共享 Arc，零拷贝）
    pub fn from_arc(arc: &ArcScanResult, cache_source: &str, cache_read_time_ms: u64) -> Self {
        Self {
            items: ScanItems::Shared(Arc::clone(&arc.items)),
            total_size: arc.total_size,
            scan_time: 0.0,
            path: CompactString::from(arc.path.as_ref()),
            mft_available: arc.mft_available,
            timing: arc.timing.clone(),
            cache_source: Some(cache_source.to_string()),
            cache_read_time_ms,
            cache_hit: true,
            perf_metrics: None,
        }
    }

    /// 转为独占的 `ScanResult`（共享来源会深拷贝；仅在确实需要时调用）
    pub fn into_scan_result(self) -> ScanResult {
        ScanResult {
            items: self.items.into_owned(),
            total_size: self.total_size,
            total_size_formatted: format_size(self.total_size),
            scan_time: self.scan_time,
            path: self.path,
            mft_available: self.mft_available,
            timing: self.timing,
            perf_metrics: self.perf_metrics,
        }
    }
}

impl From<ScanResult> for ArcScanResult {
    fn from(result: ScanResult) -> Self {
        Self {
            items: Arc::new(result.items),
            total_size: result.total_size,
            total_size_formatted: Arc::from(result.total_size_formatted.as_str()),
            scan_time: result.scan_time,
            path: Arc::from(result.path.as_str()),
            mft_available: result.mft_available,
            timing: result.timing,
        }
    }
}

#[derive(Clone)]
pub struct CacheEntry {
    pub result: ArcScanResult,
    /// 缓存写入时间（用于与目录 mtime 比较判断是否可能过期）
    pub dir_mtime: chrono::DateTime<chrono::Local>,
    /// 该目录数据已被 USN 增量校验到的位置（0 = 未知，不能走增量）
    pub verified_usn: i64,
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

    /// 从已缓存的上层目录结果推导子目录扫描结果。
    ///
    /// 上层目录扫描已经包含全量绝对路径，因此子目录只需做一次内存过滤，无需再次访问磁盘。
    /// 例如扫描过 `C:/Users` 后，扫描 `C:/Users/wfshenm` 可以直接从缓存中切出。
    ///
    /// 新鲜度约束：仅当"父缓存写入时间 >= 子目录自身 mtime"时才允许推导，
    /// 否则子目录可能在上层扫描之后发生过变更，必须走完整的缓存/USN/扫描链路。
    pub fn get_derived(
        &self,
        child_path: &str,
        child_mtime: chrono::DateTime<chrono::Local>,
    ) -> Option<(ScanResult, i64)> {
        let mut cache = self.cache.lock();

        // 找到能覆盖 child_path 的最深祖先缓存
        let mut best_key: Option<String> = None;
        for key in cache.iter().map(|(k, _)| k) {
            if key.len() < child_path.len() && is_same_or_child(key, child_path) {
                if best_key.as_deref().map_or(true, |k| key.len() > k.len()) {
                    best_key = Some(key.clone());
                }
            }
        }

        let parent = cache.get(best_key.as_ref()?)?;
        if parent.dir_mtime < child_mtime {
            return None;
        }
        let verified_usn = parent.verified_usn;
        let parent_items = parent.result.items.as_ref();

        // 子目录前缀：C:/Users/wfshenm -> C:/Users/wfshenm/
        let child_prefix = {
            let trimmed = child_path.trim_end_matches('/');
            if trimmed.is_empty() {
                "/".to_string()
            } else {
                format!("{}/", trimmed)
            }
        };

        // 子目录本身在父结果中作为一个目录条目存在；没有它说明缓存无法推导
        let child_dir_exists = parent_items.iter().any(|i| i.is_dir && i.path == child_path);
        if !child_dir_exists {
            return None;
        }

        let items: Vec<Item> = parent_items
            .iter()
            .filter(|i| i.path.starts_with(&child_prefix))
            .cloned()
            .collect();

        let total_size: i64 = items.iter().filter(|i| !i.is_dir).map(|i| i.size).sum();
        let file_count = items.iter().filter(|i| !i.is_dir).count();
        let dir_count = items.len() - file_count;

        Some((
            ScanResult {
                items,
                total_size,
                total_size_formatted: format_size(total_size),
                scan_time: 0.0,
                path: CompactString::from(child_path),
                mft_available: parent.result.mft_available,
                timing: None,
                perf_metrics: Some(ScanPerfMetrics {
                    io_phase_ms: 0,
                    compute_phase_ms: 0,
                    serialize_phase_ms: 0,
                    cache_read_time_ms: 0,
                    files_scanned: file_count,
                    dirs_scanned: dir_count,
                    io_throughput_mbps: 0.0,
                    memory_peak_mb: 0.0,
                    threads_used: 0,
                    cache_hit: true,
                    cache_source: Some("memory-derived".to_string()),
                }),
            },
            verified_usn,
        ))
    }

    /// 写入缓存并记录"数据已校验到的 USN"（0 = 未知，不可用于增量）
    pub fn insert_with_usn(&self, path: String, result: ScanResult, verified_usn: i64) {
        let arc_result = ArcScanResult::from(result);
        self.put(path, arc_result, verified_usn);
    }

    /// 直接共享已有 `Arc` 写入缓存（避免首次扫描后再全量克隆一次）
    pub fn insert_arc(&self, path: String, arc_result: ArcScanResult, verified_usn: i64) {
        self.put(path, arc_result, verified_usn);
    }

    fn put(&self, path: String, arc_result: ArcScanResult, verified_usn: i64) {
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
                verified_usn,
                size: entry_size,
            },
        );
    }

    /// USN 校验通过后刷新有效期与已校验 USN，避免为了刷新时间戳重写整份缓存
    pub fn touch(&self, path: &str, verified_usn: i64) {
        let mut cache = self.cache.lock();
        if let Some(entry) = cache.get_mut(path) {
            entry.dir_mtime = chrono::Local::now();
            if verified_usn > 0 {
                entry.verified_usn = verified_usn;
            }
        }
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
            .filter(|(k, _)| is_same_or_child(path, k))
            .map(|(k, _)| k.clone())
            .collect();
        for key in keys_to_remove {
            cache.pop(&key);
        }
    }

    /// 返回当前内存缓存统计：(条目数, 估算字节数)
    pub fn stats(&self) -> (usize, usize) {
        let cache = self.cache.lock();
        let entries = cache.len();
        let bytes = cache.iter().map(|(_, e)| e.size).sum();
        (entries, bytes)
    }
}

/// 供 Tauri command 读取内存缓存统计。
pub fn memory_cache_stats() -> (usize, usize) {
    scan_cache().stats()
}

/// 大小写不敏感相等比较。
/// Windows 文件名大小写不敏感，但 `eq_ignore_ascii_case` 只处理 ASCII；
/// 非 ASCII 场景回退到 Unicode 小写比较（少数情况下才发生分配）。
fn eq_ignore_case(a: &str, b: &str) -> bool {
    if a.eq_ignore_ascii_case(b) {
        return true;
    }
    if a.is_ascii() && b.is_ascii() {
        return false;
    }
    a.to_lowercase() == b.to_lowercase()
}

/// 大小写不敏感前缀比较（ASCII 零分配快速路径 + 非 ASCII 回退）
fn starts_with_ignore_case(path: &str, prefix: &str) -> bool {
    if prefix.is_empty() {
        return true;
    }
    if path
        .get(..prefix.len())
        .is_some_and(|head| head.eq_ignore_ascii_case(prefix))
    {
        return true;
    }
    if path.is_ascii() && prefix.is_ascii() {
        return false;
    }
    path.to_lowercase().starts_with(&prefix.to_lowercase())
}

/// 判断 `key` 是否为 `base` 本身或其子路径（统一按正斜杠规范化后比较）。
fn is_same_or_child(base: &str, key: &str) -> bool {
    if eq_ignore_case(key, base) {
        return true;
    }
    let child_prefix = if base.ends_with('/') {
        base.to_string()
    } else {
        format!("{}/", base)
    };
    starts_with_ignore_case(key, &child_prefix)
}

const SIZE_UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];

static SCAN_CACHE: OnceLock<ScanCache> = OnceLock::new();

fn scan_cache() -> &'static ScanCache {
    SCAN_CACHE.get_or_init(|| ScanCache::new(30, 200))
}

/// 释放内存扫描缓存，返回释放的目录数。
///
/// 全盘索引重建这类重活前调用：缓存（上限 200MB）与重活叠加很容易把低内存
/// 机器打满，而缓存随时可以用磁盘缓存/重扫重建，不值得为它冒闪退的风险。
pub fn clear_memory_cache() -> usize {
    let mut cache = scan_cache().cache.lock();
    let count = cache.len();
    cache.clear();
    count
}

/// 磁盘缓存后台写入任务
enum CacheWriteJob {
    Write {
        path: String,
        items: Arc<Vec<Item>>,
        mft_available: bool,
        dir_mtime: i64,
        verified_usn: i64,
    },
    Flush(std::sync::mpsc::Sender<()>),
}

/// 磁盘缓存写入线程（单线程、FIFO）：
/// 整份扫描结果的落盘可能耗时数秒到数十秒（大目录几十万行），
/// 放到后台线程后 GUI 扫描可以立即返回；同一路径的多次写入按提交顺序生效。
fn cache_writer() -> &'static std::sync::mpsc::Sender<CacheWriteJob> {
    static TX: OnceLock<std::sync::mpsc::Sender<CacheWriteJob>> = OnceLock::new();
    TX.get_or_init(|| {
        let (tx, rx) = std::sync::mpsc::channel::<CacheWriteJob>();
        std::thread::Builder::new()
            .name("flashdir-cache-writer".to_string())
            .spawn(move || {
                for job in rx {
                    match job {
                        CacheWriteJob::Write {
                            path,
                            items,
                            mft_available,
                            dir_mtime,
                            verified_usn,
                        } => {
                            if let Err(e) = DiskCache::instance().insert(
                                &path,
                                items.as_slice(),
                                mft_available,
                                dir_mtime,
                                verified_usn,
                            ) {
                                eprintln!("[Cache] 写入磁盘缓存失败 {}: {}", path, e);
                            }
                        }
                        CacheWriteJob::Flush(ack) => {
                            let _ = ack.send(());
                        }
                    }
                }
            })
            .expect("spawn flashdir-cache-writer");
        tx
    })
}

/// 提交一次整份磁盘缓存写入（异步）
pub fn schedule_disk_cache_write(
    path: String,
    items: Arc<Vec<Item>>,
    mft_available: bool,
    dir_mtime: i64,
    verified_usn: i64,
) {
    let _ = cache_writer().send(CacheWriteJob::Write {
        path,
        items,
        mft_available,
        dir_mtime,
        verified_usn,
    });
}

/// 等待已提交的磁盘缓存写入全部完成（CLI 退出前调用；GUI 不需要）
pub fn flush_disk_cache_writes() {
    let (ack_tx, ack_rx) = std::sync::mpsc::channel();
    if cache_writer().send(CacheWriteJob::Flush(ack_tx)).is_ok() {
        let _ = ack_rx.recv_timeout(std::time::Duration::from_secs(600));
    }
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
    scan_cache().get(&key).map(|e| Arc::clone(&e.result.items))
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

fn emit_scan_phase(app_handle: &Option<tauri::AppHandle>, phase: &str, message: &str, progress: Option<f64>) {
    if let Some(handle) = app_handle {
        let _ = handle.emit(
            "scan-phase",
            ScanPhasePayload {
                phase: phase.to_string(),
                message: message.to_string(),
                progress,
            },
        );
    }
}

/// 主扫描入口：返回独占的 `ScanResult`。
/// 只读场景（分页、目录树、开发者分析）请优先使用 `scan_directory_view`，
/// 命中缓存时不会深拷贝整份条目列表。
pub async fn scan_directory(
    path: &str,
    force_refresh: bool,
    perf_monitor: Arc<PerformanceMonitor>,
    app_handle: Option<tauri::AppHandle>,
) -> Result<ScanResult, anyhow::Error> {
    Ok(scan_directory_view(path, force_refresh, perf_monitor, app_handle)
        .await?
        .into_scan_result())
}

/// 主扫描函数（零拷贝视图版）。
///
/// 返回 `ScanView`：命中内存缓存 / USN 校验通过时直接共享 `Arc<Vec<Item>>`。
///
/// 新鲜度策略：
/// 1. 只要该目录缓存记录了"已校验 USN"，就先做 USN 增量校验 ——
///    目录 mtime 只在直接增删子项时变化，无法反映文件内容修改，
///    只有 USN 能捕捉这类变更；
/// 2. USN 不可用 / 增量窗口失效时，退回 mtime 缓存与上层推导逻辑；
/// 3. 增量窗口失效（Journal 回滚等）时直接全量扫描，避免返回陈旧数据。
pub async fn scan_directory_view(
    path: &str,
    force_refresh: bool,
    perf_monitor: Arc<PerformanceMonitor>,
    app_handle: Option<tauri::AppHandle>,
) -> Result<ScanView, anyhow::Error> {
    let _scan_id = perf_monitor.start_scan(path);
    let start_time = std::time::Instant::now();

    // 领取本次扫描的取消代号：不会清除其它在飞扫描的取消请求
    let scan_id = crate::cancel::begin();

    if crate::cancel::is_requested_for(scan_id) {
        perf_monitor.add_error("扫描已取消".to_string());
        perf_monitor.end_scan();
        return Err(anyhow::anyhow!("扫描已取消"));
    }

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

    if crate::cancel::is_requested_for(scan_id) {
        perf_monitor.add_error("扫描已取消".to_string());
        perf_monitor.end_scan();
        return Err(anyhow::anyhow!("扫描已取消"));
    }

    let mtime = match metadata.modified() {
        Ok(m) => m,
        Err(_) => std::time::SystemTime::UNIX_EPOCH,
    };
    let mtime_datetime: chrono::DateTime<chrono::Local> = mtime.into();
    let mtime_timestamp = mtime_datetime.timestamp();

    emit_scan_phase(&app_handle, "preparing", "准备扫描", None);

    // USN 判定"缓存不可信"时置位：必须跳过下面所有缓存分支，直接全量扫描
    let mut force_full_scan = false;

    // ── 0. USN 增量优先（唯一能捕捉"文件内容变化 / 深层变化"的机制） ──
    if !force_refresh {
        let usn_start = std::time::Instant::now();
        if let Some(base) = load_usn_base(&root_dir) {
            if base.verified_usn == 0 {
                eprintln!(
                    "[USN] {} 的缓存没有已校验 USN（非 MFT 扫描或检查点缺失），退回 mtime 新鲜度",
                    root_dir
                );
            }
            if base.verified_usn > 0 {
                emit_scan_phase(&app_handle, "usn", "USN 增量校验", None);
                match try_usn_incremental_update(
                    &root_dir,
                    base.items(),
                    base.verified_usn,
                    scan_id,
                    &perf_monitor,
                ) {
                    Some(UsnUpdate::Unchanged { verified_usn: next }) => {
                        let cache_read_time = usn_start.elapsed().as_millis() as u64;
                        perf_monitor.record_cache_hit(cache_read_time);
                        // 校验通过：只刷新有效期与已校验 USN，不重写条目
                        scan_cache().touch(&root_dir, next);
                        DiskCache::instance()
                            .touch_meta(&root_dir, mtime_timestamp, next)
                            .ok();
                        perf_monitor.end_scan();
                        let cache_source = if base.from_disk { "usn-disk" } else { "usn" };
                        return Ok(base.into_view(&root_dir, cache_source));
                    }
                    Some(UsnUpdate::Applied {
                        items,
                        verified_usn: next,
                    }) => {
                        let cache_read_time = usn_start.elapsed().as_millis() as u64;
                        perf_monitor.record_cache_hit(cache_read_time);
                        let total_size: i64 =
                            items.iter().filter(|i| !i.is_dir).map(|i| i.size).sum();
                        let arc_result = ArcScanResult {
                            items: Arc::new(items),
                            total_size,
                            total_size_formatted: Arc::from(format_size(total_size).as_str()),
                            scan_time: 0.0,
                            path: Arc::from(root_dir.as_str()),
                            mft_available: true,
                            timing: None,
                        };
                        // blob 缓存：整块重写（后台线程），内存缓存同步更新
                        scan_cache().insert_arc(root_dir.clone(), arc_result.clone(), next);
                        schedule_disk_cache_write(
                            root_dir.clone(),
                            Arc::clone(&arc_result.items),
                            true,
                            mtime_timestamp,
                            next,
                        );
                        perf_monitor.end_scan();
                        return Ok(ScanView::from_arc(&arc_result, "usn", cache_read_time));
                    }
                    Some(UsnUpdate::Stale) => {
                        eprintln!("[USN] 增量窗口失效，转为全量扫描: {}", root_dir);
                        force_full_scan = true;
                    }
                    None => {}
                }
            }
        }
    }

    // ── 1. 缓存：内存 -> 上层推导 -> 磁盘推导 -> 磁盘 ──
    if !force_refresh && !force_full_scan {
        let cache_check_start = std::time::Instant::now();
        if let Some(cached) = scan_cache().get(&root_dir) {
            // 如果缓存来自目录遍历，但当前进程是管理员且 MFT 可用，
            // 则放弃缓存并重新扫描，以升级到 MFT 快速路径。
            let can_upgrade_to_mft = !cached.result.mft_available
                && cfg!(target_os = "windows")
                && crate::fs::is_admin()
                && crate::fs::check_mft_available(&root_dir);

            if cached.dir_mtime >= mtime_datetime && !can_upgrade_to_mft {
                emit_scan_phase(&app_handle, "cache-hit-memory", "内存缓存命中", None);
                let cache_read_time = cache_check_start.elapsed().as_millis() as u64;
                perf_monitor.record_cache_hit(cache_read_time);
                perf_monitor.end_scan();
                // 零拷贝：直接共享缓存中的 Arc
                return Ok(ScanView::from_arc(&cached.result, "memory", cache_read_time));
            } else if can_upgrade_to_mft {
                eprintln!(
                    "[Scan] 管理员+MFT 可用，放弃旧缓存并重新扫描以启用 MFT: {}",
                    root_dir
                );
            }
        }

        // 1.5 从上层目录缓存推导子目录结果（要求父缓存写入时间 >= 子目录 mtime）
        if let Some((derived, derived_usn)) = scan_cache().get_derived(&root_dir, mtime_datetime) {
            emit_scan_phase(&app_handle, "cache-hit-derived", "从上层缓存推导", None);
            let cache_read_time = cache_check_start.elapsed().as_millis() as u64;
            perf_monitor.record_cache_hit(cache_read_time);

            // 把推导结果写入内存缓存，后续再次扫描该子目录可直接命中
            scan_cache().insert_with_usn(root_dir.clone(), derived.clone(), derived_usn);

            perf_monitor.end_scan();
            return Ok(ScanView::from_result(derived, "memory-derived"));
        }

        // 1.8 从磁盘上层缓存推导子目录结果（条目级 SQL 前缀查询，无需反序列化整个父目录）
        let disk_cache = DiskCache::instance();
        if let Some((derived, derived_usn)) = disk_cache.get_derived(&root_dir, mtime_timestamp) {
            emit_scan_phase(&app_handle, "cache-hit-disk-derived", "磁盘上层缓存推导", None);
            let cache_read_time = cache_check_start.elapsed().as_millis() as u64;
            perf_monitor.record_cache_hit(cache_read_time);

            scan_cache().insert_with_usn(root_dir.clone(), derived.clone(), derived_usn);

            perf_monitor.end_scan();
            return Ok(ScanView::from_result(derived, "disk-derived"));
        }

        // 2. 检查磁盘缓存
        if let Some((cached_result, cached_usn)) =
            disk_cache.get_with_usn(&root_dir, mtime_timestamp)
        {
            let can_upgrade_to_mft = !cached_result.mft_available
                && cfg!(target_os = "windows")
                && crate::fs::is_admin()
                && crate::fs::check_mft_available(&root_dir);

            if !can_upgrade_to_mft {
                emit_scan_phase(&app_handle, "cache-hit-disk", "磁盘缓存命中", None);
                let cache_read_time = cache_check_start.elapsed().as_millis() as u64;
                perf_monitor.record_cache_hit(cache_read_time);

                // 同时写入内存缓存（保留已校验 USN）
                scan_cache().insert_with_usn(root_dir.clone(), cached_result.clone(), cached_usn);

                perf_monitor.end_scan();
                return Ok(ScanView::from_result(cached_result, "disk"));
            } else {
                eprintln!(
                    "[Scan] 管理员+MFT 可用，放弃磁盘缓存并重新扫描以启用 MFT: {}",
                    root_dir
                );
            }
        }
    }

    // 没有可用缓存（或需要全量刷新）：失效内存缓存后执行全量扫描。
    // 磁盘缓存不做级联失效：每行 blob 自带 dir_mtime + 已校验 USN，能独立判断新鲜度；
    // 而级联失效在扫 C:/ 这类根路径时会把所有子目录缓存一起删掉
    //（实测扫 C:/ 后 C:/Windows、C:/Users 的 blob 全被清空）。
    scan_cache().invalidate(&root_dir);

    // ── 2. 全量扫描：MFT 直读（Everything 式快速路径）失败则回退目录遍历 ──
    let canonical_path_clone = canonical_path.clone();
    let perf_monitor_for_blocking = Arc::clone(&perf_monitor);
    let app_handle_for_blocking = app_handle.clone().map(Arc::new);

    emit_scan_phase(&app_handle, "mft", "MFT 直接读取", None);

    if crate::cancel::is_requested_for(scan_id) {
        perf_monitor.add_error("扫描已取消".to_string());
        perf_monitor.end_scan();
        return Err(anyhow::anyhow!("扫描已取消"));
    }

    let mft_result = try_mft_scan_path(
        &canonical_path_clone,
        &root_dir,
        scan_id,
        &perf_monitor_for_blocking,
        app_handle_for_blocking.as_ref(),
    );

    let output = match mft_result {
        Some(mft_output) => {
            if crate::cancel::is_requested_for(scan_id) {
                perf_monitor.add_error("扫描已取消".to_string());
                perf_monitor.end_scan();
                return Err(anyhow::anyhow!("扫描已取消"));
            }
            emit_scan_phase(&app_handle, "aggregating", "聚合目录大小", None);
            mft_output
        }
        None => {
            emit_scan_phase(&app_handle, "walking", "目录遍历", None);
            tokio::task::spawn_blocking(move || {
                scan_directory_optimized_v4(
                    &canonical_path_clone,
                    scan_id,
                    &perf_monitor_for_blocking,
                    app_handle_for_blocking,
                )
            })
            .await??
        }
    };

    let scan_time = start_time.elapsed().as_secs_f64();
    let total_size = output.total_size;
    let mft_available = output.mft_available;
    let timing = output.timing;
    let verified_usn = output.verified_usn;

    let perf_metrics = Some(ScanPerfMetrics {
        io_phase_ms: (timing.scan_phase * 1000.0) as u64,
        compute_phase_ms: (timing.compute_phase * 1000.0) as u64,
        serialize_phase_ms: (timing.format_phase * 1000.0) as u64,
        cache_read_time_ms: 0,
        files_scanned: output.file_count,
        dirs_scanned: output.dir_count,
        io_throughput_mbps: output.throughput_mbps,
        memory_peak_mb: output.memory_peak_mb,
        threads_used: output.threads_used,
        cache_hit: false,
        cache_source: None,
    });

    let arc_result = ArcScanResult {
        items: Arc::new(output.items),
        total_size,
        total_size_formatted: Arc::from(format_size(total_size).as_str()),
        scan_time,
        path: Arc::from(path),
        mft_available,
        timing: Some(timing.clone()),
    };

    // 内存缓存同步写入（后续分页/树/USN 校验都依赖它，共享 Arc 零拷贝）；
    // 磁盘缓存整份落盘较慢（大目录数十秒），交给后台线程，扫描立即返回。
    scan_cache().insert_arc(root_dir.clone(), arc_result.clone(), verified_usn);
    schedule_disk_cache_write(
        root_dir.clone(),
        Arc::clone(&arc_result.items),
        mft_available,
        mtime_timestamp,
        verified_usn,
    );

    perf_monitor.end_scan();
    Ok(ScanView {
        items: ScanItems::Shared(Arc::clone(&arc_result.items)),
        total_size,
        scan_time,
        path: CompactString::from(path),
        mft_available,
        timing: Some(timing),
        cache_source: None,
        cache_read_time_ms: 0,
        cache_hit: false,
        perf_metrics,
    })
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
    /// 本次扫描结束时该卷的 USN 位置（0 = 未知/MFT 不可用）。
    /// 作为该目录缓存的"已校验 USN"，下次扫描据此做增量校验。
    verified_usn: i64,
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

/// 高效聚合目录大小。
///
/// 旧实现为每个文件沿路径向上遍历所有祖先目录，复杂度 O(文件数 × 平均深度)。
/// 新实现：
/// 1. 每个文件只累加到直接父目录；
/// 2. 再把每个目录的累计值按“深度从深到浅”传播到其父目录。
/// 复杂度约为 O(文件数 + 目录数)，避免深路径导致聚合阶段卡顿。
fn aggregate_directory_sizes(items: &mut Vec<Item>) -> i64 {
    let n = items.len();
    let dir_index: HashMap<&str, usize> = items
        .iter()
        .enumerate()
        .filter(|(_, it)| it.is_dir)
        .map(|(i, it)| (it.path.as_str(), i))
        .collect();

    let mut dir_sizes = vec![0i64; n];
    let mut dir_parent = vec![None; n];
    let mut dir_indices = Vec::with_capacity(dir_index.len());

    for (i, item) in items.iter().enumerate() {
        if item.is_dir {
            dir_indices.push(i);
            if let Some(slash) = item.path.rfind('/') {
                if let Some(&pidx) = dir_index.get(&item.path[..slash]) {
                    dir_parent[i] = Some(pidx);
                }
            }
        } else if item.size > 0 {
            // 只累加到直接父目录，后续通过目录传播完成祖先聚合
            if let Some(slash) = item.path.rfind('/') {
                if let Some(&pidx) = dir_index.get(&item.path[..slash]) {
                    dir_sizes[pidx] += item.size;
                }
            }
        }
    }

    // 按目录深度从深到浅传播，确保子目录先累加完成再传给父目录
    dir_indices.sort_unstable_by_key(|&i| std::cmp::Reverse(items[i].path.matches('/').count()));

    for &i in &dir_indices {
        if let Some(p) = dir_parent[i] {
            dir_sizes[p] += dir_sizes[i];
        }
    }

    let total_size: i64 = items
        .iter()
        .filter(|i| !i.is_dir)
        .map(|i| i.size)
        .sum();

    // 此时不再需要借用 items 的 dir_index，可以安全地按下标回写
    for &i in &dir_indices {
        items[i].size = dir_sizes[i];
    }

    // 不再为每个条目预格式化 size_formatted（几十万次 format! + 分配）：
    // 展示层按需格式化（前端有 formatSize 回退，后端分页/树只格式化返回的少量条目）。
    total_size
}

/// 尝试使用 MFT 直接读取扫描（Everything 式快速路径）
/// 仅在 Windows + 管理员权限 + NTFS 卷上生效
/// 返回 None 表示不可用，调用者应回退到目录遍历
fn try_mft_scan_path(
    canonical_path: &Path,
    _root_dir: &str,
    scan_id: u64,
    perf_monitor: &Arc<PerformanceMonitor>,
    _app_handle: Option<&Arc<tauri::AppHandle>>,
) -> Option<ScanOutput> {
    if is_mft_disabled() {
        return None;
    }

    let root_path_str = canonical_path.to_string_lossy().to_string();
    let (drive, vol_prefix) = drive_and_vol_prefix(&root_path_str)?;

    // 在读取 MFT 之前先记录该卷的 USN 位置。
    // 这样"扫描过程中"发生的变更会在下次增量校验中被重新应用（写入是幂等的：
    // 删除不存在的项是 no-op、创建/改名/大小都以当前 MFT 记录为准），
    // 而不会因为"快照时间早于检查点"被永久跳过。
    let pre_scan_checkpoint = crate::fs::get_checkpoint(drive);
    if pre_scan_checkpoint.is_none() {
        eprintln!(
            "[USN] 未能获取扫描前的 Journal 状态（卷忙/权限/Journal 不可用），该目录本次不记录已校验 USN，将退回 mtime 新鲜度"
        );
    }

    // 尝试 MFT 全卷扫描（复用调用方的取消代号）
    let mft_result = crate::fs::try_mft_scan_with_cancel(&root_path_str, scan_id)?;

    let total_start = std::time::Instant::now();

    perf_monitor.start_io_phase();
    let scan_start = std::time::Instant::now();

    // 过滤：只保留目标目录下的文件
    // MFT 返回的路径是 volume-relative（不带盘符），需用 volume-relative 前缀匹配
    let normalized_root = vol_prefix;

    let mut items: Vec<Item> = mft_result
        .files
        .into_iter()
        .filter(|f| starts_with_ignore_case(&f.path, &normalized_root))
        .map(|f| Item {
            path: mft_path_to_abs(drive, &f.path),
            name: CompactString::from(f.name),
            size: f.size as i64,
            size_formatted: CompactString::new(), // 下面统一格式化
            is_dir: f.is_dir,
            mtime: f.mtime,
            atime: f.atime,
        })
        .collect();

    let file_count = items.iter().filter(|i| !i.is_dir).count();
    let dir_count = items.iter().filter(|i| i.is_dir).count();

    let scan_phase = scan_start.elapsed();
    perf_monitor.end_io_phase();

    // 计算目录大小（聚合子文件大小到父目录）
    perf_monitor.start_compute_phase();
    let compute_start = std::time::Instant::now();

    let actual_total_size = aggregate_directory_sizes(&mut items);
    let compute_phase = compute_start.elapsed();

    // 按大小降序排序
    items.sort_unstable_by(|a, b| b.size.cmp(&a.size));

    let format_phase = compute_start.elapsed(); // approximate
    let total = total_start.elapsed();
    perf_monitor.end_compute_phase();

    let throughput_mbps = if scan_phase.as_secs_f64() > 0.0 {
        (actual_total_size as f64 / 1024.0 / 1024.0) / scan_phase.as_secs_f64()
    } else {
        0.0
    };

    let memory_peak_mb = (items.capacity() * std::mem::size_of::<Item>()) as f64 / 1024.0 / 1024.0;

    perf_monitor.update_memory_stats(memory_peak_mb, memory_peak_mb);
    perf_monitor.update_io_stats(file_count, dir_count, actual_total_size as u64, file_count + dir_count);

    eprintln!(
        "[MFT] 扫描完成: {} 文件, {} 目录, {:.2}s (filtered from {} total)",
        file_count,
        dir_count,
        total.as_secs_f64(),
        mft_result.file_count + mft_result.dir_count
    );

    // 保存 USN 检查点，并把"开始扫描前的 USN 位置"写入缓存元数据，
    // 供下次扫描据此做增量校验
    let verified_usn = save_usn_checkpoint(&root_path_str, pre_scan_checkpoint);

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
        verified_usn,
    })
}

// ─── USN Journal 增量更新 ───────────────────────────────────

/// 保存 USN 检查点，返回该目录数据"已校验到的 USN"（0 = 不可用）。
///
/// `checkpoint` 必须是**开始读取文件系统之前**取得的 Journal 状态：
/// 只有"校验点不晚于快照时间"，增量窗口才不会漏掉扫描期间的变更。
#[cfg(target_os = "windows")]
fn save_usn_checkpoint(
    path: &str,
    checkpoint: Option<crate::fs::UsnCheckpoint>,
) -> i64 {
    let Some(drive) = crate::fs::extract_drive_letter(path) else {
        return 0;
    };
    let Some(checkpoint) = checkpoint else {
        // 拿不到扫描前的 Journal 状态：不做任何"已校验"断言，
        // 该目录退化为按 mtime 判断新鲜度
        return 0;
    };

    let checkpoint_path = usn_checkpoint_path(drive);
    if let Some(parent) = checkpoint_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string(&checkpoint) {
        let _ = write_usn_checkpoint_atomic(&checkpoint_path, &json);
        eprintln!(
            "[USN] 检查点已保存: {}.{} (next_usn={})",
            drive, checkpoint.journal_id, checkpoint.next_usn
        );
    }
    checkpoint.next_usn
}

#[cfg(not(target_os = "windows"))]
fn save_usn_checkpoint(
    _path: &str,
    _checkpoint: Option<crate::fs::UsnCheckpoint>,
) -> i64 {
    0
}

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

/// 原子写入 USN 检查点：先写临时文件再 rename，避免检查点文件损坏。
#[cfg(target_os = "windows")]
fn write_usn_checkpoint_atomic(path: &std::path::Path, json: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp_path = path.with_extension("json.tmp");
    std::fs::write(&tmp_path, json)?;
    std::fs::rename(&tmp_path, path)?;
    Ok(())
}

/// USN 增量检查的结果
pub enum UsnUpdate {
    /// 增量窗口覆盖了缓存数据，且区间内没有影响该目录的变更
    Unchanged { verified_usn: i64 },
    /// 变更已应用，返回新的条目集合
    Applied { items: Vec<Item>, verified_usn: i64 },
    /// 增量窗口已失效（Journal 回滚/校验点过旧），必须全量扫描
    Stale,
}

/// USN 增量检查的基底：优先内存缓存（共享 Arc），否则读磁盘缓存
pub struct UsnBase {
    items: ScanItems,
    total_size: i64,
    mft_available: bool,
    timing: Option<TimingInfo>,
    verified_usn: i64,
    pub from_disk: bool,
    cache_read_time_ms: u64,
}

impl UsnBase {
    pub fn items(&self) -> &[Item] {
        self.items.as_slice()
    }

    pub fn into_view(self, root_dir: &str, cache_source: &str) -> ScanView {
        ScanView {
            items: self.items,
            total_size: self.total_size,
            scan_time: 0.0,
            path: CompactString::from(root_dir),
            mft_available: self.mft_available,
            timing: self.timing,
            cache_source: Some(cache_source.to_string()),
            cache_read_time_ms: self.cache_read_time_ms,
            cache_hit: true,
            perf_metrics: None,
        }
    }
}

/// 选择 USN 增量检查的基底。
/// 只有缓存元数据里记录了非零 `verified_usn` 才值得做增量：
/// verified_usn = 0 表示这份缓存不是由 MFT 全量扫描产生（例如目录遍历），
/// 无法断定它已覆盖 Journal 的哪一段，只能走常规缓存/全量扫描。
fn load_usn_base(root_dir: &str) -> Option<UsnBase> {
    if let Some(entry) = scan_cache().get(root_dir) {
        return Some(UsnBase {
            items: ScanItems::Shared(Arc::clone(&entry.result.items)),
            total_size: entry.result.total_size,
            mft_available: entry.result.mft_available,
            timing: entry.result.timing.clone(),
            verified_usn: entry.verified_usn,
            from_disk: false,
            cache_read_time_ms: 0,
        });
    }

    let verified = DiskCache::instance().verified_usn(root_dir)?;
    if verified <= 0 {
        return None;
    }
    let started = std::time::Instant::now();
    let (result, verified_usn) = DiskCache::instance().get_stale_with_usn(root_dir)?;
    Some(UsnBase {
        items: ScanItems::Owned(result.items),
        total_size: result.total_size,
        mft_available: result.mft_available,
        timing: result.timing,
        verified_usn,
        from_disk: true,
        cache_read_time_ms: started.elapsed().as_millis() as u64,
    })
}

/// 重写检查点：把"已消费到的 USN"推进到 next_usn
#[cfg(target_os = "windows")]
fn update_usn_checkpoint(
    cp_path: &std::path::Path,
    checkpoint: &crate::fs::UsnCheckpoint,
    next_usn: i64,
) {
    let updated = crate::fs::UsnCheckpoint {
        created_at: chrono::Utc::now().timestamp(),
        // 已消费位置单调前进
        next_usn: next_usn.max(checkpoint.next_usn),
        ..checkpoint.clone()
    };
    if let Ok(json) = serde_json::to_string(&updated) {
        let _ = write_usn_checkpoint_atomic(cp_path, &json);
    }
}

/// 使用 USN Journal 对某目录缓存做增量校验。
///
/// `base_items` / `verified_usn` 来自该目录缓存元数据。返回：
/// - `Unchanged`：区间内没有影响该目录的变更，缓存可信；
/// - `Applied`：返回应用变更后的新条目集合；
/// - `Stale`：增量窗口失效，调用方应直接全量扫描；
/// - `None`：增量不可用（无检查点、I/O 失败等），调用方退回常规缓存逻辑。
#[cfg(target_os = "windows")]
fn try_usn_incremental_update(
    root_dir: &str,
    base_items: &[Item],
    verified_usn: i64,
    scan_id: u64,
    _perf_monitor: &Arc<PerformanceMonitor>,
) -> Option<UsnUpdate> {
    if verified_usn <= 0 {
        return None;
    }
    let drive = crate::fs::extract_drive_letter(root_dir)?;

    // 读取检查点（用于校验 Journal ID / 卷序列号）
    let cp_path = usn_checkpoint_path(drive);
    let checkpoint: crate::fs::UsnCheckpoint = {
        let data = match std::fs::read_to_string(&cp_path) {
            Ok(data) => data,
            Err(e) => {
                eprintln!("[USN] 读取检查点失败 {}: {}（退回常规缓存逻辑）", cp_path.display(), e);
                return None;
            }
        };
        match serde_json::from_str(&data) {
            Ok(cp) => cp,
            Err(e) => {
                eprintln!("[USN] 解析检查点失败 {}: {}（退回常规缓存逻辑）", cp_path.display(), e);
                return None;
            }
        }
    };

    let delta = match crate::fs::read_incremental_changes(drive, &checkpoint, verified_usn) {
        Ok(delta) => delta,
        Err(crate::fs::UsnReadError::JournalReset)
        | Err(crate::fs::UsnReadError::VolumeChanged)
        | Err(crate::fs::UsnReadError::WindowExpired) => return Some(UsnUpdate::Stale),
        Err(crate::fs::UsnReadError::Io(e)) => {
            // 记录格式不受支持（例如未来版本 USN_RECORD）时不能"假装没变更"，
            // 必须走全量扫描；其它 I/O 错误才退回常规缓存逻辑。
            if e.kind() == std::io::ErrorKind::InvalidData {
                eprintln!("[USN] Journal 记录格式不支持，转为全量扫描: {}", e);
                return Some(UsnUpdate::Stale);
            }
            eprintln!("[USN] 增量读取失败，退回常规缓存逻辑: {}", e);
            return None;
        }
    };

    // Journal 中可读的最早 USN 已经高于校验点：中间变更不可得
    if verified_usn < delta.lowest_valid_usn {
        eprintln!(
            "[USN] 校验点 {} 早于 Journal 最早可读 {}，转为全量扫描",
            verified_usn, delta.lowest_valid_usn
        );
        return Some(UsnUpdate::Stale);
    }

    // 变更过多：增量已不划算，全量扫描更可靠
    if delta.changes.len() > crate::fs::MAX_USN_CHANGES {
        eprintln!("[USN] 变更过多 ({})，转为全量扫描", delta.changes.len());
        return Some(UsnUpdate::Stale);
    }

    let next_usn = delta.next_usn;

    // 被扫描目录的 volume-relative 前缀（用于过滤其它目录的变更）
    let root_rel = {
        let without_drive = if root_dir.len() >= 2 && root_dir.as_bytes().get(1) == Some(&b':') {
            &root_dir[2..]
        } else {
            root_dir
        };
        without_drive
            .trim_start_matches('/')
            .trim_end_matches('/')
            .to_string()
    };
    let in_scope = |vol_path: &str| -> bool {
        root_rel.is_empty() || is_same_or_child(&root_rel, vol_path)
    };

    let relevant: Vec<&crate::fs::UsnChangeRecord> = delta
        .changes
        .iter()
        .filter(|c| {
            c.reason
                & (crate::fs::USN_REASON_FILE_DELETE
                    | crate::fs::USN_REASON_RENAME_OLD_NAME
                    | crate::fs::USN_REASON_FILE_CREATE
                    | crate::fs::USN_REASON_RENAME_NEW_NAME
                    | crate::fs::USN_REASON_DATA_OVERWRITE
                    | crate::fs::USN_REASON_DATA_EXTEND
                    | crate::fs::USN_REASON_DATA_TRUNCATION)
                != 0
        })
        .collect();

    if relevant.is_empty() {
        // 区间内没有相关变更：缓存数据可信，只需推进校验点
        update_usn_checkpoint(&cp_path, &checkpoint, next_usn);
        eprintln!(
            "[USN] 无相关变更 (共 {} 条，已推进到 USN {})",
            delta.changes.len(),
            next_usn
        );
        return Some(UsnUpdate::Unchanged { verified_usn: next_usn });
    }

    // ── 以下开始真正应用变更 ──
    // 构建可变的条目索引（只在确实存在变更时才克隆整份条目）
    let mut items_map: HashMap<CompactString, Item> =
        HashMap::with_capacity(base_items.len() + relevant.len());
    for item in base_items {
        items_map.insert(item.path.clone(), item.clone());
    }

    let mut scanner = match crate::fs::MftScanner::open(drive) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("[USN] 无法打开 MFT 扫描器: {}", e);
            return None;
        }
    };
    scanner.set_cancel_id(scan_id);

    // (FRN, 记录 USN) → volume-relative 路径缓存
    // （同一父目录在不同 USN 下可能对应不同历史名称，因此缓存键带上 USN）
    let mut parent_path_cache: HashMap<(u64, i64), Option<String>> = HashMap::new();

    // FILETIME（1601 起 100ns）→ Unix 秒
    let filetime_to_unix = |ft: i64| -> i64 { (ft - 116444736000000000) / 10_000_000 };

    // 实际改动了多少条（用于判断"只有范围外变更"时不必重写缓存）
    let mut applied = 0usize;

    // 目录改名时暂存其子树（key = FRN），等 NEW_NAME 记录出现后按新前缀重挂。
    // 否则改名后整棵子树会从缓存中消失（Windows 只为目录本身生成 USN 记录）。
    let mut renamed_subtrees: HashMap<u64, (String, Vec<Item>)> = HashMap::new();

    // 窗口内的改名历史：FRN -> [(改名发生时的 USN, 旧名)]（按 USN 升序）。
    // 作用：USN 记录只带"父目录 FRN + 自己的名字"，而 FRN→路径解析读的是
    // **当前** MFT 名称；若父目录在同一窗口内被改名，早期记录会解析到新路径而
    // 匹配不上缓存（典型表现：目录改名前的删除记录被静默丢弃）。
    // 解析祖先路径时，对"改名 USN 晚于当前记录"的节点回退使用旧名即可还原当时路径。
    let mut rename_history: HashMap<u64, Vec<(i64, String)>> = HashMap::new();
    for change in &relevant {
        if change.reason & crate::fs::USN_REASON_RENAME_OLD_NAME != 0 {
            rename_history
                .entry(change.file_ref)
                .or_default()
                .push((change.usn, change.name.clone()));
        }
    }

    // 每个 FRN 上"删除 / 改名旧名"的最大 USN。
    // 同一批次里"先创建后删除"（临时文件很常见）或"连续改名"时，
    // 较早的创建/改名新名记录已被后续记录取代，不能再写入缓存，否则留下幽灵条目。
    let mut superseded_frn_usn: HashMap<u64, i64> = HashMap::new();
    for change in &relevant {
        let supersedes = change.reason
            & (crate::fs::USN_REASON_FILE_DELETE | crate::fs::USN_REASON_RENAME_OLD_NAME)
            != 0;
        if supersedes {
            let slot = superseded_frn_usn
                .entry(change.file_ref)
                .or_insert(change.usn);
            if change.usn > *slot {
                *slot = change.usn;
            }
        }
    }

    // ── Phase 1：删除 / 重命名旧名（先移除） ──
    let mut removed_abs_paths: Vec<String> = Vec::new();
    for change in &relevant {
        let is_delete = change.reason & crate::fs::USN_REASON_FILE_DELETE != 0;
        let is_rename_old = change.reason & crate::fs::USN_REASON_RENAME_OLD_NAME != 0;
        if !is_delete && !is_rename_old {
            continue;
        }
        if is_delete {
            // "目录改名后又被删除"：子树保持在删除状态，不再需要重挂
            renamed_subtrees.remove(&change.file_ref);
        }
        let Some(parent_path) = resolve_parent_path_at(
            &scanner,
            &mut parent_path_cache,
            change.parent_ref,
            change.usn,
            &rename_history,
        ) else {
            continue;
        };
        let vol_path = if parent_path.is_empty() {
            change.name.clone()
        } else {
            format!("{}/{}", parent_path, change.name)
        };
        if !in_scope(&vol_path) {
            continue;
        }
        let abs = crate::global_search::normalize_abs_path(drive, &vol_path);
        // 缓存 key 统一为绝对路径；同时兼容旧版本写入的相对路径。
        // 只做查找、不立即删除：目录还需要连带处理整棵子树。
        let abs_key = CompactString::from(abs.as_str());
        let rel_key = CompactString::from(vol_path.as_str());
        let hit_key = if items_map.contains_key(&abs_key) {
            Some(abs_key.clone())
        } else if items_map.contains_key(&rel_key) {
            Some(rel_key.clone())
        } else {
            None
        };

        if let Some(key) = hit_key {
            let is_dir = items_map.get(&key).map(|item| item.is_dir).unwrap_or(false);
            items_map.remove(&key);
            applied += 1;
            eprintln!(
                "  [USN-{}] 移除: {}",
                if is_delete { "DEL" } else { "RN_OLD" },
                key
            );

            if is_dir {
                // 目录（删除或改名）：整棵子树的路径都会变化/失效
                let prefix = format!("{}/", key);
                let mut children: Vec<CompactString> = items_map
                    .keys()
                    .filter(|k| k.as_str().starts_with(prefix.as_str()))
                    .cloned()
                    .collect();
                children.sort_unstable();

                let mut subtree: Vec<Item> = Vec::with_capacity(children.len());
                for child in children {
                    if let Some(item) = items_map.remove(&child) {
                        // 全局索引同样要移除这些路径（改名时会在新前缀下重新 upsert）
                        removed_abs_paths.push(child.to_string());
                        subtree.push(item);
                    }
                }
                if is_rename_old {
                    // 改名：暂存子树，等 NEW_NAME 记录出现后按新前缀重挂
                    renamed_subtrees.insert(change.file_ref, (key.to_string(), subtree));
                }
                // 真删除：子树不再重挂（blob 整块重写时会一并消失）
            }
        }
        removed_abs_paths.push(abs);
    }

    if !removed_abs_paths.is_empty() {
        crate::global_search::instance().remove_paths_batch(&removed_abs_paths);
        let _ = DiskCache::instance().remove_global_index_by_paths(&removed_abs_paths);
    }

    // ── Phase 2：创建 / 重命名新名 / 数据变化（后写入） ──
    let mut upserted_entries: Vec<crate::global_search::IndexEntry> = Vec::new();
    for change in &relevant {
        let is_create = change.reason & crate::fs::USN_REASON_FILE_CREATE != 0;
        let is_rename_new = change.reason & crate::fs::USN_REASON_RENAME_NEW_NAME != 0;
        let is_data_change = change.reason
            & (crate::fs::USN_REASON_DATA_OVERWRITE
                | crate::fs::USN_REASON_DATA_EXTEND
                | crate::fs::USN_REASON_DATA_TRUNCATION)
            != 0;
        if !is_create && !is_rename_new && !is_data_change {
            continue;
        }
        let Some(parent_path) = resolve_parent_path_at(
            &scanner,
            &mut parent_path_cache,
            change.parent_ref,
            change.usn,
            &rename_history,
        ) else {
            continue;
        };
        let vol_path = if parent_path.is_empty() {
            change.name.clone()
        } else {
            format!("{}/{}", parent_path, change.name)
        };
        if !in_scope(&vol_path) {
            continue;
        }
        let abs = crate::global_search::normalize_abs_path(drive, &vol_path);
        let item_key = CompactString::from(abs.as_str());
        let mtime = filetime_to_unix(change.timestamp);

        if is_create || is_rename_new {
            if superseded_frn_usn
                .get(&change.file_ref)
                .is_some_and(|later_usn| *later_usn > change.usn)
            {
                eprintln!(
                    "  [USN] 跳过已被后续记录取代的创建/改名: {}",
                    change.name
                );
                continue;
            }
            // 从 MFT 读取真实大小 / 目录标志 / 修改时间
            let (file_size, is_dir, file_mtime, file_atime) =
                match scanner.read_single_record(change.file_ref) {
                    Ok(Some(record)) => (
                        record.real_size as i64,
                        record.is_dir,
                        record.mtime,
                        record.atime,
                    ),
                    _ => (0i64, (change.attributes & 0x10) != 0, mtime, 0),
                };
            applied += 1;
            let new_item = Item {
                path: CompactString::from(abs.as_str()),
                name: CompactString::from(change.name.as_str()),
                size: file_size,
                size_formatted: format_size(file_size),
                is_dir,
                mtime: file_mtime,
                atime: file_atime,
            };
            items_map.insert(item_key, new_item);

            // 目录改名：把暂存的子树改挂到新前缀下（Windows 不会为子项生成 USN 记录）
            if is_dir && is_rename_new {
                if let Some((old_prefix, subtree)) = renamed_subtrees.remove(&change.file_ref) {
                    for mut child in subtree {
                        let child_path = child.path.as_str().to_string();
                        if let Some(rest) = child_path.strip_prefix(old_prefix.as_str()) {
                            child.path = CompactString::from(format!("{}{}", abs, rest).as_str());
                            let name = child.name.to_string();
                            upserted_entries.push(crate::global_search::IndexEntry {
                                path: child.path.to_string(),
                                name_lower: name.to_lowercase(),
                                size: child.size,
                                is_dir: child.is_dir,
                                mtime: child.mtime,
                            });
                            items_map.insert(child.path.clone(), child);
                        }
                    }
                }
            }
            upserted_entries.push(crate::global_search::IndexEntry {
                path: abs.clone(),
                name_lower: change.name.to_lowercase(),
                size: file_size,
                is_dir,
                mtime: file_mtime,
            });
            eprintln!(
                "  [USN-{}] 添加: {} ({} bytes, dir={})",
                if is_create { "CREATE" } else { "RN_NEW" },
                abs,
                file_size,
                is_dir
            );
        } else if let Some(item) = items_map.get_mut(&item_key) {
            if !item.is_dir {
                if let Ok(Some(record)) = scanner.read_single_record(change.file_ref) {
                    let new_size = record.real_size as i64;
                    if new_size != item.size {
                        eprintln!(
                            "  [USN-DATA] 更新大小: {} {} -> {} bytes",
                            abs, item.size, new_size
                        );
                        applied += 1;
                        item.size = new_size;
                        item.size_formatted = format_size(new_size);
                        item.mtime = record.mtime;
                        let name = item.name.to_string();
                        upserted_entries.push(crate::global_search::IndexEntry {
                            path: abs.clone(),
                            name_lower: name.to_lowercase(),
                            size: new_size,
                            is_dir: false,
                            mtime: record.mtime,
                        });
                    }
                }
            }
        }
    }

    if !upserted_entries.is_empty() {
        let _ = DiskCache::instance().upsert_global_index_entries(&upserted_entries);
        crate::global_search::instance().upsert_batch(upserted_entries);
    }

    drop(scanner);
    drop(parent_path_cache);

    // 目录改名批次里只出现了旧名（新名记录被批次边界截断）：子树已从缓存移除，
    // 继续增量会留下不完整的结果 —— 直接要求全量扫描，绝不写回不一致的缓存。
    if !renamed_subtrees.is_empty() {
        eprintln!(
            "[USN] 有 {} 个目录改名缺少新名记录，转为全量扫描",
            renamed_subtrees.len()
        );
        return Some(UsnUpdate::Stale);
    }

    // 变更都在扫描目录之外：缓存本身无需重写，只推进校验点
    if applied == 0 {
        update_usn_checkpoint(&cp_path, &checkpoint, next_usn);
        return Some(UsnUpdate::Unchanged { verified_usn: next_usn });
    }

    // ── 重新聚合目录大小并排序 ──
    let mut new_items: Vec<Item> = items_map.into_values().collect();
    let actual_total_size = aggregate_directory_sizes(&mut new_items);
    new_items.sort_unstable_by(|a, b| b.size.cmp(&a.size));

    eprintln!(
        "[USN] 增量更新完成: {} 项, {} ({} 条相关变更, next_usn={})",
        new_items.len(),
        format_size(actual_total_size),
        relevant.len(),
        next_usn
    );

    update_usn_checkpoint(&cp_path, &checkpoint, next_usn);

    Some(UsnUpdate::Applied {
        items: new_items,
        verified_usn: next_usn,
    })
}

/// FRN → volume-relative 路径（带缓存），并还原"记录发生时"的祖先名称。
#[cfg(target_os = "windows")]
fn resolve_parent_path_at(
    scanner: &crate::fs::MftScanner,
    cache: &mut HashMap<(u64, i64), Option<String>>,
    parent_ref: u64,
    at_usn: i64,
    rename_history: &HashMap<u64, Vec<(i64, String)>>,
) -> Option<String> {
    let key = (parent_ref, at_usn);
    if let Some(cached) = cache.get(&key) {
        return cached.clone();
    }
    let resolved = resolve_path_walk(scanner, parent_ref, at_usn, rename_history);
    cache.insert(key, resolved.clone());
    resolved
}

/// 从 FRN 向上走到卷根，逐级取"该记录时间点"的名字
#[cfg(target_os = "windows")]
fn resolve_path_walk(
    scanner: &crate::fs::MftScanner,
    frn: u64,
    at_usn: i64,
    rename_history: &HashMap<u64, Vec<(i64, String)>>,
) -> Option<String> {
    const ROOT_FRN: u64 = 5;
    const MAX_DEPTH: u32 = 64;

    if frn == ROOT_FRN {
        return Some(String::new());
    }

    let mut components: Vec<String> = Vec::new();
    let mut current = frn;
    let mut depth = 0u32;
    loop {
        if depth > MAX_DEPTH {
            return None;
        }
        depth += 1;

        let record = scanner.read_single_record(current).ok().flatten()?;
        components.push(historical_name(
            current,
            record.name.as_str(),
            at_usn,
            rename_history,
        ));
        current = record.parent_frn;
        if current == ROOT_FRN {
            break;
        }
    }

    components.reverse();
    Some(components.join("/"))
}

/// 该 FRN 在 `at_usn` 时刻的名字：
/// 若窗口内存在"改名 USN 晚于 at_usn"的事件，说明 MFT 里已是改名后的名字，
/// 应回退到那次改名之前的旧名（取最早的那次）。
#[cfg(target_os = "windows")]
fn historical_name(
    frn: u64,
    mft_name: &str,
    at_usn: i64,
    rename_history: &HashMap<u64, Vec<(i64, String)>>,
) -> String {
    if let Some(events) = rename_history.get(&frn) {
        for (usn, old_name) in events {
            if *usn > at_usn {
                return old_name.clone();
            }
        }
    }
    mft_name.to_string()
}

#[cfg(not(target_os = "windows"))]
fn try_usn_incremental_update(
    _root_dir: &str,
    _base_items: &[Item],
    _verified_usn: i64,
    _scan_id: u64,
    _perf_monitor: &Arc<PerformanceMonitor>,
) -> Option<UsnUpdate> {
    None
}


/// 优化的扫描实现 v4
/// 集成：性能监控、内存优化、Windows 原生 I/O
fn scan_directory_optimized_v4(
    root_path: &Path,
    scan_id: u64,
    perf_monitor: &Arc<PerformanceMonitor>,
    _app_handle: Option<Arc<tauri::AppHandle>>,
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

    let cancelled = Arc::new(AtomicBool::new(false));
    let skipped_dirs = Arc::new(std::sync::atomic::AtomicUsize::new(0));

    pool.scope(|s| {
        for _ in 0..num_threads {
            let dir_sender = dir_sender.clone();
            let dir_receiver = dir_receiver.clone();
            let item_sender = item_sender.clone();
            let cancelled_clone = Arc::clone(&cancelled);
            let skipped_dirs = Arc::clone(&skipped_dirs);

            s.spawn(move |_| {
                let mut idle_count = 0;

                loop {
                    if crate::cancel::is_requested_for(scan_id) {
                        cancelled_clone.store(true, Ordering::Relaxed);
                        break;
                    }

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
                    // 注意：读取失败的目录不能静默忽略，否则结果会少目录且用户无感知
                    match crate::fs::read_dir_entries(&dir_path) {
                        Ok(entries) => {
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
                                path: abs_path,
                                name: CompactString::from(entry.name.as_str()),
                                size,
                                is_dir: entry.is_dir,
                                mtime: entry.mtime,
                                atime: entry.atime,
                            });
                        }
                        }
                        Err(e) => {
                            skipped_dirs.fetch_add(1, Ordering::Relaxed);
                            eprintln!(
                                "[Scan] 跳过无法读取的目录 {}: {}",
                                dir_path.display(),
                                e
                            );
                        }
                    }
                }
            });
        }
    });

    drop(item_sender);
    drop(dir_sender);

    let skipped_dirs = skipped_dirs.load(Ordering::Relaxed);
    if skipped_dirs > 0 {
        eprintln!("[Scan] 本次扫描共跳过 {} 个无法读取的目录", skipped_dirs);
    }

    if cancelled.load(Ordering::Relaxed) || crate::cancel::is_requested_for(scan_id) {
        perf_monitor.add_error("扫描已取消".to_string());
        return Err(anyhow::anyhow!("扫描已取消"));
    }

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
                mtime: internal.mtime,
                atime: internal.atime,
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
        // 目录遍历（非管理员）无法读取 USN Journal：
        // verified_usn 保持 0，表示"未校验"，下次不会走增量
        verified_usn: 0,
    })
}

struct ItemInternal {
    path: CompactString,
    name: CompactString,
    size: i64,
    is_dir: bool,
    mtime: i64,
    atime: i64,
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

    /// 合成数据基准：目录大小聚合 / 排序 / format_size 的代价。
    /// 运行：cargo test --release --lib -- --ignored --nocapture bench_aggregate
    #[test]
    #[ignore = "benchmark: 合成 40 万条目"]
    fn bench_aggregate_and_format() {
        use std::time::Instant;

        // 30 万文件 + 10 万目录，深度 6，模拟 C 盘规模
        let mut items: Vec<Item> = Vec::with_capacity(400_000);
        for d in 0..100_000 {
            let p = format!("C:/root/d{}/d{}/d{}", d % 100, d % 1000, d);
            items.push(Item {
                path: CompactString::from(p),
                name: CompactString::from(format!("d{}", d)),
                size: 0,
                size_formatted: CompactString::new(),
                is_dir: true,
                mtime: 0,
                atime: 0,
            });
        }
        for f in 0..300_000 {
            let p = format!("C:/root/d{}/d{}/d{}/f{}.dat", f % 100, f % 1000, f % 100, f);
            items.push(Item {
                path: CompactString::from(p),
                name: CompactString::from(format!("f{}.dat", f)),
                size: 1024 + (f % 4096) as i64,
                size_formatted: CompactString::new(),
                is_dir: false,
                mtime: 0,
                atime: 0,
            });
        }

        let t = Instant::now();
        let total = aggregate_directory_sizes(&mut items);
        eprintln!(
            "[bench] 聚合目录大小({} 条, 总 {}): {:?}",
            items.len(),
            total,
            t.elapsed()
        );

        let t = Instant::now();
        items.sort_unstable_by(|a, b| b.size.cmp(&a.size));
        eprintln!("[bench] 按大小排序: {:?}", t.elapsed());

        let t = Instant::now();
        let mut acc = 0usize;
        for i in 0..items.len() {
            acc += format_size(items[i].size).len();
        }
        eprintln!("[bench] format_size x{}: {:?} (acc={})", items.len(), t.elapsed(), acc);
    }

    /// 基准：磁盘缓存 blob 的反序列化成本拆解
    /// 运行：cargo test --release --lib -- --ignored --nocapture bench_blob_decode
    #[test]
    #[ignore = "benchmark: 需要真实 blob 缓存"]
    fn bench_blob_decode() {
        use std::time::Instant;

        let t = Instant::now();
        let blob: Vec<u8> = match DiskCache::instance().raw_largest_blob_for_bench() {
            Some(b) => b,
            None => {
                eprintln!("[bench] 无 blob 缓存，跳过");
                return;
            }
        };
        eprintln!(
            "[bench] SQL 读取 blob {:.1} MB: {:?}",
            blob.len() as f64 / 1024.0 / 1024.0,
            t.elapsed()
        );

        // 完整加载路径（fetch + decode + 内存排序）
        if let Some(path) = DiskCache::instance().largest_blob_path_for_bench() {
            let t = Instant::now();
            let loaded = DiskCache::instance().get_stale_with_usn(&path);
            eprintln!(
                "[bench] get_stale_with_usn({}) 完整加载: {:?} ({:?} 条)",
                path,
                t.elapsed(),
                loaded.as_ref().map(|(r, _)| r.items.len())
            );
        }

        // 1) 当前路径：bincode 反序列化整份 Vec<Item>（path+name+size_formatted）
        let t = Instant::now();
        let items: Vec<Item> = bincode::deserialize(&blob).unwrap();
        let full = t.elapsed();
        eprintln!("[bench] bincode 反序列化 {} 条: {:?}", items.len(), full);
        drop(items);

        // 说明：实测瓶颈在 SQLite 读取 blob（页缓存路径 ~900ms/147MB），
        // 而非反序列化本身（~350ms/76 万条）。读取已改用增量 Blob API 直读。
    }

    #[test]
    fn test_aggregate_directory_sizes() {
        let mut items = vec![
            Item { path: CompactString::from("C:/a"), name: CompactString::from("a"), size: 0, size_formatted: CompactString::new(), is_dir: true, mtime: 0, atime: 0 },
            Item { path: CompactString::from("C:/a/b"), name: CompactString::from("b"), size: 0, size_formatted: CompactString::new(), is_dir: true, mtime: 0, atime: 0 },
            Item { path: CompactString::from("C:/a/b/f"), name: CompactString::from("f"), size: 10, size_formatted: CompactString::new(), is_dir: false, mtime: 0, atime: 0 },
            Item { path: CompactString::from("C:/a/f2"), name: CompactString::from("f2"), size: 5, size_formatted: CompactString::new(), is_dir: false, mtime: 0, atime: 0 },
        ];
        let total = aggregate_directory_sizes(&mut items);
        assert_eq!(total, 15);
        assert_eq!(items[0].size, 15);
        assert_eq!(items[1].size, 10);
    }
}

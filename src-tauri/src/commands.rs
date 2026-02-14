// 命令处理器 - 优化版
// 集成性能监控、磁盘缓存、二进制协议

use crate::scan::{self, HistoryItem, HistoryItemSummary, ScanResult};
use crate::perf::{PerformanceMonitor, ScanMetrics};
use crate::disk_cache::DiskCache;
use crate::binary_protocol::{OptimizedScanResult, BinaryPayload};
use crate::AppState;
use chrono::Utc;
use std::collections::VecDeque;
use tauri::{command, State};
use std::path::PathBuf;
use tokio::{fs, io::AsyncWriteExt};

fn get_history_file_path() -> Result<PathBuf, String> {
    let home_dir = std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .map_err(|_| "无法获取用户目录")?;

    let mut path = PathBuf::from(home_dir);
    path.push(".flashdir");
    path.push("history.json");
    Ok(path)
}

pub fn load_history_from_file_sync() -> VecDeque<HistoryItem> {
    match get_history_file_path() {
        Ok(path) => {
            if path.exists() {
                match std::fs::read_to_string(&path) {
                    Ok(content) => {
                        match serde_json::from_str::<VecDeque<HistoryItem>>(&content) {
                            Ok(history) => history,
                            Err(_) => {
                                #[derive(serde::Deserialize)]
                                struct OldHistoryItem {
                                    path: String,
                                    #[serde(with = "chrono::serde::ts_seconds")]
                                    scan_time: chrono::DateTime<chrono::Utc>,
                                    total_size: i64,
                                    size_format: String,
                                    items: Vec<scan::Item>,
                                }

                                let old_history: Vec<OldHistoryItem> =
                                    serde_json::from_str(&content).unwrap_or_default();

                                old_history.into_iter().map(|old| HistoryItem {
                                    path: smartstring::SmartString::from(old.path),
                                    scan_time: old.scan_time,
                                    total_size: old.total_size,
                                    size_format: smartstring::SmartString::from(old.size_format),
                                    item_count: old.items.len(),
                                }).collect()
                            }
                        }
                    }
                    Err(_) => VecDeque::new()
                }
            } else {
                if let Some(parent) = path.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                VecDeque::new()
            }
        }
        Err(_) => VecDeque::new()
    }
}

async fn save_history_to_file_async(history: &VecDeque<HistoryItem>) -> Result<(), String> {
    let path = get_history_file_path()?;

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .await
            .map_err(|e| format!("创建目录失败: {}", e))?;
    }

    let json = serde_json::to_string(history)
        .map_err(|e| format!("序列化失败: {}", e))?;

    let mut file = fs::File::create(&path)
        .await
        .map_err(|e| format!("创建文件失败: {}", e))?;

    file.write_all(json.as_bytes())
        .await
        .map_err(|e| format!("写入文件失败: {}", e))?;

    file.sync_all()
        .await
        .map_err(|e| format!("同步文件失败: {}", e))?;

    Ok(())
}

/// 扫描目录 - 优化版
#[command]
pub async fn scan_directory(
    path: String,
    force_refresh: bool,
    state: State<'_, AppState>,
) -> Result<ScanResult, String> {
    let path = path.trim().to_string();

    if path.is_empty() {
        return Err("请提供有效的目录路径".to_string());
    }

    let perf_monitor = PerformanceMonitor::instance();

    match scan::scan_directory(&path, force_refresh, perf_monitor).await {
        Ok(result) => {
            let history_item = HistoryItem {
                path: smartstring::SmartString::from(path.clone()),
                scan_time: Utc::now(),
                total_size: result.total_size,
                size_format: smartstring::SmartString::from(result.total_size_formatted.as_str()),
                item_count: result.items.len(),
            };

            let mut history = state.history.lock();
            history.push_back(history_item);

            while history.len() > 20 {
                history.pop_front();
            }

            let history_for_save: VecDeque<HistoryItem> = history.clone();
            drop(history);

            tokio::spawn(async move {
                if let Err(e) = save_history_to_file_async(&history_for_save).await {
                    eprintln!("保存历史记录失败: {}", e);
                }
            });

            Ok(result)
        }
        Err(e) => Err(e.to_string()),
    }
}

/// 扫描目录 - 二进制格式返回（用于大数据）
#[command]
pub async fn scan_directory_binary(
    path: String,
    force_refresh: bool,
    state: State<'_, AppState>,
) -> Result<BinaryPayload, String> {
    let result = scan_directory(path, force_refresh, state).await?;
    
    // 转换为优化格式并序列化
    let optimized: OptimizedScanResult = result.into();
    
    BinaryPayload::from_data(&optimized, 1024 * 1024) // 1MB 压缩阈值
        .map_err(|e| format!("序列化失败: {}", e))
}

/// 批量扫描
#[command]
pub async fn scan_directories_batch(
    paths: Vec<String>,
    force_refresh: bool,
    state: State<'_, AppState>,
) -> Result<Vec<ScanResult>, String> {
    let mut results = Vec::with_capacity(paths.len());
    
    for path in paths {
        match scan_directory(path, force_refresh, state.clone()).await {
            Ok(result) => results.push(result),
            Err(e) => eprintln!("扫描失败: {}", e),
        }
    }
    
    Ok(results)
}

#[command]
pub fn get_history_summary(state: State<'_, AppState>) -> Vec<HistoryItemSummary> {
    let history = state.history.lock();
    let summaries: Vec<HistoryItemSummary> = history.iter().map(|item| item.into()).collect();
    summaries.into_iter().rev().collect()
}

#[command]
pub fn get_history(state: State<'_, AppState>) -> Vec<HistoryItem> {
    let history = state.history.lock();
    let mut result: Vec<_> = history.iter().cloned().collect();
    result.reverse();
    result
}

#[command]
pub async fn clear_history(state: State<'_, AppState>) -> Result<(), String> {
    {
        let mut history = state.history.lock();
        history.clear();
    }

    save_history_to_file_async(&VecDeque::new()).await
}

/// 获取性能指标
#[command]
pub fn get_performance_metrics() -> Option<ScanMetrics> {
    PerformanceMonitor::instance().get_current_metrics()
}

/// 获取性能历史
#[command]
pub fn get_performance_history() -> Vec<ScanMetrics> {
    PerformanceMonitor::instance().get_history()
}

/// 清除性能历史
#[command]
pub fn clear_performance_history() {
    PerformanceMonitor::instance().clear_history();
}

/// 获取性能摘要
#[command]
pub fn get_performance_summary() -> crate::perf::PerformanceSummary {
    PerformanceMonitor::instance().get_summary()
}

/// 获取磁盘缓存统计
#[command]
pub fn get_disk_cache_stats() -> crate::disk_cache::CacheStats {
    DiskCache::instance().get_stats()
}

/// 清除磁盘缓存
#[command]
pub fn clear_disk_cache() -> Result<(), String> {
    DiskCache::instance()
        .clear()
        .map_err(|e| format!("清除缓存失败: {}", e))
}

/// 获取内存缓存统计
#[command]
pub fn get_memory_cache_stats() -> MemoryCacheStats {
    // 返回内存缓存统计
    MemoryCacheStats {
        max_entries: 30,
        max_size_mb: 200,
        current_entries: 0, // 需要实现获取逻辑
        current_size_mb: 0.0,
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct MemoryCacheStats {
    pub max_entries: usize,
    pub max_size_mb: usize,
    pub current_entries: usize,
    pub current_size_mb: f64,
}

/// 获取系统信息
#[command]
pub fn get_system_info() -> SystemInfo {
    use sysinfo::{System, RefreshKind, CpuRefreshKind};

    let mut system = System::new_with_specifics(
        RefreshKind::new().with_cpu(CpuRefreshKind::everything())
    );
    system.refresh_all();

    let cpu_usage = system.global_cpu_info().cpu_usage();

    SystemInfo {
        cpu_count: num_cpus::get(),
        cpu_usage,
        memory_total_mb: system.total_memory() / 1024,
        memory_used_mb: system.used_memory() / 1024,
        os_name: System::name().unwrap_or_default(),
        os_version: System::os_version().unwrap_or_default(),
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct SystemInfo {
    pub cpu_count: usize,
    pub cpu_usage: f32,
    pub memory_total_mb: u64,
    pub memory_used_mb: u64,
    pub os_name: String,
    pub os_version: String,
}

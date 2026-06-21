// 命令处理器 - 优化版
// 集成性能监控、磁盘缓存、二进制协议

use flashdir::scan::{self, HistoryItem, HistoryItemSummary, ScanResult};
use flashdir::perf::{PerformanceMonitor, ScanMetrics};
use flashdir::disk_cache::DiskCache;
use crate::AppState;
use chrono::Utc;
use std::collections::VecDeque;
use tauri::{command, State, Emitter};
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

/// 扫描目录 - 优化版（支持渐进式流式传输）
#[command]
pub async fn scan_directory(
    path: String,
    force_refresh: bool,
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<ScanResult, String> {
    let path = path.trim().to_string();

    if path.is_empty() {
        return Err("请提供有效的目录路径".to_string());
    }

    let perf_monitor = PerformanceMonitor::instance();

    match scan::scan_directory(&path, force_refresh, perf_monitor, Some(app)).await {
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

/// 扫描目录 - 自定义紧凑二进制格式（经 Tauri 原始字节通道返回，避免 serde_json 序列化百万级 items）
#[command]
pub async fn scan_directory_binary(
    path: String,
    force_refresh: bool,
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<tauri::ipc::Response, String> {
    let result = scan_directory(path, force_refresh, app, state).await?;
    Ok(tauri::ipc::Response::new(scan::encode_scan_result(&result)))
}

/// 批量扫描
#[command]
pub async fn scan_directories_batch(
    paths: Vec<String>,
    force_refresh: bool,
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<Vec<ScanResult>, String> {
    let mut results = Vec::with_capacity(paths.len());

    for path in paths {
        match scan_directory(path, force_refresh, app.clone(), state.clone()).await {
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
pub fn get_performance_summary() -> flashdir::perf::PerformanceSummary {
    PerformanceMonitor::instance().get_summary()
}

/// 获取磁盘缓存统计
#[command]
pub fn get_disk_cache_stats() -> flashdir::disk_cache::CacheStats {
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

/// 使用系统默认程序打开文件或目录
#[command]
pub async fn open_path(app: tauri::AppHandle, path: String) -> Result<(), String> {
    use tauri_plugin_shell::ShellExt;

    let target = if path.starts_with("//?/") {
        // 将 canonicalize 风格路径转换回普通 Windows 路径
        path[4..].replace('/', "\\")
    } else {
        path.replace('/', "\\")
    };

    app.shell()
        .open(&target, None)
        .map_err(|e| format!("无法打开路径: {}", e))
}

/// 判断路径是否为目录
#[command]
pub async fn is_directory(path: String) -> Result<bool, String> {
    let p = if path.starts_with("//?/") {
        PathBuf::from(&path[4..].replace('/', "\\"))
    } else {
        PathBuf::from(&path.replace('/', "\\"))
    };

    match fs::metadata(&p).await {
        Ok(m) => Ok(m.is_dir()),
        Err(e) => Err(format!("无法访问路径: {}", e)),
    }
}

/// 检测当前进程是否以管理员/提升权限运行
#[command]
pub fn is_admin() -> bool {
    flashdir::fs::is_admin()
}

/// 检测 MFT 直接扫描是否可用（Windows 管理员权限）
#[command]
pub fn check_mft_available(path: String) -> bool {
    flashdir::fs::check_mft_available(&path)
}

/// 获取当前扫描环境状态（管理员 + 指定路径 MFT 可用性）
#[command]
pub fn get_scan_status(path: String) -> ScanStatus {
    ScanStatus {
        is_admin: flashdir::fs::is_admin(),
        mft_available: flashdir::fs::check_mft_available(&path),
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ScanStatus {
    pub is_admin: bool,
    pub mft_available: bool,
}

/// 以管理员权限重启应用
#[command]
pub fn restart_as_admin() -> bool {
    flashdir::fs::restart_as_admin()
}

/// 开发者磁盘分析：从内存缓存读取当前路径的扫描结果（避免百万级 items 跨 IPC 传输），
/// 识别并分类常见开发工具/缓存目录的空间占用（已按"匹配边界顶层"去重，杜绝重复累加）
#[command]
pub fn analyze_dev_disk(path: String) -> Option<flashdir::dev_analyzer::DevAnalysisResult> {
    let items = flashdir::scan::get_cached_items(&path)?;
    let total_size: i64 = items.iter().filter(|i| !i.is_dir).map(|i| i.size).sum();
    let total_items = items.len();
    Some(flashdir::dev_analyzer::analyze(&items, total_size, total_items))
}

// ─── 快照管理 ────────────────────────────────────────────

/// 保存当前扫描结果为快照
#[command]
pub fn save_snapshot(
    path: String,
    items: Vec<flashdir::scan::Item>,
    total_size: i64,
    total_size_formatted: String,
) -> Result<i64, String> {
    let result = flashdir::scan::ScanResult {
        items,
        total_size,
        total_size_formatted: flashdir::scan::CompactString::from(total_size_formatted.as_str()),
        scan_time: 0.0,
        path: flashdir::scan::CompactString::from(path.as_str()),
        mft_available: false,
        timing: None,
        perf_metrics: None,
    };

    let file_count = result.items.iter().filter(|i| !i.is_dir).count();
    let dir_count = result.items.iter().filter(|i| i.is_dir).count();

    flashdir::disk_cache::DiskCache::instance()
        .insert_snapshot(&path, &result, file_count, dir_count)
        .map_err(|e| format!("保存快照失败: {}", e))
}

/// 列出指定路径的所有快照
#[command]
pub fn list_snapshots(path: String) -> Result<Vec<flashdir::disk_cache::SnapshotInfo>, String> {
    flashdir::disk_cache::DiskCache::instance()
        .list_snapshots(&path)
        .map_err(|e| format!("获取快照列表失败: {}", e))
}

/// 比较两个快照（传入快照 ID）
#[command]
pub fn compare_snapshots(
    old_id: i64,
    new_id: i64,
) -> Result<flashdir::diff_engine::SnapshotDiff, String> {
    let disk_cache = flashdir::disk_cache::DiskCache::instance();

    let old_result = disk_cache
        .get_snapshot(old_id)
        .ok_or_else(|| format!("快照 {} 不存在", old_id))?;

    let new_result = disk_cache
        .get_snapshot(new_id)
        .ok_or_else(|| format!("快照 {} 不存在", new_id))?;

    Ok(flashdir::diff_engine::diff(
        &old_result.items,
        &new_result.items,
        old_result.total_size,
    ))
}

/// 删除指定快照
#[command]
pub fn delete_snapshot(id: i64) -> Result<(), String> {
    flashdir::disk_cache::DiskCache::instance()
        .delete_snapshot(id)
        .map_err(|e| format!("删除快照失败: {}", e))
}

/// 比较最新快照与当前扫描结果（用于增量增长分析）
#[command]
pub fn compare_with_latest_snapshot(
    path: String,
    current_items: Vec<flashdir::scan::Item>,
    _current_total_size: i64,
) -> Result<Option<flashdir::diff_engine::SnapshotDiff>, String> {
    let disk_cache = flashdir::disk_cache::DiskCache::instance();
    let snapshots = disk_cache
        .list_snapshots(&path)
        .map_err(|e| format!("获取快照列表失败: {}", e))?;

    if snapshots.is_empty() {
        return Ok(None);
    }

    // 取最新的快照
    let latest = &snapshots[0];
    let old_result = disk_cache
        .get_snapshot(latest.id)
        .ok_or_else(|| format!("快照 {} 不存在", latest.id))?;

    Ok(Some(flashdir::diff_engine::diff(
        &old_result.items,
        &current_items,
        old_result.total_size,
    )))
}

// ─── 全局文件搜索 ──────────────────────────────────────────

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GlobalSearchResponse {
    pub ready: bool,
    pub state: flashdir::global_search::IndexState,
    pub results: Vec<flashdir::global_search::IndexEntry>,
    /// 诊断：搜索无结果时返回索引实际条目数
    #[serde(skip_serializing_if = "Option::is_none")]
    pub index_size: Option<usize>,
    /// 诊断：搜索无结果时返回前几个索引条目名称(确认 name 字段是否正常)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sample_names: Option<Vec<String>>,
}

/// 查询全局索引状态
#[command]
pub fn global_search_status() -> flashdir::global_search::IndexState {
    flashdir::global_search::instance().state()
}

/// 构建全盘索引：逐盘调 scan_directory（与主界面相同的已验证路径，确保文件名正确）
#[command]
pub async fn global_search_ensure_index(app: tauri::AppHandle) -> Result<(), String> {
    {
        let idx = flashdir::global_search::instance();
        match idx.state() {
            flashdir::global_search::IndexState::Ready(..)
            | flashdir::global_search::IndexState::Loading { .. } => return Ok(()),
            _ => {}
        }
    }

    let idx = flashdir::global_search::instance();
    idx.set_loading();

    let drives = flashdir::global_search::list_ntfs_drives();
    if drives.is_empty() {
        idx.finish_building(&[]);
        return Err("未检测到可扫描的 NTFS 卷（需要管理员权限读取 MFT）".to_string());
    }

    let perf = flashdir::perf::PerformanceMonitor::instance();
    let mut ok_drives: Vec<char> = Vec::new();

    for &drive in &drives {
        let root = format!("{}:\\", drive);
        let _ = app.emit(
            "global-search-progress",
            serde_json::json!({ "drive": drive.to_string(), "scanned": 0, "phase": "scanning" }),
        );

        // 1) 内存缓存命中：毫秒级（之前扫过该盘）
        if let Some(cached) = flashdir::scan::get_cached_items(&root) {
            idx.append_scan(drive, &cached);
            ok_drives.push(drive);
            let _ = app.emit(
                "global-search-progress",
                serde_json::json!({ "drive": drive.to_string(), "scanned": cached.len(), "phase": "ok (cache)" }),
            );
            continue;
        }

        // 2) 轻量 MFT 扫描：仅取文件名/路径/大小，跳过聚合/format/排序（3-5s）
        if let Some(lite_items) = flashdir::scan::scan_lite(&root) {
            idx.append_scan(drive, &lite_items);
            ok_drives.push(drive);
            let _ = app.emit(
                "global-search-progress",
                serde_json::json!({ "drive": drive.to_string(), "scanned": lite_items.len(), "phase": "ok (lite)" }),
            );
            continue;
        }

        // 3) 完整 scan_directory（回退，同时写缓存供后续命中）
        match flashdir::scan::scan_directory(&root, false, std::sync::Arc::clone(&perf), Some(app.clone()))
            .await
        {
            Ok(result) => {
                idx.append_scan(drive, &result.items);
                ok_drives.push(drive);
                let _ = app.emit(
                    "global-search-progress",
                    serde_json::json!({ "drive": drive.to_string(), "scanned": result.items.len(), "phase": "ok" }),
                );
            }
            Err(e) => {
                let _ = app.emit(
                    "global-search-progress",
                    serde_json::json!({ "drive": drive.to_string(), "scanned": 0, "phase": format!("skipped: {e}") }),
                );
            }
        }
    }

    idx.finish_building(&ok_drives);
    let _ = app.emit(
        "global-search-progress",
        serde_json::json!({ "drive": "", "scanned": 0, "phase": "done" }),
    );
    Ok(())
}

/// 全局搜索：按文件名匹配，返回结果（索引未就绪时 ready=false）
#[command]
pub fn global_search(query: String, limit: Option<usize>) -> GlobalSearchResponse {
    let idx = flashdir::global_search::instance();
    let state = idx.state();
    let ready = matches!(state, flashdir::global_search::IndexState::Ready(..));
    let (results, index_size, sample_names) = if ready {
        let r = idx.search_with_filter(&query, limit.unwrap_or(500));
        let empty = r.is_empty() && !query.trim().is_empty();
        let n = if empty { Some(idx.entries_len()) } else { None };
        let sn = if empty { Some(idx.sample_names(5)) } else { None };
        (r, n, sn)
    } else {
        (vec![], None, None)
    };
    GlobalSearchResponse { ready, state, results, index_size, sample_names }
}

/// 将主界面扫描结果追加到全局索引（复用已验证的 scan_dir 结果，
/// 绕开 MFT 在异步上下文偶现的 name 解析异常。前端 scan 完成后自动调用）
#[command]
pub fn global_search_add_scan(
    path: String,
    items: Vec<flashdir::scan::Item>,
) -> Result<(), String> {
    flashdir::global_search::instance().add_items(&path, &items);
    Ok(())
}

/// 刷新索引（全量重建，走 scan_directory 保证文件名正确）
#[command]
pub async fn global_search_refresh(app: tauri::AppHandle) -> Result<(), String> {
    let idx = flashdir::global_search::instance();
    idx.set_loading();

    let drives = flashdir::global_search::list_ntfs_drives();
    let perf = flashdir::perf::PerformanceMonitor::instance();
    let mut ok_drives: Vec<char> = Vec::new();

    for &drive in &drives {
        let root = format!("{}:\\", drive);
        if let Some(cached) = flashdir::scan::get_cached_items(&root) {
            idx.append_scan(drive, &cached);
            ok_drives.push(drive);
            continue;
        }
        if let Some(lite_items) = flashdir::scan::scan_lite(&root) {
            idx.append_scan(drive, &lite_items);
            ok_drives.push(drive);
            continue;
        }
        if let Ok(result) = flashdir::scan::scan_directory(
            &root, false, std::sync::Arc::clone(&perf), Some(app.clone()),
        )
        .await
        {
            idx.append_scan(drive, &result.items);
            ok_drives.push(drive);
        }
    }
    idx.finish_building(&ok_drives);
    Ok(())
}

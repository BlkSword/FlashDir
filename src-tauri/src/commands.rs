// 命令处理器 - 优化版
// 集成性能监控、磁盘缓存、自定义二进制扫描结果传输

use flashdir::scan::{self, HistoryItem, HistoryItemSummary, ScanResult};
use flashdir::perf::PerformanceMonitor;
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

    // 原子写入：先写临时文件再 rename，避免进程中断导致 history.json 损坏
    let tmp_path = path.with_extension("json.tmp");
    let mut file = fs::File::create(&tmp_path)
        .await
        .map_err(|e| format!("创建临时文件失败: {}", e))?;

    file.write_all(json.as_bytes())
        .await
        .map_err(|e| format!("写入临时文件失败: {}", e))?;

    file.sync_all()
        .await
        .map_err(|e| format!("同步临时文件失败: {}", e))?;

    fs::rename(&tmp_path, &path)
        .await
        .map_err(|e| format!("替换历史文件失败: {}", e))?;

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

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanPageResponse {
    pub items: Vec<flashdir::scan::Item>,
    pub total_items: usize,
    pub total_size: i64,
    pub total_size_formatted: String,
    pub file_count: usize,
    pub dir_count: usize,
    pub scan_time: f64,
    pub path: String,
    pub mft_available: bool,
    pub page: usize,
    pub page_size: usize,
    pub top_files: Vec<flashdir::scan::Item>,
}

/// 分页扫描：后端只返回当前页，避免大目录把全量 items 传到前端导致崩溃。
#[command]
pub async fn scan_directory_paged(
    path: String,
    force_refresh: bool,
    page: Option<usize>,
    page_size: Option<usize>,
    sort_column: Option<String>,
    sort_direction: Option<String>,
    filter: Option<String>,
) -> Result<ScanPageResponse, String> {
    let result = flashdir::scan::scan_directory(
        &path,
        force_refresh,
        flashdir::perf::PerformanceMonitor::instance(),
        None,
    )
    .await
    .map_err(|e| e.to_string())?;

    let flashdir::scan::ScanResult {
        items,
        total_size,
        total_size_formatted,
        scan_time,
        path,
        mft_available,
        ..
    } = result;

    let page = page.unwrap_or(1).max(1);
    let page_size = page_size.unwrap_or(100).clamp(1, 1000);
    let sort_column = sort_column.unwrap_or_else(|| "size".to_string());
    let sort_direction = sort_direction.unwrap_or_else(|| "desc".to_string());

    let mut items = items;
    // MFT 解析失败的占位记录不应展示给用户
    items.retain(|i| !i.name.starts_with("<record_"));
    if let Some(filter) = filter.as_deref() {
        let lower = filter.trim().to_lowercase();
        if !lower.is_empty() {
            items.retain(|i| {
                i.name.to_lowercase().contains(&lower) || i.path.to_lowercase().contains(&lower)
            });
        }
    }

    if sort_column != "size" || sort_direction != "desc" {
        items.sort_unstable_by(|a, b| {
            let ord = match sort_column.as_str() {
                "name" => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
                "mtime" => a.mtime.cmp(&b.mtime),
                _ => a.size.cmp(&b.size),
            };
            if sort_direction == "asc" { ord } else { ord.reverse() }
        });
    }

    let total_items = items.len();
    let file_count = items.iter().filter(|i| !i.is_dir).count();
    let dir_count = total_items - file_count;
    let top_files: Vec<flashdir::scan::Item> = items
        .iter()
        .filter(|i| !i.is_dir)
        .take(5)
        .cloned()
        .collect();
    let start = (page - 1) * page_size;
    let end = start.saturating_add(page_size).min(total_items);
    let page_items: Vec<flashdir::scan::Item> = if start < total_items {
        items[start..end].to_vec()
    } else {
        Vec::new()
    };

    Ok(ScanPageResponse {
        items: page_items,
        total_items,
        total_size,
        total_size_formatted: total_size_formatted.to_string(),
        file_count,
        dir_count,
        scan_time,
        path: path.to_string(),
        mft_available,
        page,
        page_size,
        top_files,
    })
}

/// 目录树懒加载：只返回指定目录的直接子目录。
#[command]
pub async fn get_dir_children(path: String) -> Result<Vec<flashdir::scan::Item>, String> {
    let result = flashdir::scan::scan_directory(
        &path,
        false,
        flashdir::perf::PerformanceMonitor::instance(),
        None,
    )
    .await
    .map_err(|e| e.to_string())?;

    let normalized = path.replace('\\', "/");
    let root = normalized.trim_end_matches('/');
    let prefix = if root.ends_with(':') {
        format!("{}/", root)
    } else {
        format!("{}/", root)
    };

    let children: Vec<flashdir::scan::Item> = result
        .items
        .into_iter()
        .filter(|i| {
            i.is_dir
                && !i.name.starts_with("<record_")
                && i.path.starts_with(&prefix)
                && !i.path[prefix.len()..].contains('/')
        })
        .collect();

    Ok(children)
}

#[command]
pub fn get_history_summary(state: State<'_, AppState>) -> Vec<HistoryItemSummary> {
    let history = state.history.lock();
    let summaries: Vec<HistoryItemSummary> = history.iter().map(|item| item.into()).collect();
    summaries.into_iter().rev().collect()
}

#[command]
pub async fn clear_history(state: State<'_, AppState>) -> Result<(), String> {
    {
        let mut history = state.history.lock();
        history.clear();
    }

    save_history_to_file_async(&VecDeque::new()).await
}

/// 获取内存缓存统计
#[command]
pub fn get_memory_cache_stats() -> MemoryCacheStats {
    let (entries, bytes) = flashdir::scan::memory_cache_stats();
    MemoryCacheStats {
        max_entries: 30,
        max_size_mb: 200,
        current_entries: entries,
        current_size_mb: bytes as f64 / 1024.0 / 1024.0,
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct MemoryCacheStats {
    pub max_entries: usize,
    pub max_size_mb: usize,
    pub current_entries: usize,
    pub current_size_mb: f64,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticsInfo {
    pub is_admin: bool,
    pub data_dir: String,
    pub memory_cache: MemoryCacheStats,
    pub disk_cache: flashdir::disk_cache::CacheStats,
    pub global_search_state: flashdir::global_search::IndexState,
    pub usn_checkpoints: Vec<String>,
    pub history_file_exists: bool,
    pub history_count: usize,
}

/// 汇总当前运行时诊断信息，便于用户/开发者快速定位缓存、索引、权限和 USN 状态。
#[command]
pub fn get_diagnostics(state: State<'_, AppState>) -> DiagnosticsInfo {
    let history_path = get_history_file_path().ok();
    let data_dir = history_path
        .as_ref()
        .and_then(|p| p.parent())
        .map(|p| p.display().to_string())
        .unwrap_or_default();

    let usn_checkpoints = if let Some(dir) = history_path.as_ref().and_then(|p| p.parent()) {
        std::fs::read_dir(dir)
            .map(|entries| {
                entries
                    .filter_map(|e| e.ok())
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .filter(|name| name.starts_with("usn_checkpoint_") && name.ends_with(".json"))
                    .collect()
            })
            .unwrap_or_default()
    } else {
        Vec::new()
    };

    let history_count = state.history.lock().len();

    DiagnosticsInfo {
        is_admin: flashdir::fs::is_admin(),
        data_dir,
        memory_cache: get_memory_cache_stats(),
        disk_cache: flashdir::disk_cache::DiskCache::instance().get_stats(),
        global_search_state: flashdir::global_search::instance().state(),
        usn_checkpoints,
        history_file_exists: history_path.as_ref().map(|p| p.exists()).unwrap_or(false),
        history_count,
    }
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

/// 请求取消当前扫描
#[command]
pub fn cancel_scan() {
    flashdir::cancel::request();
}

/// 开始监听指定目录的变更（内部定期 USN 增量刷新）
#[command]
pub fn start_watch(app: tauri::AppHandle, path: String) {
    flashdir::watcher::start(app, path);
}

/// 停止目录变更监听
#[command]
pub fn stop_watch() {
    flashdir::watcher::stop();
}

/// 查询目录变更监听状态
#[command]
pub fn watch_status() -> bool {
    flashdir::watcher::is_active()
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

/// 重复文件检测：从内存缓存读取当前扫描结果，按大小 + 内容哈希找出重复文件。
#[command]
pub fn find_duplicates(
    path: String,
    min_size: Option<i64>,
) -> Result<flashdir::duplicate_finder::DuplicateResult, String> {
    let items = flashdir::scan::get_cached_items(&path)
        .ok_or_else(|| format!("内存缓存中不存在 {} 的扫描结果，请重新扫描", path))?;
    let min_size = min_size.unwrap_or(1).max(0);
    Ok(flashdir::duplicate_finder::find_duplicates(&items, min_size))
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

/// 从内存缓存构造 ScanResult，避免快照/全局搜索等场景把百万级 items 经 JSON 跨 IPC 回传。
fn cached_scan_result(path: &str) -> Result<flashdir::scan::ScanResult, String> {
    let cached = flashdir::scan::get_cached_items(path)
        .ok_or_else(|| format!("内存缓存中不存在 {} 的扫描结果，请重新扫描", path))?;
    let items: Vec<flashdir::scan::Item> = cached.iter().cloned().collect();
    let total_size: i64 = items
        .iter()
        .filter(|i| !i.is_dir)
        .map(|i| i.size)
        .sum();

    Ok(flashdir::scan::ScanResult {
        items,
        total_size,
        total_size_formatted: flashdir::scan::format_size(total_size),
        scan_time: 0.0,
        path: flashdir::scan::CompactString::from(path),
        mft_available: false,
        timing: None,
        perf_metrics: None,
    })
}

/// 保存当前扫描结果为快照（缓存优先版本，避免把全量 items 从前端传回后端）。
#[command]
pub fn save_snapshot_from_cache(path: String) -> Result<i64, String> {
    let result = cached_scan_result(&path)?;
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

/// 对比最新快照与当前扫描结果（缓存优先，避免前端回传全量 items）。
#[command]
pub fn compare_with_latest_snapshot_from_cache(
    path: String,
) -> Result<Option<flashdir::diff_engine::SnapshotDiff>, String> {
    let disk_cache = flashdir::disk_cache::DiskCache::instance();
    let snapshots = disk_cache
        .list_snapshots(&path)
        .map_err(|e| format!("获取快照列表失败: {}", e))?;

    if snapshots.is_empty() {
        return Ok(None);
    }

    let latest = &snapshots[0];
    let old_result = disk_cache
        .get_snapshot(latest.id)
        .ok_or_else(|| format!("快照 {} 不存在", latest.id))?;

    let current_result = cached_scan_result(&path)?;

    Ok(Some(flashdir::diff_engine::diff(
        &old_result.items,
        &current_result.items,
        old_result.total_size,
    )))
}

/// 删除指定快照
#[command]
pub fn delete_snapshot(id: i64) -> Result<(), String> {
    flashdir::disk_cache::DiskCache::instance()
        .delete_snapshot(id)
        .map_err(|e| format!("删除快照失败: {}", e))
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
        // 不能调 finish_building(&[])：那会把状态置为 Ready(0 项)，
        // 不仅显示"就绪 · 0 项"，还会把空索引持久化，且后续 ensure_index
        // 因 Ready 提前返回而永远不再重试
        idx.set_failed("未检测到可扫描的 NTFS 卷（需要管理员权限读取 MFT）".to_string());
        return Err("未检测到可扫描的 NTFS 卷（需要管理员权限读取 MFT）".to_string());
    }

    let perf = flashdir::perf::PerformanceMonitor::instance();
    let mut ok_drives: Vec<char> = Vec::new();
    let mut failed_drives: Vec<char> = Vec::new();

    // 并行 MFT 扫描所有 NTFS 卷，内存/磁盘缓存命中直接复用
    let parallel_results: Vec<(char, Option<usize>)> = std::thread::scope(|scope| {
        let mut handles: Vec<(char, std::thread::ScopedJoinHandle<'_, (char, Option<usize>)>)> = Vec::new();
        for &drive in &drives {
            let root = format!("{}:\\", drive);
            handles.push((
                drive,
                scope.spawn(move || {
                    if let Some(cached) = flashdir::scan::get_cached_items(&root) {
                        let count = cached.len();
                        flashdir::global_search::instance().append_scan(drive, &cached);
                        (drive, Some(count))
                    } else if let Some(mft_result) = flashdir::fs::try_mft_scan(&root) {
                        let count = mft_result.files.len();
                        flashdir::global_search::instance().append_mft_files(drive, &mft_result.files);
                        (drive, Some(count))
                    } else {
                        (drive, None)
                    }
                }),
            ));
        }
        handles
            .into_iter()
            .map(|(drive, handle)| handle.join().unwrap_or((drive, None)))
            .collect()
    });

    for (drive, count) in parallel_results {
        match count {
            Some(count) => {
                ok_drives.push(drive);
                let _ = app.emit(
                    "global-search-progress",
                    serde_json::json!({ "drive": drive.to_string(), "scanned": flashdir::global_search::instance().entries_len(), "phase": "ok (mft)", "count": count }),
                );
            }
            None => failed_drives.push(drive),
        }
    }

    // 对 MFT 失败的卷回退到完整目录遍历
    for drive in failed_drives {
        let root = format!("{}:\\", drive);
        match flashdir::scan::scan_directory(&root, false, std::sync::Arc::clone(&perf), Some(app.clone()))
            .await
        {
            Ok(result) => {
                idx.append_scan(drive, &result.items);
                ok_drives.push(drive);
                let _ = app.emit(
                    "global-search-progress",
                    serde_json::json!({ "drive": drive.to_string(), "scanned": idx.entries_len(), "phase": "ok (walk)", "count": result.items.len() }),
                );
            }
            Err(e) => {
                let _ = app.emit(
                    "global-search-progress",
                    serde_json::json!({ "drive": drive.to_string(), "scanned": idx.entries_len(), "phase": format!("skipped: {e}") }),
                );
            }
        }
    }

    if ok_drives.is_empty() {
        idx.set_failed("所有卷都无法建立索引（需要管理员权限读取 MFT）".to_string());
    } else {
        idx.finish_building(&ok_drives);
    }
    let _ = app.emit(
        "global-search-progress",
        serde_json::json!({ "drive": "", "scanned": idx.entries_len(), "phase": "done" }),
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

/// 将主界面扫描结果追加到全局索引（缓存优先版本）。
/// 扫描刚完成时后端内存缓存中一定存在该路径，因此无需把百万级 items 经 JSON 传回后端。
#[command]
pub fn global_search_add_scan_from_cache(path: String) -> Result<(), String> {
    let items = flashdir::scan::get_cached_items(&path)
        .ok_or_else(|| format!("内存缓存中不存在 {} 的扫描结果，请重新扫描", path))?;
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

        let mut count = 0usize;
        if let Some(cached) = flashdir::scan::get_cached_items(&root) {
            idx.append_scan(drive, &cached);
            ok_drives.push(drive);
            count = cached.len();
        } else if let Some(mft_result) = flashdir::fs::try_mft_scan(&root) {
            idx.append_mft_files(drive, &mft_result.files);
            ok_drives.push(drive);
            count = mft_result.files.len();
        } else if let Ok(result) = flashdir::scan::scan_directory(
            &root, false, std::sync::Arc::clone(&perf), Some(app.clone()),
        )
        .await
        {
            idx.append_scan(drive, &result.items);
            ok_drives.push(drive);
            count = result.items.len();
        }

        let _ = app.emit(
            "global-search-progress",
            serde_json::json!({ "drive": drive.to_string(), "scanned": idx.entries_len(), "phase": if count > 0 { "ok" } else { "skipped" }, "count": count }),
        );
    }
    if ok_drives.is_empty() {
        idx.set_failed("所有卷都无法建立索引（需要管理员权限读取 MFT）".to_string());
    } else {
        idx.finish_building(&ok_drives);
    }
    let _ = app.emit(
        "global-search-progress",
        serde_json::json!({ "drive": "", "scanned": idx.entries_len(), "phase": "done" }),
    );
    Ok(())
}

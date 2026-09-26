// 命令处理器
// 负责 IPC 边界：参数校验、历史记录、调用后端引擎、重活下沉到线程池

use flashdir::scan::{self, HistoryItem, HistoryItemSummary};
use flashdir::global_search::GlobalSearchResponse;
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

/// 记录一次"用户主动扫描"到历史（分页/过滤等只读请求不记录）
fn push_history(state: &AppState, path: &str, total_size: i64, item_count: usize) {
    let history_item = HistoryItem {
        path: smartstring::SmartString::from(path),
        scan_time: Utc::now(),
        total_size,
        size_format: smartstring::SmartString::from(
            flashdir::scan::format_size(total_size).as_str(),
        ),
        item_count,
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
}

/// 分页扫描的响应体（需要 Serialize 才能作为 IPC 响应返回）
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
    /// 本次结果来源：memory / disk / usn / usn-disk / derived / scan
    pub cache_source: String,
}

/// 分页扫描：后端只返回当前页，避免大目录把全量 items 传到前端导致崩溃。
///
/// 使用 `scan_directory_view`：命中缓存/USN 校验通过时共享 `Arc<Vec<Item>>`，
/// 不再为每次翻页深拷贝整份条目列表。
#[command]
pub async fn scan_directory_paged(
    path: String,
    force_refresh: bool,
    page: Option<usize>,
    page_size: Option<usize>,
    sort_column: Option<String>,
    sort_direction: Option<String>,
    filter: Option<String>,
    record_history: Option<bool>,
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<ScanPageResponse, String> {
    let view = flashdir::scan::scan_directory_view(
        &path,
        force_refresh,
        flashdir::perf::PerformanceMonitor::instance(),
        Some(app),
    )
    .await
    .map_err(|e| e.to_string())?;

    let page = page.unwrap_or(1).max(1);
    let page_size = page_size.unwrap_or(100).clamp(1, 1000);
    let sort_column = sort_column.unwrap_or_else(|| "size".to_string());
    let sort_direction = sort_direction.unwrap_or_else(|| "desc".to_string());

    // 本地过滤与全局搜索共用 Everything 式语法（ext:/size:/type:/dir:/mtime:/NOT/通配符）
    let filters = filter
        .as_deref()
        .map(flashdir::global_search::parse_search_filter)
        .unwrap_or_default();
    let use_filter = !filters.is_empty();

    // 过滤匹配"相对扫描根的路径"：否则扫描 C:/Windows 时输入 windows 会命中全部条目
    //（每个条目的绝对路径都含 C:/Windows），用户会以为过滤没生效。
    let root_prefix: String = {
        let p = view.path.to_string();
        if p.ends_with('/') {
            p
        } else {
            format!("{}/", p)
        }
    };

    // 只收集引用，避免为过滤/排序克隆整份条目
    let mut filtered: Vec<&flashdir::scan::Item> = Vec::new();
    for item in view.items() {
        // MFT 解析失败的占位记录不应展示给用户
        if item.name.starts_with("<record_") {
            continue;
        }
        let match_path = item
            .path
            .as_str()
            .strip_prefix(root_prefix.as_str())
            .unwrap_or(item.path.as_str());
        if use_filter
            && !flashdir::global_search::item_matches_filters(
                item.name.as_str(),
                match_path,
                item.size,
                item.is_dir,
                item.mtime,
                &filters,
            )
        {
            continue;
        }
        filtered.push(item);
    }

    // Top 5 大文件必须独立按 size 挑选：
    // 早期实现直接取"当前排序后的前 5 个文件"，用户按名称排序时会拿到错的 Top5。
    let mut top_files: Vec<flashdir::scan::Item> = {
        let mut files: Vec<&flashdir::scan::Item> =
            filtered.iter().copied().filter(|i| !i.is_dir).collect();
        if files.len() > 5 {
            files.select_nth_unstable_by(5, |a, b| b.size.cmp(&a.size));
            files.truncate(5);
        }
        files.sort_unstable_by(|a, b| b.size.cmp(&a.size));
        files.into_iter().cloned().collect()
    };
    // 只有返回给前端的这几条需要格式化文本（全量条目的格式化已移除）
    for item in top_files.iter_mut() {
        item.size_formatted = flashdir::scan::format_size(item.size);
    }

    if sort_column != "size" || sort_direction != "desc" {
        filtered.sort_unstable_by(|a, b| {
            let ord = match sort_column.as_str() {
                "atime" => a.atime.cmp(&b.atime),
                "name" => compare_ignore_case(a.name.as_str(), b.name.as_str()),
                "mtime" => a.mtime.cmp(&b.mtime),
                _ => a.size.cmp(&b.size),
            };
            if sort_direction == "asc" {
                ord
            } else {
                ord.reverse()
            }
        });
    }

    let total_items = filtered.len();
    let file_count = filtered.iter().filter(|i| !i.is_dir).count();
    let dir_count = total_items - file_count;

    let start = (page - 1) * page_size;
    let end = start.saturating_add(page_size).min(total_items);
    let mut page_items: Vec<flashdir::scan::Item> = if start < total_items {
        filtered[start..end].iter().map(|i| (*i).clone()).collect()
    } else {
        Vec::new()
    };
    for item in page_items.iter_mut() {
        item.size_formatted = flashdir::scan::format_size(item.size);
    }

    // 历史记录只在"用户主动扫描"时写入；分页/排序/过滤请求不重复记录
    if record_history.unwrap_or(false) {
        push_history(&state, &path, view.total_size, view.items().len());
    }

    Ok(ScanPageResponse {
        items: page_items,
        total_items,
        total_size: view.total_size,
        total_size_formatted: flashdir::scan::format_size(view.total_size).to_string(),
        file_count,
        dir_count,
        scan_time: view.scan_time,
        path: view.path.to_string(),
        mft_available: view.mft_available,
        page,
        page_size,
        cache_source: view.cache_source.clone().unwrap_or_else(|| "scan".to_string()),
        top_files,
    })
}

/// 大小写不敏感的名称比较（不分配字符串，避免每次排序比较都 to_lowercase）
fn compare_ignore_case(a: &str, b: &str) -> std::cmp::Ordering {
    let mut ai = a.chars().flat_map(|c| c.to_lowercase());
    let mut bi = b.chars().flat_map(|c| c.to_lowercase());
    loop {
        match (ai.next(), bi.next()) {
            (None, None) => return std::cmp::Ordering::Equal,
            (None, Some(_)) => return std::cmp::Ordering::Less,
            (Some(_), None) => return std::cmp::Ordering::Greater,
            (Some(x), Some(y)) => {
                if x != y {
                    return x.cmp(&y);
                }
            }
        }
    }
}

/// 目录树懒加载：只返回指定目录的直接子目录。
///
/// 前缀必须基于 canonicalize 后的路径构造：用户输入的大小写/分隔符
/// 与磁盘不一致时（例如小写盘符路径），旧实现会匹配不到任何条目，目录树静默为空。
#[command]
pub async fn get_dir_children(path: String) -> Result<Vec<flashdir::scan::Item>, String> {
    let canonical = tokio::fs::canonicalize(&path)
        .await
        .map_err(|e| format!("无法访问路径 {}: {}", path, e))?;
    let raw = canonical
        .to_string_lossy()
        .replace(std::path::MAIN_SEPARATOR, "/");
    let root = raw
        .trim_start_matches("//?/")
        .trim_start_matches("//./")
        .trim_end_matches('/');
    let prefix = format!("{}/", root);

    let view = flashdir::scan::scan_directory_view(
        &path,
        false,
        flashdir::perf::PerformanceMonitor::instance(),
        None,
    )
    .await
    .map_err(|e| e.to_string())?;

    let mut children: Vec<flashdir::scan::Item> = view
        .items()
        .iter()
        .filter(|i| {
            i.is_dir
                && !i.name.starts_with("<record_")
                && i.path.starts_with(&prefix)
                && !i.path.as_str()[prefix.len()..].contains('/')
        })
        .cloned()
        .collect();
    for child in children.iter_mut() {
        child.size_formatted = flashdir::scan::format_size(child.size);
    }

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
/// MCP 运行状态（状态栏展示：是否被 AI 客户端连接、最近调用了什么）
#[cfg(feature = "mcp")]
#[command]
pub fn get_mcp_status() -> serde_json::Value {
    flashdir::mcp::status_json()
}

/// 读取 MCP 设置（开关 / 端口 / 当前地址 / 运行状态）
#[cfg(feature = "mcp")]
#[command]
pub fn get_mcp_settings() -> serde_json::Value {
    flashdir::mcp::settings_json()
}

/// 修改 MCP 设置：开关与端口（写入 ~/.flashdir/mcp-settings.json，
/// 桌面端内端点会在 ~1 秒内热生效——无需重启）
#[cfg(feature = "mcp")]
#[command]
pub fn set_mcp_settings(
    enabled: bool,
    port: Option<u16>,
) -> Result<serde_json::Value, String> {
    let mut s = flashdir::mcp::load_settings();
    s.enabled = enabled;
    if let Some(p) = port {
        if !(1024..=65535).contains(&p) {
            return Err("端口需在 1024-65535 之间".to_string());
        }
        s.port = p;
    }
    flashdir::mcp::save_settings(&s)?;
    // 立即返回最新状态；实际绑定由端点线程在 1 秒内完成
    Ok(flashdir::mcp::settings_json())
}

/// MCP 配置片段，供设置页一键复制。
///
/// 两种形态：
/// - **HTTP（推荐）**：地址固定（`127.0.0.1:47821`）+ 持久 token → 配置跨机器一致、长期有效，
///   适合支持 `url` 的 Host（Claude Desktop 连接器 / Cursor）。
/// - **stdio**：命令 + 参数，兼容所有 Host；桥接会共享桌面端的索引/缓存与管理员权限。
#[cfg(feature = "mcp")]
#[command]
pub fn get_mcp_config() -> serde_json::Value {
    let exe = std::env::current_exe()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| "flashdir.exe".to_string());

    let http_cfg = serde_json::json!({
        "mcpServers": {
            "flashdir": { "url": flashdir::mcp::http_url_for_display() }
        }
    });
    let stdio_cfg = serde_json::json!({
        "mcpServers": {
            "flashdir": { "command": exe, "args": ["--bridge"] }
        }
    });

    serde_json::json!({
        "url": flashdir::mcp::http_url_for_display(),
        "port": flashdir::mcp::endpoint_info().get("port").and_then(|p| p.as_u64()),
        "command": exe,
        "args": ["--bridge"],
        "configHttp": serde_json::to_string_pretty(&http_cfg).unwrap_or_default(),
        "configStdio": serde_json::to_string_pretty(&stdio_cfg).unwrap_or_default(),
        "endpoint": flashdir::mcp::endpoint_info(),
        "status": flashdir::mcp::status_json(),
    })
}

#[command]
pub fn get_volumes() -> Vec<crate::volumes::VolumeInfo> {
    crate::volumes::list_volumes()
}

#[command]
pub fn is_admin() -> bool {
    flashdir::fs::is_admin()
}

/// 请求取消当前扫描
#[command]
pub fn cancel_scan() {
    flashdir::cancel::request();
}
/// 以管理员权限重启应用。
/// 提权进程启动成功后立即退出当前实例，避免出现"旧实例 + 新实例 + 两个托盘"。
#[command]
pub fn restart_as_admin() -> bool {
    let started = flashdir::fs::restart_as_admin();
    if started {
        std::process::exit(0);
    }
    false
}

/// 开发者磁盘分析：从内存缓存读取当前路径的扫描结果（避免百万级 items 跨 IPC 传输），
/// 识别并分类常见开发工具/缓存目录的空间占用（已按"匹配边界顶层"去重，杜绝重复累加）。
///
/// 使用 spawn_blocking：Rayon 全量分类是 CPU 密集操作，不能在 IPC 主线程执行。
#[command]
pub async fn analyze_dev_disk(path: String) -> Option<flashdir::dev_analyzer::DevAnalysisResult> {
    tauri::async_runtime::spawn_blocking(move || {
        let items = flashdir::scan::get_cached_items(&path)?;
        let total_size: i64 = items.iter().filter(|i| !i.is_dir).map(|i| i.size).sum();
        let total_items = items.len();
        Some(flashdir::dev_analyzer::analyze(
            &items,
            total_size,
            total_items,
        ))
    })
    .await
    .ok()
    .flatten()
}

/// 重复文件检测：从内存缓存读取当前扫描结果，按大小 + 内容哈希找出重复文件。
///
/// 使用 spawn_blocking：需要读取并哈希大量文件内容，不能在 IPC 主线程执行。
#[command]
pub async fn find_duplicates(
    path: String,
    min_size: Option<i64>,
) -> Result<flashdir::duplicate_finder::DuplicateResult, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let items = flashdir::scan::get_cached_items(&path)
            .ok_or_else(|| format!("内存缓存中不存在 {} 的扫描结果，请重新扫描", path))?;
        let min_size = min_size.unwrap_or(1).max(0);
        Ok::<_, String>(flashdir::duplicate_finder::find_duplicates(
            &items, min_size,
        ))
    })
    .await
    .map_err(|e| format!("任务执行失败: {}", e))?
}

// ─── 快照管理 ────────────────────────────────────────────

/// 保存当前扫描结果为快照（只从后端内存缓存读取，避免前端回传全量 items）。
///
/// 说明：早期还有一个"从前端传入 items"的 `save_snapshot` 命令，但前端在缓存
/// 被淘汰时只能回传当前分页的 100 条，会生成"残缺但看起来正常"的快照，
/// 已移除；缓存缺失时直接报错，要求重新扫描。
#[command]
pub async fn save_snapshot_from_cache(path: String) -> Result<i64, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let items = flashdir::scan::get_cached_items(&path)
            .ok_or_else(|| format!("内存缓存中不存在 {} 的扫描结果，请重新扫描后再保存快照", path))?;
        let total_size: i64 = items.iter().filter(|i| !i.is_dir).map(|i| i.size).sum();
        let file_count = items.iter().filter(|i| !i.is_dir).count();
        let dir_count = items.len() - file_count;

        flashdir::disk_cache::DiskCache::instance()
            .insert_snapshot(
                &path,
                items.as_slice(),
                total_size,
                flashdir::scan::format_size(total_size).as_str(),
                file_count,
                dir_count,
            )
            .map_err(|e| format!("保存快照失败: {}", e))
    })
    .await
    .map_err(|e| format!("任务执行失败: {}", e))?
}

/// 列出指定路径的所有快照
#[command]
pub fn list_snapshots(path: String) -> Result<Vec<flashdir::disk_cache::SnapshotInfo>, String> {
    flashdir::disk_cache::DiskCache::instance()
        .list_snapshots(&path)
        .map_err(|e| format!("获取快照列表失败: {}", e))
}

/// 比较两个快照（传入快照 ID）。
/// 使用 spawn_blocking：百万级条目的 diff 是 CPU/内存密集操作。
#[command]
pub async fn compare_snapshots(
    old_id: i64,
    new_id: i64,
) -> Result<flashdir::diff_engine::SnapshotDiff, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let disk_cache = flashdir::disk_cache::DiskCache::instance();

        let old_result = disk_cache
            .get_snapshot(old_id)
            .ok_or_else(|| format!("快照 {} 不存在", old_id))?;

        let new_result = disk_cache
            .get_snapshot(new_id)
            .ok_or_else(|| format!("快照 {} 不存在", new_id))?;

        Ok::<_, String>(flashdir::diff_engine::diff(
            &old_result.items,
            &new_result.items,
            old_result.total_size,
        ))
    })
    .await
    .map_err(|e| format!("任务执行失败: {}", e))?
}

/// 对比最新快照与当前扫描结果（缓存优先，避免前端回传全量 items）。
#[command]
pub async fn compare_with_latest_snapshot_from_cache(
    path: String,
) -> Result<Option<flashdir::diff_engine::SnapshotDiff>, String> {
    tauri::async_runtime::spawn_blocking(move || {
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

        // 直接共享内存缓存中的条目，不再克隆一份 Vec
        let current = flashdir::scan::get_cached_items(&path)
            .ok_or_else(|| format!("内存缓存中不存在 {} 的扫描结果，请重新扫描", path))?;

        Ok(Some(flashdir::diff_engine::diff(
            &old_result.items,
            current.as_slice(),
            old_result.total_size,
        )))
    })
    .await
    .map_err(|e| format!("任务执行失败: {}", e))?
}

/// 删除指定快照
#[command]
pub fn delete_snapshot(id: i64) -> Result<(), String> {
    flashdir::disk_cache::DiskCache::instance()
        .delete_snapshot(id)
        .map_err(|e| format!("删除快照失败: {}", e))
}

// ─── 全局文件搜索 ──────────────────────────────────────────

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

    // 重活前的两个动作：先把内存扫描缓存降下来，再检查可用内存。
    // 内存不足时返回错误交给界面提示，而不是让分配失败把进程干掉
    // （release 是 panic=abort，分配失败时窗口会毫无征兆地消失）。
    let freed = flashdir::scan::clear_memory_cache();
    flashdir::diag::breadcrumb(&format!(
        "索引构建：开始（已释放 {freed} 个目录的内存缓存，可用内存 {}）",
        flashdir::diag::memory_text()
    ));
    flashdir::diag::ensure_memory_available(500, "构建全盘索引")?;

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
} else if let Some(summary) = flashdir::fs::try_mft_scan_streaming(
    &root,
    50_000,
    |batch| flashdir::global_search::instance().append_mft_files(drive, &batch),
) {
    (drive, Some(summary.file_count + summary.dir_count - 1 /* 去掉未被写入的根目录 */))
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
    let mut still_failed: Vec<char> = Vec::new();
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
                still_failed.push(drive);
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
        // 记录跳过的盘符，前端会提示"（X 跳过）"
        idx.finish_building(&ok_drives, &still_failed);
    }
    let _ = app.emit(
        "global-search-progress",
        serde_json::json!({ "drive": "", "scanned": idx.entries_len(), "phase": "done" }),
    );
    Ok(())
}

/// 全局搜索：按文件名匹配，返回结果（索引未就绪时 ready=false）。
/// 使用 spawn_blocking：索引过滤/排序可能扫描百万级条目。
#[command]
pub async fn global_search(
    query: String,
    limit: Option<usize>,
    offset: Option<usize>,
) -> flashdir::global_search::GlobalSearchResponse {
    // 上限 20 万条：既够"查看更多"，又不至于把 IPC 打爆
    let limit = limit.unwrap_or(500).clamp(1, 200_000);
    let offset = offset.unwrap_or(0);
    tauri::async_runtime::spawn_blocking(move || {
        let idx = flashdir::global_search::instance();
        let state = idx.state();
        let ready = matches!(state, flashdir::global_search::IndexState::Ready(..));
        let (results, total, index_size, sample_names) = if ready {
            let (r, total) = idx.search_with_filter_paged(&query, limit, offset);
            let empty = r.is_empty() && !query.trim().is_empty();
            let n = if empty { Some(idx.entries_len()) } else { None };
            let sn = if empty { Some(idx.sample_names(5)) } else { None };
            (r, total, n, sn)
        } else {
            (vec![], 0usize, None, None)
        };
        let truncated = total > offset + results.len();
        GlobalSearchResponse {
            ready,
            state,
            results,
            total,
            truncated,
            index_size,
            sample_names,
        }
    })
    .await
    .unwrap_or_else(|_| GlobalSearchResponse {
        ready: false,
        state: flashdir::global_search::IndexState::NotLoaded,
        results: Vec::new(),
        total: 0,
        truncated: false,
        index_size: None,
        sample_names: None,
    })
}

/// 将主界面扫描结果追加到全局索引（缓存优先）。
/// 扫描刚完成时后端内存缓存中一定存在该路径，因此无需把百万级 items 经 JSON 回传。
/// 使用 spawn_blocking：批量 upsert + SQLite 事务不应阻塞 IPC 主线程。
#[command]
pub async fn global_search_add_scan_from_cache(path: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        let items = flashdir::scan::get_cached_items(&path)
            .ok_or_else(|| format!("内存缓存中不存在 {} 的扫描结果，请重新扫描", path))?;
        flashdir::global_search::instance().add_items(&path, &items);
        Ok(())
    })
    .await
    .map_err(|e| format!("任务执行失败: {}", e))?
}

/// 刷新索引（全量重建，走 scan_directory 保证文件名正确）
#[command]
pub async fn global_search_refresh(app: tauri::AppHandle) -> Result<(), String> {
    let idx = flashdir::global_search::instance();
    idx.set_loading();

    // 重活前的两个动作：先把内存扫描缓存降下来，再检查可用内存。
    // 内存不足时返回错误交给界面提示，而不是让分配失败把进程干掉
    // （release 是 panic=abort，分配失败时窗口会毫无征兆地消失）。
    let freed = flashdir::scan::clear_memory_cache();
    flashdir::diag::breadcrumb(&format!(
        "索引构建：开始（已释放 {freed} 个目录的内存缓存，可用内存 {}）",
        flashdir::diag::memory_text()
    ));
    flashdir::diag::ensure_memory_available(500, "构建全盘索引")?;

    let drives = flashdir::global_search::list_ntfs_drives();
    let perf = flashdir::perf::PerformanceMonitor::instance();
    let mut ok_drives: Vec<char> = Vec::new();
    let mut failed_drives: Vec<char> = Vec::new();

    for &drive in &drives {
        let root = format!("{}:\\", drive);

        let mut count = 0usize;
        let mut ok = false;
        if let Some(cached) = flashdir::scan::get_cached_items(&root) {
            idx.append_scan(drive, &cached);
            count = cached.len();
            ok = true;
} else if let Some(summary) = flashdir::fs::try_mft_scan_streaming(&root, 50_000, |batch| {
    idx.append_mft_files(drive, &batch);
}) {
    count = summary.file_count + summary.dir_count - 1 /* 去掉未被写入的根目录 */;
    ok = true;
        } else if let Ok(result) = flashdir::scan::scan_directory(
            &root, false, std::sync::Arc::clone(&perf), Some(app.clone()),
        )
        .await
        {
            idx.append_scan(drive, &result.items);
            count = result.items.len();
            ok = true;
        }

        if ok {
            ok_drives.push(drive);
        } else {
            failed_drives.push(drive);
        }

        let _ = app.emit(
            "global-search-progress",
            serde_json::json!({ "drive": drive.to_string(), "scanned": idx.entries_len(), "phase": if ok { "ok" } else { "skipped" }, "count": count }),
        );
    }
    if ok_drives.is_empty() {
        idx.set_failed("所有卷都无法建立索引（需要管理员权限读取 MFT）".to_string());
    } else {
        idx.finish_building(&ok_drives, &failed_drives);
    }
    let _ = app.emit(
        "global-search-progress",
        serde_json::json!({ "drive": "", "scanned": idx.entries_len(), "phase": "done" }),
    );
    Ok(())
}


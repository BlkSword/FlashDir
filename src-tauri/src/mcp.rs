//! MCP（Model Context Protocol）核心：协议 + 工具实现。
//!
//! 三种运行形态共用本模块：
//! 1. **stdio**（`flashdir-mcp.exe`）：Host 直接以子进程启动，最简部署；
//! 2. **本机端点**（GUI 内后台线程）：`127.0.0.1` + 一次性 token，
//!    与桌面端**共享同一份索引与扫描缓存**（热态、且继承管理员权限 → 可走 MFT 直读）；
//! 3. **桥接**（`flashdir-mcp.exe --bridge`）：Host 的 stdio ↔ GUI 本机端点，
//!    GUI 未运行时自动拉起并等待就绪。
//!
//! 为什么用 127.0.0.1 而不是命名管道：GUI 通常以管理员运行，
//! 命名管道的强制完整性标签会阻止非管理员进程（Host 启动的桥）读写；
//! 本机回环 + 仅当前用户可读的 token 文件既跨完整性级别可用，也不对外开放。

use serde_json::{json, Value};
use std::io::{BufRead, Write};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex};

/// 支持的协议版本（按新→旧）；回包选择双方都支持的最新版本
const SUPPORTED_PROTOCOL_VERSIONS: [&str; 3] = ["2025-06-18", "2025-03-26", "2024-11-05"];
const SERVER_NAME: &str = "flashdir";
/// 本机端点握手超时 / 桥接等待 GUI 就绪的上限
const HANDSHAKE_TIMEOUT_MS: u64 = 5000;
const GUI_WAIT_TIMEOUT_MS: u64 = 25000;

/* ─── 运行状态（供桌面端状态栏展示"谁在用我的磁盘数据"） ─────── */

#[derive(Default)]
pub struct McpStatus {
    /// 当前打开的连接数（桥可能重连/多客户端）
    open_conns: AtomicU64,
    /// 当前是否有 Host（桥）连接
    connected: AtomicBool,
    /// 是否正在处理请求
    active: AtomicBool,
    /// 累计调用次数
    calls: AtomicU64,
    /// 最近一次调用时间（Unix 秒）
    last_at: AtomicI64,
    /// 最近一次方法 / 工具
    last_method: Mutex<String>,
    last_tool: Mutex<String>,
    /// 最近一次客户端标识（initialize 里的 clientInfo.name）
    client: Mutex<String>,
}

static STATUS: LazyLock<Arc<McpStatus>> = LazyLock::new(|| Arc::new(McpStatus::default()));

fn conn_opened() {
    STATUS.open_conns.fetch_add(1, Ordering::Relaxed);
    STATUS.connected.store(true, Ordering::Relaxed);
}

fn conn_closed() {
    let left = STATUS.open_conns.load(Ordering::Relaxed).saturating_sub(1);
    STATUS.open_conns.store(left, Ordering::Relaxed);
    if left == 0 {
        STATUS.connected.store(false, Ordering::Relaxed);
    }
}

fn set_last(method: &str, tool: &str) {
    if let Ok(mut m) = STATUS.last_method.lock() {
        *m = method.to_string();
    }
    if let Ok(mut t) = STATUS.last_tool.lock() {
        *t = tool.to_string();
    }
    STATUS.calls.fetch_add(1, Ordering::Relaxed);
    STATUS
        .last_at
        .store(chrono::Utc::now().timestamp(), Ordering::Relaxed);
}

/// 供前端查询的运行状态
pub fn status_json() -> Value {
    json!({
        "connected": STATUS.open_conns.load(Ordering::Relaxed) > 0,
        "active": STATUS.active.load(Ordering::Relaxed),
        "calls": STATUS.calls.load(Ordering::Relaxed),
        "lastAt": STATUS.last_at.load(Ordering::Relaxed),
        "lastMethod": STATUS.last_method.lock().map(|s| s.clone()).unwrap_or_default(),
        "lastTool": STATUS.last_tool.lock().map(|s| s.clone()).unwrap_or_default(),
        "client": STATUS.client.lock().map(|s| s.clone()).unwrap_or_default(),
        "endpoint": endpoint_info(),
    })
}

/* ─── JSON-RPC ─────────────────────────────────────────────── */

fn rpc_result(id: &Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn rpc_error(id: &Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

fn tool_text(text: String) -> Value {
    json!({ "content": [{ "type": "text", "text": text }], "isError": false })
}

fn tool_error(message: &str) -> Value {
    json!({ "content": [{ "type": "text", "text": message }], "isError": true })
}

/// 处理一行输入，返回需要写回的一行（notification 返回 None）
pub async fn handle_line(line: &str) -> Option<String> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }

    let req: Value = match serde_json::from_str(trimmed) {
        Ok(v) => v,
        Err(e) => {
            let resp = rpc_error(&Value::Null, -32700, &format!("JSON 解析失败: {}", e));
            return Some(resp.to_string());
        }
    };

    let id = req.get("id").cloned();
    let method = req.get("method").and_then(|m| m.as_str()).unwrap_or("");
    let params = req.get("params").cloned().unwrap_or(json!({}));

    // notification（无 id）：不回复
    if id.is_none() {
        match method {
            "notifications/initialized" => {}
            "notifications/cancelled" => {
                crate::cancel::request();
            }
            _ => {}
        }
        return None;
    }
    let id = id.unwrap();

    if let Some(client) = params
        .get("clientInfo")
        .and_then(|c| c.get("name"))
        .and_then(|n| n.as_str())
    {
        if let Ok(mut c) = STATUS.client.lock() {
            *c = client.to_string();
        }
    }
    let tool_name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
    set_last(method, tool_name);
    STATUS.active.store(true, Ordering::Relaxed);
    let resp = handle_request(method, &params, &id).await;
    STATUS.active.store(false, Ordering::Relaxed);
    Some(resp.to_string())
}

/// 方法分发（stdio 与本机端点共用）
async fn handle_request(method: &str, params: &Value, id: &Value) -> Value {
    let id = id.clone();
    let params = params.clone();
    match method {
        "initialize" => {
            let client_version = params
                .get("protocolVersion")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let chosen = SUPPORTED_PROTOCOL_VERSIONS
                .iter()
                .find(|v| **v == client_version)
                .copied()
                .unwrap_or(SUPPORTED_PROTOCOL_VERSIONS[0]);
            rpc_result(
                &id,
                json!({
                    "protocolVersion": chosen,
                    "capabilities": { "tools": { "listChanged": false } },
                    "serverInfo": { "name": SERVER_NAME, "version": env!("CARGO_PKG_VERSION") },
                    "instructions": "本机磁盘可观测性工具：先用 search_files 做毫秒级全局搜索，\
需要精确体积/目录构成时再 scan_directory（首次约 3-5 秒，之后命中缓存）。所有操作只读。"
                }),
            )
        }
        "ping" => rpc_result(&id, json!({})),
        "tools/list" => rpc_result(&id, json!({ "tools": tool_definitions() })),
        "tools/call" => {
            let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let args = params.get("arguments").cloned().unwrap_or(json!({}));
            match call_tool(name, &args).await {
                Ok(v) => rpc_result(&id, v),
                Err(msg) => rpc_result(&id, tool_error(&msg)),
            }
        }
        "resources/list" => rpc_result(&id, json!({ "resources": [] })),
        "prompts/list" => rpc_result(&id, json!({ "prompts": [] })),
        other => rpc_error(&id, -32601, &format!("不支持的方法: {}", other)),
    }
}

/* ─── 工具定义 ─────────────────────────────────────────────── */

pub fn tool_definitions() -> Vec<Value> {
    vec![
        json!({
            "name": "list_volumes",
            "description": "列出本机所有卷（盘符、卷标、文件系统、总容量/可用容量、是否 NTFS、是否就绪）。用于回答“磁盘还剩多少空间”。",
            "inputSchema": { "type": "object", "properties": {}, "additionalProperties": false }
        }),
        json!({
            "name": "search_files",
            "description": "在全局索引中搜索文件（Everything 式语法，毫秒级）。语法：ext:zip / size:>1GB / dir:node_modules / !tmp / type:file|dir / mtime:>7d / 前缀* / 通配符 *.pdf。返回命中总数与条目列表。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "查询表达式，例如 \"*.pdf size:>10MB\"" },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 1000, "description": "返回条数（默认 50）" },
                    "offset": { "type": "integer", "minimum": 0, "description": "分页偏移（默认 0）" }
                },
                "required": ["query"],
                "additionalProperties": false
            }
        }),
        json!({
            "name": "scan_directory",
            "description": "扫描目录并返回体积构成（走完整流水线：USN 增量 → 内存缓存 → 磁盘缓存 → 上层推导 → MFT 直读/目录遍历）。首次调用约 3-5 秒，之后命中缓存为毫秒级。返回总量、文件/目录数、耗时、结果来源与 Top N 条目。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "目录绝对路径，例如 \"C:\\Users\\me\\Downloads\"" },
                    "force": { "type": "boolean", "description": "忽略缓存强制重新扫描（默认 false）" },
                    "sort": { "type": "string", "enum": ["size", "name", "mtime", "atime"], "description": "排序字段（默认 size）" },
                    "direction": { "type": "string", "enum": ["desc", "asc"], "description": "排序方向（默认 desc）" },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 1000, "description": "返回条目数（默认 50）" },
                    "filter": { "type": "string", "description": "过滤表达式，与 search_files 同一套语法" }
                },
                "required": ["path"],
                "additionalProperties": false
            }
        }),
        json!({
            "name": "list_directory",
            "description": "分页列出目录内容（与 scan_directory 同一条流水线，但支持 offset 翻页）。适合“看看这个目录里都有什么”。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "sort": { "type": "string", "enum": ["size", "name", "mtime", "atime"] },
                    "direction": { "type": "string", "enum": ["desc", "asc"] },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 1000 },
                    "offset": { "type": "integer", "minimum": 0 },
                    "filter": { "type": "string" }
                },
                "required": ["path"],
                "additionalProperties": false
            }
        }),
        json!({
            "name": "cache_stats",
            "description": "查看 FlashDir 的磁盘缓存统计（条目数、占用、上限、最早条目时间）。用于判断某个目录是否需要重新扫描。",
            "inputSchema": { "type": "object", "properties": {}, "additionalProperties": false }
        }),
        json!({
            "name": "diagnostics",
            "description": "运行诊断：版本、是否管理员（决定能否走 MFT 直读）、全局索引状态与条目数、磁盘缓存统计、卷列表。回答“为什么扫描慢/搜不到”时先调用它。",
            "inputSchema": { "type": "object", "properties": {}, "additionalProperties": false }
        }),
    ]
}

/* ─── 工具实现 ─────────────────────────────────────────────── */

fn arg_str(args: &Value, key: &str) -> Option<String> {
    args.get(key).and_then(|v| v.as_str()).map(|s| s.to_string())
}

fn arg_usize(args: &Value, key: &str, default: usize) -> usize {
    args.get(key)
        .and_then(|v| v.as_u64())
        .map(|v| v as usize)
        .unwrap_or(default)
}

fn arg_bool(args: &Value, key: &str) -> bool {
    args.get(key).and_then(|v| v.as_bool()).unwrap_or(false)
}

fn item_json(item: &crate::scan::Item) -> Value {
    json!({
        "name": item.name,
        "path": item.path,
        "size": item.size,
        "isDir": item.is_dir,
        "mtime": item.mtime,
        "atime": item.atime,
    })
}

pub async fn call_tool(name: &str, args: &Value) -> Result<Value, String> {
    match name {
        "list_volumes" => {
            let vols = crate::volumes::list_volumes();
            let list: Vec<Value> = vols
                .iter()
                .map(|v| {
                    json!({
                        "letter": v.letter,
                        "label": v.label,
                        "fs": v.fs,
                        "totalBytes": v.total_bytes,
                        "freeBytes": v.free_bytes,
                        "isNtfs": v.is_ntfs,
                        "ready": v.ready,
                    })
                })
                .collect();
            Ok(tool_text(
                json!({
                    "summary": format!("共 {} 个卷", list.len()),
                    "volumes": list,
                })
                .to_string(),
            ))
        }

        "search_files" => {
            let query = arg_str(args, "query").ok_or("缺少参数 query")?;
            let limit = arg_usize(args, "limit", 50).clamp(1, 1000);
            let offset = arg_usize(args, "offset", 0);
            let idx = crate::global_search::instance();
            // 索引在后台加载：等一小会儿（首次调用很常见），避免直接返回"未就绪"
            let ready = wait_index_ready(idx, 3000).await;
            if !ready {
                return Ok(tool_text(
                    json!({
                        "ready": false,
                        "hint": "全局索引尚未就绪：请先运行一次桌面端，或等待索引构建完成",
                        "indexEntries": idx.entries_len(),
                    })
                    .to_string(),
                ));
            }
            let (entries, total) = idx.search_with_filter_paged(&query, limit, offset);
            let truncated = total > offset + entries.len();
            let results: Vec<Value> = entries
                .iter()
                .map(|e| {
                    json!({
                        "path": e.path,
                        "name": name_from_path(&e.path),
                        "size": e.size,
                        "isDir": e.is_dir,
                        "mtime": e.mtime,
                    })
                })
                .collect();
            Ok(tool_text(
                json!({
                    "summary": format!("命中 {} 项，返回 {} 项", total, results.len()),
                    "query": query,
                    "total": total,
                    "truncated": truncated,
                    "indexEntries": idx.entries_len(),
                    "results": results,
                })
                .to_string(),
            ))
        }

        "scan_directory" | "list_directory" => {
            let path = arg_str(args, "path").ok_or("缺少参数 path")?;
            let force = arg_bool(args, "force");
            let sort = arg_str(args, "sort").unwrap_or_else(|| "size".into());
            let direction = arg_str(args, "direction").unwrap_or_else(|| "desc".into());
            let limit = arg_usize(args, "limit", 50).clamp(1, 1000);
            let offset = if name == "list_directory" {
                arg_usize(args, "offset", 0)
            } else {
                0
            };
            let filter = arg_str(args, "filter").unwrap_or_default();

            let perf = crate::perf::PerformanceMonitor::instance();
            let started = std::time::Instant::now();
            let view = crate::scan::scan_directory_view(&path, force, perf, None)
                .await
                .map_err(|e| format!("扫描失败: {}", e))?;
            let elapsed_ms = started.elapsed().as_millis() as u64;

            // 过滤：与 GUI 一致，匹配"名称 + 相对扫描根的路径"
            let root_prefix = {
                let p = view.path.to_string();
                if p.ends_with('/') {
                    p
                } else {
                    format!("{}/", p)
                }
            };
            let filters = crate::global_search::parse_search_filter(&filter);
            let mut items: Vec<&crate::scan::Item> = view
                .items()
                .iter()
                .filter(|i| !i.name.starts_with("<record_"))
                .filter(|i| {
                    if filters.is_empty() {
                        return true;
                    }
                    let rel = i
                        .path
                        .as_str()
                        .strip_prefix(root_prefix.as_str())
                        .unwrap_or(i.path.as_str());
                    crate::global_search::item_matches_filters(
                        i.name.as_str(),
                        rel,
                        i.size,
                        i.is_dir,
                        i.mtime,
                        &filters,
                    )
                })
                .collect();

            let asc = direction == "asc";
            items.sort_unstable_by(|a, b| {
                let ord = match sort.as_str() {
                    "name" => a
                        .name
                        .as_str()
                        .to_lowercase()
                        .cmp(&b.name.as_str().to_lowercase()),
                    "mtime" => a.mtime.cmp(&b.mtime),
                    "atime" => a.atime.cmp(&b.atime),
                    _ => a.size.cmp(&b.size),
                };
                if asc {
                    ord
                } else {
                    ord.reverse()
                }
            });

            let total_matched = items.len();
            let page: Vec<Value> = items.iter().skip(offset).take(limit).map(|i| item_json(i)).collect();
            let file_count = items.iter().filter(|i| !i.is_dir).count();
            let truncated = total_matched > offset + page.len();
            let source = view.cache_source.clone().unwrap_or_else(|| "scan".into());

            Ok(tool_text(
                json!({
                    "summary": format!(
                        "{}：{} 项，合计 {}，耗时 {}ms（来源 {}）",
                        view.path,
                        total_matched,
                        crate::scan::format_size(view.total_size),
                        elapsed_ms,
                        source
                    ),
                    "path": view.path,
                    "totalItems": total_matched,
                    "totalSize": view.total_size,
                    "totalSizeFormatted": crate::scan::format_size(view.total_size),
                    "fileCount": file_count,
                    "dirCount": total_matched - file_count,
                    "elapsedMs": elapsed_ms,
                    "source": source,
                    "mftAvailable": view.mft_available,
                    "offset": offset,
                    "truncated": truncated,
                    "items": page,
                })
                .to_string(),
            ))
        }

        "cache_stats" => {
            let stats = crate::disk_cache::DiskCache::instance().get_stats();
            Ok(tool_text(
                json!({
                    "summary": format!(
                        "磁盘缓存 {} 个目录 / {:.1} MB（上限 {} MB）",
                        stats.entry_count, stats.total_size_mb, stats.max_size_mb
                    ),
                    "entryCount": stats.entry_count,
                    "totalSizeBytes": stats.total_size_bytes,
                    "totalSizeMb": stats.total_size_mb,
                    "maxSizeMb": stats.max_size_mb,
                    "oldestEntryTimestamp": stats.oldest_entry_timestamp,
                    "enabled": stats.enabled,
                })
                .to_string(),
            ))
        }

        "diagnostics" => {
            let idx = crate::global_search::instance();
            let (index_kind, index_meta) = match idx.state() {
                crate::global_search::IndexState::Ready(meta) => (
                    "ready",
                    Some(json!({
                        "fileCount": meta.file_count,
                        "dirCount": meta.dir_count,
                        "driveCount": meta.drive_count,
                        "partial": meta.partial,
                        "failedDrives": meta.failed_drives.iter().map(|c| c.to_string()).collect::<Vec<_>>(),
                    })),
                ),
                crate::global_search::IndexState::Loading { drive, scanned } => (
                    "loading",
                    Some(json!({ "drive": drive.to_string(), "scanned": scanned })),
                ),
                crate::global_search::IndexState::Failed { reason } => {
                    ("failed", Some(json!({ "reason": reason })))
                }
                crate::global_search::IndexState::NotLoaded => ("notLoaded", None),
            };
            let cache = crate::disk_cache::DiskCache::instance().get_stats();
            // fs::mft_scanner 是私有模块；管理员判断用公开入口
            let is_admin = crate::fs::is_admin();
            let volume_list: Vec<Value> = crate::volumes::list_volumes()
                .iter()
                .map(|v| {
                    json!({
                        "letter": v.letter,
                        "fs": v.fs,
                        "isNtfs": v.is_ntfs,
                        "totalBytes": v.total_bytes,
                        "freeBytes": v.free_bytes,
                    })
                })
                .collect();
            Ok(tool_text(
                json!({
                    "summary": format!(
                        "FlashDir {} · 管理员={} · 索引={}（{} 项）· 缓存 {} 个目录",
                        env!("CARGO_PKG_VERSION"), is_admin, index_kind, idx.entries_len(), cache.entry_count
                    ),
                    "version": env!("CARGO_PKG_VERSION"),
                    "isAdmin": is_admin,
                    "mftDirectRead": is_admin,
                    "indexState": index_kind,
                    "indexEntries": idx.entries_len(),
                    "indexMeta": index_meta,
                    "diskCache": {
                        "entryCount": cache.entry_count,
                        "totalSizeMb": cache.total_size_mb,
                        "maxSizeMb": cache.max_size_mb,
                    },
                    "volumes": volume_list,
                })
                .to_string(),
            ))
        }

        other => Err(format!("未知工具: {}", other)),
    }
}

/// 等待全局索引就绪（最多 timeout_ms 毫秒）。索引由 serve() 在后台加载。
async fn wait_index_ready(idx: &crate::global_search::GlobalIndex, timeout_ms: u64) -> bool {
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
    loop {
        if matches!(idx.state(), crate::global_search::IndexState::Ready(..)) {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
}

fn name_from_path(path: &str) -> String {
    path.rsplit_once('/')
        .map(|(_, n)| n.to_string())
        .unwrap_or_else(|| path.to_string())
}

/* ─── 主循环与自测 ─────────────────────────────────────────── */

fn write_line(out: &mut impl Write, line: &str) {
    // 单行输出 + 立即 flush：Host 侧是逐行读取
    let _ = out.write_all(line.as_bytes());
    let _ = out.write_all(b"\n");
    let _ = out.flush();
}

pub async fn serve_stdio() {
    eprintln!(
        "[MCP] FlashDir {} 启动（stdio / JSON-RPC 2.0 / NDJSON）",
        env!("CARGO_PKG_VERSION")
    );

    // 后台加载持久化全局索引（与 GUI 相同），让 search_files 尽快可用
    {
        let idx = crate::global_search::instance();
        std::thread::spawn(move || {
            idx.load_persisted();
            eprintln!("[MCP] 全局索引已就绪：{} 项", idx.entries_len());
        });
    }

    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    let reader = std::io::BufReader::new(stdin.lock());

    for line in reader.lines() {
        match line {
            Ok(l) => {
                if let Some(resp) = handle_line(&l).await {
                    write_line(&mut stdout, &resp);
                }
            }
            Err(e) => {
                eprintln!("[MCP] 读取 stdin 失败: {}", e);
                break;
            }
        }
    }
    eprintln!("[MCP] stdin 关闭，退出");
}

/// 自测：脚本化会话，校验协议与工具返回结构（不依赖 Host）
pub async fn selftest() -> i32 {
    let mut failures = 0usize;
    let mut check = |name: &str, ok: bool, detail: &str| {
        if ok {
            eprintln!("  [PASS] {}", name);
        } else {
            failures += 1;
            eprintln!("  [FAIL] {} → {}", name, detail);
        }
    };

    // 1) initialize
    let req = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"selftest","version":"0"}}}"#;
    let resp: Value = serde_json::from_str(&handle_line(req).await.unwrap_or_default()).unwrap_or(Value::Null);
    check(
        "initialize 返回协议版本与 serverInfo",
        resp["result"]["protocolVersion"].is_string()
            && resp["result"]["serverInfo"]["name"] == "flashdir",
        &resp.to_string(),
    );
    check(
        "initialize 声明 tools 能力",
        resp["result"]["capabilities"]["tools"].is_object(),
        &resp.to_string(),
    );

    // 2) notification 不回复
    let n = handle_line(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#).await;
    check("notification 不产生响应", n.is_none(), "expected None");

    // 3) tools/list
    let resp: Value =
        serde_json::from_str(&handle_line(r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#).await.unwrap_or_default())
            .unwrap_or(Value::Null);
    let tools = resp["result"]["tools"].as_array().cloned().unwrap_or_default();
    check("tools/list 返回工具清单", tools.len() >= 6, &format!("{} tools", tools.len()));
    let all_have_schema = tools.iter().all(|t| t["inputSchema"]["type"] == "object" && t["name"].is_string());
    check("每个工具都有 name 与 inputSchema", all_have_schema, "");

    // 4) 未知方法 → -32601
    let resp: Value = serde_json::from_str(
        &handle_line(r#"{"jsonrpc":"2.0","id":3,"method":"no/such/method"}"#).await.unwrap_or_default(),
    )
    .unwrap_or(Value::Null);
    check("未知方法返回 -32601", resp["error"]["code"] == -32601, &resp.to_string());

    // 5) 非法 JSON → -32700
    let resp: Value = serde_json::from_str(&handle_line("{not json").await.unwrap_or_default()).unwrap_or(Value::Null);
    check("非法 JSON 返回 -32700", resp["error"]["code"] == -32700, &resp.to_string());

    // 6) tools/call list_volumes → 文本是合法 JSON 且含 volumes 数组
    let resp: Value = serde_json::from_str(
        &handle_line(
            r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"list_volumes","arguments":{}}}"#,
        )
        .await
        .unwrap_or_default(),
    )
    .unwrap_or(Value::Null);
    let text = resp["result"]["content"][0]["text"].as_str().unwrap_or("");
    let parsed: Value = serde_json::from_str(text).unwrap_or(Value::Null);
    check(
        "tools/call list_volumes 返回可解析 JSON",
        resp["result"]["isError"] == false && parsed["volumes"].is_array(),
        text,
    );

    // 7) tools/call search_files → 结构正确（索引可能未就绪，也必须是合法响应）
    let resp: Value = serde_json::from_str(
        &handle_line(
            r#"{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"search_files","arguments":{"query":"*.exe","limit":5}}}"#,
        )
        .await
        .unwrap_or_default(),
    )
    .unwrap_or(Value::Null);
    let text = resp["result"]["content"][0]["text"].as_str().unwrap_or("");
    let parsed: Value = serde_json::from_str(text).unwrap_or(Value::Null);
    check(
        "tools/call search_files 返回结构正确",
        parsed.get("total").is_some() || parsed.get("hint").is_some(),
        text,
    );

    // 8) tools/call 参数缺失 → isError
    let resp: Value = serde_json::from_str(
        &handle_line(
            r#"{"jsonrpc":"2.0","id":6,"method":"tools/call","params":{"name":"scan_directory","arguments":{}}}"#,
        )
        .await
        .unwrap_or_default(),
    )
    .unwrap_or(Value::Null);
    check(
        "缺参数时返回 isError 而不是崩溃",
        resp["result"]["isError"] == true,
        &resp.to_string(),
    );

    // 9) diagnostics 可调用
    let resp: Value = serde_json::from_str(
        &handle_line(
            r#"{"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":"diagnostics","arguments":{}}}"#,
        )
        .await
        .unwrap_or_default(),
    )
    .unwrap_or(Value::Null);
    let text = resp["result"]["content"][0]["text"].as_str().unwrap_or("");
    let parsed: Value = serde_json::from_str(text).unwrap_or(Value::Null);
    check(
        "tools/call diagnostics 返回结构正确",
        parsed["version"].is_string() && parsed["volumes"].is_array(),
        text,
    );

    if failures == 0 {
        eprintln!("[MCP] 自测全部通过 ✅");
        0
    } else {
        eprintln!("[MCP] 自测失败 {} 项 ❌", failures);
        1
    }
}

/* ─── 本机端点（GUI 内后台线程）───────────────────────────── */

/// 端点信息文件：`~/.flashdir/mcp-endpoint.json`
/// 内容 `{ "port": 12345, "token": "<随机>" }`，仅当前用户可读（用户目录默认 ACL）
pub fn endpoint_file_path() -> Option<std::path::PathBuf> {
    let home = std::env::var("USERPROFILE").ok()?;
    Some(std::path::PathBuf::from(home).join(".flashdir").join("mcp-endpoint.json"))
}

fn random_token() -> String {
    // 不引入 rand 依赖：用时间 + 进程号 + 地址熵拼一个 128 位 hex
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let pid = std::process::id() as u128;
    let stack = &now as *const _ as u128;
    let mut x = now ^ (pid << 64) ^ stack.rotate_left(17);
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    format!("{:032x}", x)
}

/// 当前端点信息（未启动时返回 null）
pub fn endpoint_info() -> Value {
    match endpoint_file_path().and_then(|p| std::fs::read_to_string(p).ok()) {
        Some(text) => serde_json::from_str(&text).unwrap_or(Value::Null),
        None => Value::Null,
    }
}

/// 在 GUI 内启动本机端点（阻塞循环，放在后台线程里跑）
pub async fn serve_endpoint() {
    let token = random_token();
    let listener = match tokio::net::TcpListener::bind("127.0.0.1:0").await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("[MCP] 本机端点监听失败: {}", e);
            return;
        }
    };
    let port = listener.local_addr().map(|a| a.port()).unwrap_or(0);
    if let Some(path) = endpoint_file_path() {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let payload = json!({ "port": port, "token": token, "pid": std::process::id() });
        if std::fs::write(&path, payload.to_string()).is_ok() {
            eprintln!("[MCP] 本机端点已就绪: 127.0.0.1:{}", port);
        }
    }

    loop {
        let (stream, _) = match listener.accept().await {
            Ok(v) => v,
            Err(e) => {
                eprintln!("[MCP] accept 失败: {}", e);
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                continue;
            }
        };
        // 每个连接独立任务：桥可能重连，且不能因为一条长连接阻塞其它客户端
        let expected = token.clone();
        tokio::spawn(async move {
            conn_opened();
            if let Err(e) = handle_endpoint_conn(stream, &expected).await {
                eprintln!("[MCP] 端点连接结束: {}", e);
            }
            conn_closed();
        });
    }
}

/// 单个端点连接：先校验 token，然后与 stdio 相同的 NDJSON 协议
async fn handle_endpoint_conn(
    stream: tokio::net::TcpStream,
    expected_token: &str,
) -> Result<(), String> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    let (read_half, mut write_half) = stream.into_split();
    let mut lines = BufReader::new(read_half).lines();

    // 1) 握手：第一行必须是 {"token":"..."}，超时保护
    let first = tokio::time::timeout(
        std::time::Duration::from_millis(HANDSHAKE_TIMEOUT_MS),
        lines.next_line(),
    )
    .await
    .map_err(|_| "握手超时".to_string())?
    .map_err(|e| e.to_string())?
    .ok_or("连接被关闭".to_string())?;

    let ok = serde_json::from_str::<Value>(&first)
        .ok()
        .and_then(|v| v.get("token").and_then(|t| t.as_str()).map(|t| t == expected_token))
        .unwrap_or(false);
    if !ok {
        let _ = write_half.write_all("{\"error\":\"token 无效\"}\n".as_bytes()).await;
        return Err("token 校验失败".into());
    }
    let _ = write_half.write_all(b"{\"ok\":true}\n").await;
    let _ = write_half.flush().await;

    // 2) 协议循环
    while let Some(line) = lines.next_line().await.map_err(|e| e.to_string())? {
        if let Some(resp) = handle_line(&line).await {
            write_half
                .write_all(resp.as_bytes())
                .await
                .map_err(|e| e.to_string())?;
            write_half.write_all(b"\n").await.map_err(|e| e.to_string())?;
            write_half.flush().await.map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

/* ─── 桥接模式（Host 的 stdio ↔ GUI 本机端点）───────────────── */

fn read_endpoint() -> Option<(u16, String)> {
    let path = endpoint_file_path()?;
    let text = std::fs::read_to_string(path).ok()?;
    let v: Value = serde_json::from_str(&text).ok()?;
    let port = v.get("port")?.as_u64()? as u16;
    let token = v.get("token")?.as_str()?.to_string();
    Some((port, token))
}

/// 拉起桌面端（与桥同目录的 flashdir.exe）
fn launch_gui() {
    let Ok(exe) = std::env::current_exe() else { return };
    let Some(dir) = exe.parent() else { return };
    let gui = dir.join("flashdir.exe");
    if !gui.exists() {
        eprintln!("[MCP] 未找到桌面端: {}", gui.display());
        return;
    }
    match std::process::Command::new(&gui).spawn() {
        Ok(_) => eprintln!("[MCP] 已拉起桌面端: {}", gui.display()),
        Err(e) => eprintln!("[MCP] 拉起桌面端失败: {}", e),
    }
}

/// 等待端点可用（轮询端点文件 + 试连）
async fn wait_endpoint(timeout_ms: u64) -> Option<(tokio::net::TcpStream, u16, String)> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
    loop {
        if let Some((port, token)) = read_endpoint() {
            if let Ok(stream) = tokio::net::TcpStream::connect(("127.0.0.1", port)).await {
                return Some((stream, port, token));
            }
        }
        if std::time::Instant::now() >= deadline {
            return None;
        }
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }
}

/// 桥接：把 Host 的 stdio 与桌面端端点对接；桌面端未运行则自动拉起
pub async fn serve_bridge() -> i32 {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    let mut endpoint = read_endpoint();
    if endpoint.is_none() {
        launch_gui();
    }

    let (mut stream, port, token) = match wait_endpoint(GUI_WAIT_TIMEOUT_MS).await {
        Some(v) => v,
        None => {
            eprintln!(
                "[MCP] 等待桌面端端点超时（{}ms）。请先启动 FlashDir 桌面端，或改用 --mcp 独立模式。",
                GUI_WAIT_TIMEOUT_MS
            );
            return 2;
        }
    };

    // 握手：token 校验（响应不转发给 Host）
    let hello = json!({ "token": token }).to_string();
    if stream.write_all(hello.as_bytes()).await.is_err()
        || stream.write_all(b"\n").await.is_err()
        || stream.flush().await.is_err()
    {
        eprintln!("[MCP] 端点握手写入失败");
        return 3;
    }
    let (read_half, write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half);
    let mut first = String::new();
    match tokio::time::timeout(
        std::time::Duration::from_millis(HANDSHAKE_TIMEOUT_MS),
        reader.read_line(&mut first),
    )
    .await
    {
        Ok(Ok(n)) if n > 0 => {
            if !first.contains("\"ok\":true") {
                eprintln!("[MCP] 端点拒绝连接: {}", first.trim());
                return 4;
            }
        }
        _ => {
            eprintln!("[MCP] 端点握手读取失败");
            return 4;
        }
    }
    eprintln!("[MCP] 已连接桌面端端点 127.0.0.1:{}（共享其索引与扫描缓存）", port);
    let mut write_half = write_half;

    // 双向泵：stdin → 端点；端点 → stdout
    let to_endpoint = tokio::spawn(async move {
        let mut stdin = tokio::io::stdin();
        let _ = tokio::io::copy(&mut stdin, &mut write_half).await;
        let _ = write_half.shutdown().await;
    });
    let mut to_host = tokio::spawn(async move {
        let mut stdout = tokio::io::stdout();
        let _ = tokio::io::copy(&mut reader, &mut stdout).await;
        let _ = stdout.flush().await;
    });

    tokio::select! {
        _ = to_endpoint => {
            // Host 关闭了 stdin（会话结束）：先把端点侧剩余响应排空再退出，
            // 否则最后一批响应会被进程退出吞掉（实测一次性管道调用会 0 响应）。
            let _ = tokio::time::timeout(std::time::Duration::from_secs(5), &mut to_host).await;
        }
        _ = &mut to_host => {}
    }
    let _ = endpoint.take();
    0
}

/* ─── 端点/桥接自测 ───────────────────────────────────────── */

/// 自测本机端点：启动端点 → 连接 → 握手 → 跑一次 tools/list 与 list_volumes
pub async fn selftest_endpoint() -> i32 {
    let mut failures = 0usize;
    let mut check = |name: &str, ok: bool, detail: &str| {
        if ok {
            eprintln!("  [PASS] {}", name);
        } else {
            failures += 1;
            eprintln!("  [FAIL] {} → {}", name, detail);
        }
    };

    // 后台启动端点（会写端点文件）
    tokio::spawn(async move { serve_endpoint().await });
    tokio::time::sleep(std::time::Duration::from_millis(400)).await;

    let (mut stream, port, token) = match wait_endpoint(3000).await {
        Some(v) => v,
        None => {
            eprintln!("  [FAIL] 端点未就绪");
            return 1;
        }
    };
    check("端点已监听并可连接", true, &format!("port={}", port));

    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let hello = json!({ "token": token }).to_string();
    let _ = stream.write_all(hello.as_bytes()).await;
    let _ = stream.write_all(b"\n").await;
    let _ = stream.flush().await;

    let (r, mut w) = stream.into_split();
    let mut lines = BufReader::new(r).lines();

    let ack = tokio::time::timeout(std::time::Duration::from_secs(5), lines.next_line())
        .await
        .ok()
        .and_then(|r| r.ok())
        .flatten()
        .unwrap_or_default();
    check("token 握手成功", ack.contains("\"ok\":true"), &ack);

    // 错误 token 必须被拒
    if let Some((_, _bad)) = read_endpoint() {
        if let Ok(mut s2) = tokio::net::TcpStream::connect(("127.0.0.1", port)).await {
            let _ = s2.write_all(b"{\"token\":\"wrong\"}\n").await;
            let _ = s2.flush().await;
            let (r2, _w2) = s2.into_split();
            let mut l2 = BufReader::new(r2).lines();
            let resp = tokio::time::timeout(std::time::Duration::from_secs(5), l2.next_line())
                .await
                .ok()
                .and_then(|r| r.ok())
                .flatten()
                .unwrap_or_default();
            check("错误 token 被拒绝", !resp.contains("\"ok\":true"), &resp);
        }
    }

    // 正常请求
    for (i, (req, expect)) in [
        (
            r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#,
            "tools",
        ),
        (
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"list_volumes","arguments":{}}}"#,
            "volumes",
        ),
    ]
    .iter()
    .enumerate()
    {
        let _ = w.write_all(req.as_bytes()).await;
        let _ = w.write_all(b"\n").await;
        let _ = w.flush().await;
        let line = tokio::time::timeout(std::time::Duration::from_secs(10), lines.next_line())
            .await
            .ok()
            .and_then(|r| r.ok())
            .flatten()
            .unwrap_or_default();
        // 正确做法：解析 JSON-RPC 响应，再解析 content[0].text 里的业务 JSON
        let parsed: Value = serde_json::from_str(&line).unwrap_or(Value::Null);
        let ok = if *expect == "tools" {
            parsed["result"]["tools"].is_array()
        } else {
            parsed["result"]["content"][0]["text"]
                .as_str()
                .and_then(|t| serde_json::from_str::<Value>(t).ok())
                .map(|v| v["volumes"].is_array())
                .unwrap_or(false)
        };
        check(&format!("端点请求 #{} 返回正确", i + 1), ok, &line);
    }

    if failures == 0 {
        eprintln!("[MCP] 端点自测全部通过 ✅");
        0
    } else {
        eprintln!("[MCP] 端点自测失败 {} 项 ❌", failures);
        1
    }
}

/// 自测桥接链路（需要桌面端已运行）：连端点 → 握手 → tools/call diagnostics
pub async fn selftest_bridge() -> i32 {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let Some((mut stream, port, token)) = wait_endpoint(5000).await else {
        eprintln!("[MCP] 未发现运行中的桌面端端点（请先启动 FlashDir）");
        return 2;
    };
    let hello = json!({ "token": token }).to_string();
    let _ = stream.write_all(hello.as_bytes()).await;
    let _ = stream.write_all(b"\n").await;
    let _ = stream.flush().await;
    let (r, mut w) = stream.into_split();
    let mut lines = BufReader::new(r).lines();
    let ack = tokio::time::timeout(std::time::Duration::from_secs(5), lines.next_line())
        .await
        .ok()
        .and_then(|r| r.ok())
        .flatten()
        .unwrap_or_default();
    if !ack.contains("\"ok\":true") {
        eprintln!("[MCP] 握手失败: {}", ack);
        return 3;
    }
    let req = r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"diagnostics","arguments":{}}}"#;
    let _ = w.write_all(req.as_bytes()).await;
    let _ = w.write_all(b"\n").await;
    let _ = w.flush().await;
    let line = tokio::time::timeout(std::time::Duration::from_secs(20), lines.next_line())
        .await
        .ok()
        .and_then(|r| r.ok())
        .flatten()
        .unwrap_or_default();
    let text = serde_json::from_str::<Value>(&line)
        .ok()
        .and_then(|v| {
            v.get("result")
                .and_then(|r| r.get("content"))
                .and_then(|c| c.get(0))
                .and_then(|c| c.get("text"))
                .and_then(|t| t.as_str())
                .map(|s| s.to_string())
        })
        .unwrap_or_default();
    let parsed: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
    if parsed["version"].is_string() {
        eprintln!(
            "[MCP] 桥接链路正常（127.0.0.1:{}）→ {}",
            port,
            parsed["summary"].as_str().unwrap_or("")
        );
        0
    } else {
        eprintln!("[MCP] 桥接返回异常: {}", line);
        1
    }
}

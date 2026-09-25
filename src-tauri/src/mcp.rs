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
    let enabled = load_settings().enabled;
    json!({
        "enabled": enabled,
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
    let mut tools = base_tool_definitions();
    // 统一补 MCP 工具标注：除 save_snapshot 外全部只读
    for t in tools.iter_mut() {
        let read_only = t
            .get("name")
            .and_then(|n| n.as_str())
            .map(|n| n != "save_snapshot")
            .unwrap_or(true);
        if let Some(obj) = t.as_object_mut() {
            obj.insert(
                "annotations".to_string(),
                json!({
                    "readOnlyHint": read_only,
                    "destructiveHint": false,
                    "idempotentHint": true,
                    "openWorldHint": false,
                }),
            );
        }
    }
    tools
}

fn base_tool_definitions() -> Vec<Value> {
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
        json!({
            "name": "find_large_files",
            "description": "找出目录下大于指定体积的文件（按体积降序）。适合“哪些文件占地方最大”。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "目录绝对路径" },
                    "minSizeBytes": { "type": "integer", "minimum": 0, "description": "最小体积（字节），默认 100MB" },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 1000, "description": "返回条数（默认 50）" }
                },
                "required": ["path"],
                "additionalProperties": false
            }
        }),
        json!({
            "name": "find_duplicates",
            "description": "按内容哈希检测目录内的重复文件，返回可回收空间与重复组（只读，不会删除任何文件）。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "minSizeBytes": { "type": "integer", "minimum": 0, "description": "只检测大于该体积的文件，默认 1MB" },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 200, "description": "返回组数（默认 20）" }
                },
                "required": ["path"],
                "additionalProperties": false
            }
        }),
        json!({
            "name": "analyze_dev_cache",
            "description": "分析开发类缓存占用（node_modules / target / 包管理器缓存 / 构建产物等），返回各类别体积与 Top 项。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 50, "description": "返回类别数（默认 10）" }
                },
                "required": ["path"],
                "additionalProperties": false
            }
        }),
        json!({
            "name": "list_snapshots",
            "description": "列出某目录的历史快照（时间、体积、条目数）。用于回答“这个目录最近长大了多少”。",
            "inputSchema": {
                "type": "object",
                "properties": { "path": { "type": "string" } },
                "required": ["path"],
                "additionalProperties": false
            }
        }),
        json!({
            "name": "save_snapshot",
            "description": "给当前目录保存一份快照（写入 FlashDir 自己的快照库，不修改任何用户文件）。之后可用 compare_snapshots / disk_usage_trend 观察变化。",
            "inputSchema": {
                "type": "object",
                "properties": { "path": { "type": "string" } },
                "required": ["path"],
                "additionalProperties": false
            }
        }),
        json!({
            "name": "compare_snapshots",
            "description": "对比两份快照（或快照与当前状态），返回净变化与新增/删除/修改的文件。不传 id 时自动对比该目录最近两次；newId 传 \"current\" 表示与当前状态对比。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "oldId": { "type": "integer", "description": "旧快照 id（省略则取最近两次中的旧者）" },
                    "newId": { "type": ["integer", "string"], "description": "新快照 id，或 \"current\" 表示当前状态" },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 200, "description": "每类返回条数（默认 20）" }
                },
                "required": ["path"],
                "additionalProperties": false
            }
        }),
        json!({
            "name": "disk_usage_trend",
            "description": "基于历史快照给出目录体积趋势（时间点、体积、相邻变化），用于回答“为什么变大/变小”。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "limit": { "type": "integer", "minimum": 2, "maximum": 200, "description": "最多返回多少个时间点（默认 30，取最近的）" }
                },
                "required": ["path"],
                "additionalProperties": false
            }
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

        "find_large_files" => {
            let path = arg_str(args, "path").ok_or("缺少参数 path")?;
            let min_bytes = args
                .get("minSizeBytes")
                .and_then(|v| v.as_u64())
                .unwrap_or(100 * 1024 * 1024);
            let limit = arg_usize(args, "limit", 50).clamp(1, 1000);
            let (items, source, _) = items_for(&path, false).await?;
            let mut hits: Vec<&crate::scan::Item> = items
                .iter()
                .filter(|i| !i.is_dir && i.size >= min_bytes as i64)
                .collect();
            hits.sort_unstable_by(|a, b| b.size.cmp(&a.size));
            let total = hits.len();
            let page: Vec<Value> = hits.iter().take(limit).map(|i| item_json(i)).collect();
            Ok(tool_text(
                json!({
                    "summary": format!(
                        "{} 个文件大于 {}（共 {} 项扫描来源 {}）",
                        total,
                        crate::scan::format_size(min_bytes as i64),
                        items.len(),
                        source
                    ),
                    "path": path,
                    "minSizeBytes": min_bytes,
                    "total": total,
                    "truncated": total > page.len(),
                    "files": page,
                })
                .to_string(),
            ))
        }

        "find_duplicates" => {
            let path = arg_str(args, "path").ok_or("缺少参数 path")?;
            let min_bytes = args
                .get("minSizeBytes")
                .and_then(|v| v.as_u64())
                .unwrap_or(1024 * 1024) as i64;
            let limit = arg_usize(args, "limit", 20).clamp(1, 200);
            let (items, source, _) = items_for(&path, false).await?;
            let res = crate::duplicate_finder::find_duplicates(&items, min_bytes);
            let groups: Vec<Value> = res
                .groups
                .iter()
                .take(limit)
                .map(|g| {
                    json!({
                        "size": g.size,
                        "sizeFormatted": g.size_formatted,
                        "fileCount": g.file_count,
                        "wasted": g.wasted_bytes,
                        "wastedFormatted": g.wasted_formatted,
                        // 每组最多列 5 个路径，避免响应过大
                        "files": g.files.iter().take(5).map(|f| f.path.clone()).collect::<Vec<_>>(),
                    })
                })
                .collect();
            Ok(tool_text(
                json!({
                    "summary": format!(
                        "重复文件 {} 组 / {} 个文件，可回收 {}（来源 {}）",
                        res.total_groups, res.total_files, res.total_wasted_formatted, source
                    ),
                    "path": path,
                    "totalGroups": res.total_groups,
                    "totalFiles": res.total_files,
                    "totalWastedBytes": res.total_wasted_bytes,
                    "totalWastedFormatted": res.total_wasted_formatted,
                    "truncated": res.total_groups > groups.len(),
                    "groups": groups,
                })
                .to_string(),
            ))
        }

        "analyze_dev_cache" => {
            let path = arg_str(args, "path").ok_or("缺少参数 path")?;
            let limit = arg_usize(args, "limit", 10).clamp(1, 50);
            let (items, source, total_size) = items_for(&path, false).await?;
            let res = crate::dev_analyzer::analyze(&items, total_size, items.len());
            let cats: Vec<Value> = res
                .categories
                .iter()
                .take(limit)
                .map(|c| {
                    json!({
                        "category": c.category,
                        "label": c.label,
                        "description": c.description,
                        "itemCount": c.item_count,
                        "fileCount": c.file_count,
                        "dirCount": c.dir_count,
                        "totalSize": c.total_size,
                        "totalSizeFormatted": c.total_size_formatted,
                        "percentOfDev": c.percent_of_dev,
                        "topItems": c.top_items.iter().take(5).map(|t| json!({
                            "name": t.name,
                            "size": t.size,
                            "sizeFormatted": t.size_formatted,
                        })).collect::<Vec<_>>(),
                    })
                })
                .collect();
            Ok(tool_text(
                json!({
                    "summary": format!(
                        "开发类占用 {}（占全部 {:.1}%，{} 项命中；来源 {}）",
                        crate::scan::format_size(res.dev_total_size),
                        res.dev_percent,
                        res.dev_items,
                        source
                    ),
                    "path": path,
                    "devTotalSize": res.dev_total_size,
                    "devPercent": res.dev_percent,
                    "devItems": res.dev_items,
                    "totalItems": res.total_items,
                    "categories": cats,
                })
                .to_string(),
            ))
        }

        "list_snapshots" => {
            let path = arg_str(args, "path").ok_or("缺少参数 path")?;
            let list = crate::disk_cache::DiskCache::instance()
                .list_snapshots(&path)
                .map_err(|e| format!("读取快照失败: {}", e))?;
            let snaps: Vec<Value> = list
                .iter()
                .map(|s| {
                    json!({
                        "id": s.id,
                        "scanTime": s.scan_time,
                        "totalSize": s.total_size,
                        "totalSizeFormatted": s.total_size_formatted,
                        "itemCount": s.item_count,
                        "fileCount": s.file_count,
                        "dirCount": s.dir_count,
                    })
                })
                .collect();
            Ok(tool_text(
                json!({
                    "summary": format!("{} 共 {} 份快照", path, snaps.len()),
                    "path": path,
                    "count": snaps.len(),
                    "snapshots": snaps,
                })
                .to_string(),
            ))
        }

        "save_snapshot" => {
            let path = arg_str(args, "path").ok_or("缺少参数 path")?;
            let (items, source, total_size) = items_for(&path, false).await?;
            let file_count = items.iter().filter(|i| !i.is_dir).count();
            let dir_count = items.len() - file_count;
            let id = crate::disk_cache::DiskCache::instance()
                .insert_snapshot(
                    &path,
                    &items,
                    total_size,
                    &crate::scan::format_size(total_size),
                    file_count,
                    dir_count,
                )
                .map_err(|e| format!("保存快照失败: {}", e))?;
            Ok(tool_text(
                json!({
                    "summary": format!(
                        "已为 {} 保存快照 #{}（{} 项 / {}，来源 {}）",
                        path, id, items.len(), crate::scan::format_size(total_size), source
                    ),
                    "id": id,
                    "path": path,
                    "itemCount": items.len(),
                    "fileCount": file_count,
                    "dirCount": dir_count,
                    "totalSize": total_size,
                    "source": source,
                })
                .to_string(),
            ))
        }

        "compare_snapshots" => {
            let path = arg_str(args, "path").ok_or("缺少参数 path")?;
            let limit = arg_usize(args, "limit", 20).clamp(1, 200);
            let cache = crate::disk_cache::DiskCache::instance();
            let list = cache
                .list_snapshots(&path)
                .map_err(|e| format!("读取快照失败: {}", e))?;
            if list.is_empty() {
                return Ok(tool_text(
                    json!({
                        "summary": format!("{} 还没有快照，请先调用 save_snapshot 建立基准", path),
                        "path": path,
                        "snapshotCount": 0,
                    })
                    .to_string(),
                ));
            }

            let wants_current = args
                .get("newId")
                .and_then(|v| v.as_str())
                .map(|s| s.eq_ignore_ascii_case("current"))
                .unwrap_or(false);

            // 选定对照的旧快照与"新状态"
            let (old_id, new_label, new_items, new_total): (i64, String, Vec<crate::scan::Item>, i64) =
                if wants_current {
                    let old = &list[0];
                    let (items, source, total) = items_for(&path, false).await?;
                    (old.id, format!("当前状态（{}）", source), (*items).clone(), total)
                } else if let Some(n) = args.get("newId").and_then(|v| v.as_i64()) {
                    let o = args
                        .get("oldId")
                        .and_then(|v| v.as_i64())
                        .or_else(|| list.iter().map(|s| s.id).find(|id| *id != n))
                        .ok_or("需要两份不同的快照")?;
                    let ni = cache.get_snapshot(n).ok_or_else(|| format!("快照 {} 不存在", n))?;
                    (o, format!("#{}", n), ni.items, ni.total_size)
                } else {
                    if list.len() < 2 {
                        return Ok(tool_text(
                            json!({
                                "summary": format!("{} 需要至少两份快照，当前 {} 份", path, list.len()),
                                "path": path,
                                "snapshotCount": list.len(),
                            })
                            .to_string(),
                        ));
                    }
                    let newer = &list[0];
                    let older = &list[1];
                    let ni = cache
                        .get_snapshot(newer.id)
                        .ok_or_else(|| format!("快照 {} 不存在", newer.id))?;
                    (older.id, format!("#{}", newer.id), ni.items, ni.total_size)
                };

            let old = cache
                .get_snapshot(old_id)
                .ok_or_else(|| format!("快照 {} 不存在", old_id))?;
            let d = crate::diff_engine::diff(&old.items, &new_items, old.total_size);

            let keep = |items: Vec<Value>| items.into_iter().take(limit).collect::<Vec<_>>();
            let added = keep(
                d.added
                    .iter()
                    .map(|i| json!({ "path": i.path, "size": i.size, "isDir": i.is_dir }))
                    .collect(),
            );
            let removed = keep(
                d.removed
                    .iter()
                    .map(|i| json!({ "path": i.path, "size": i.size, "isDir": i.is_dir }))
                    .collect(),
            );
            let modified = keep(
                d.modified
                    .iter()
                    .map(|i| {
                        json!({
                            "path": i.path,
                            "delta": i.delta,
                            "oldSize": i.old_size,
                            "newSize": i.new_size,
                        })
                    })
                    .collect(),
            );

            Ok(tool_text(
                json!({
                    "summary": format!(
                        "#{} → {}：{} → {}，净变化 {}（新增 {} / 删除 {} / 修改 {}）",
                        old_id, new_label, old.total_size_formatted,
                        crate::scan::format_size(new_total),
                        crate::scan::format_size(d.net_change),
                        d.added.len(), d.removed.len(), d.modified.len()
                    ),
                    "path": path,
                    "oldId": old_id,
                    "newLabel": new_label,
                    "oldTotalSize": old.total_size,
                    "newTotalSize": new_total,
                    "netChange": d.net_change,
                    "addedTotalSize": d.added_total_size,
                    "removedTotalSize": d.removed_total_size,
                    "modifiedDelta": d.modified_delta,
                    "addedCount": d.added.len(),
                    "removedCount": d.removed.len(),
                    "modifiedCount": d.modified.len(),
                    "added": added,
                    "removed": removed,
                    "modified": modified,
                })
                .to_string(),
            ))
        }

        "disk_usage_trend" => {
            let path = arg_str(args, "path").ok_or("缺少参数 path")?;
            let limit = arg_usize(args, "limit", 30).clamp(2, 200);
            let list = crate::disk_cache::DiskCache::instance()
                .list_snapshots(&path)
                .map_err(|e| format!("读取快照失败: {}", e))?;
            if list.is_empty() {
                return Ok(tool_text(
                    json!({
                        "summary": format!("{} 还没有快照，先用 save_snapshot 建立基准", path),
                        "path": path,
                        "snapshotCount": 0,
                        "points": [],
                    })
                    .to_string(),
                ));
            }
            // list_snapshots 按时间倒序 → 反转取最近 limit 个点
            let mut asc: Vec<&crate::disk_cache::SnapshotInfo> = list.iter().collect();
            asc.reverse();
            let skip = asc.len().saturating_sub(limit);
            let window = &asc[skip..];
            let mut points: Vec<Value> = Vec::new();
            let mut prev: Option<i64> = None;
            for s in window {
                points.push(json!({
                    "id": s.id,
                    "scanTime": s.scan_time,
                    "totalSize": s.total_size,
                    "totalSizeFormatted": s.total_size_formatted,
                    "itemCount": s.item_count,
                    "delta": prev.map(|p| s.total_size - p),
                }));
                prev = Some(s.total_size);
            }
            let first = window.first().map(|s| s.total_size).unwrap_or(0);
            let last = window.last().map(|s| s.total_size).unwrap_or(0);
            let net = last - first;
            let pct = if first > 0 {
                (net as f64 / first as f64) * 100.0
            } else {
                0.0
            };
            Ok(tool_text(
                json!({
                    "summary": format!(
                        "{} 共 {} 份快照（展示最近 {} 个点）：{} → {}，净变化 {}{:.1}%",
                        path, list.len(), points.len(),
                        crate::scan::format_size(first), crate::scan::format_size(last),
                        if net >= 0 { "+" } else { "" }, pct
                    ),
                    "path": path,
                    "snapshotCount": list.len(),
                    "netChange": net,
                    "netChangePercent": pct,
                    "points": points,
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

/// 取目录条目：优先内存缓存（含 USN 校验结果），否则做一次完整扫描。
/// 返回 (条目, 来源, 文件总大小)。
async fn items_for(
    path: &str,
    force: bool,
) -> Result<(Arc<Vec<crate::scan::Item>>, String, i64), String> {
    if !force {
        if let Some(items) = crate::scan::get_cached_items(path) {
            let total: i64 = items.iter().filter(|i| !i.is_dir).map(|i| i.size).sum();
            return Ok((items, "memory-cache".to_string(), total));
        }
    }
    let perf = crate::perf::PerformanceMonitor::instance();
    let view = crate::scan::scan_directory_view(path, force, perf, None)
        .await
        .map_err(|e| format!("扫描失败: {}", e))?;
    let source = view.cache_source.clone().unwrap_or_else(|| "scan".to_string());
    let total_size = view.total_size;
    let result = view.into_scan_result();
    Ok((Arc::new(result.items), source, total_size))
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

/* ─── 本机 HTTP 端点（GUI 内后台线程）──────────────────────── */

/// 配置里写死的默认端口：`http://127.0.0.1:47821/mcp`
/// 被占用时自动顺延（实际端口写在端点文件与设置页里）
pub const DEFAULT_MCP_PORT: u16 = 47821;
const PORT_FALLBACKS: u16 = 5;

fn flashdir_dir() -> Option<std::path::PathBuf> {
    let home = std::env::var("USERPROFILE").ok()?;
    Some(std::path::PathBuf::from(home).join(".flashdir"))
}

/// 端点信息文件：`~/.flashdir/mcp-endpoint.json`
pub fn endpoint_file_path() -> Option<std::path::PathBuf> {
    Some(flashdir_dir()?.join("mcp-endpoint.json"))
}

fn token_file_path() -> Option<std::path::PathBuf> {
    Some(flashdir_dir()?.join("mcp-token"))
}

fn random_hex() -> String {
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

/// 持久 token：首次生成后固定保存在 `~/.flashdir/mcp-token`，
/// 这样配置里的 URL 长期有效（跨桌面端重启也不需要改配置）。
pub fn persistent_token() -> String {
    if let Some(path) = token_file_path() {
        if let Ok(text) = std::fs::read_to_string(&path) {
            let t = text.trim().to_string();
            if t.len() >= 16 {
                return t;
            }
        }
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let token = random_hex();
        let _ = std::fs::write(&path, &token);
        return token;
    }
    random_hex()
}

/// 当前端点信息（桌面端未运行时为 null）
pub fn endpoint_info() -> Value {
    match endpoint_file_path().and_then(|p| std::fs::read_to_string(p).ok()) {
        Some(text) => serde_json::from_str(&text).unwrap_or(Value::Null),
        None => Value::Null,
    }
}

/// MCP 的 HTTP 地址（含 token），供设置页展示/复制
pub fn endpoint_url() -> String {
    let token = persistent_token();
    if let Some(info) = endpoint_info().as_object() {
        if let Some(port) = info.get("port").and_then(|p| p.as_u64()) {
            return format!("http://127.0.0.1:{}/mcp?token={}", port, token);
        }
    }
    // 端点未运行时按设置里的端口展示（用户复制的地址与启用后的实际地址一致）
    format!("http://127.0.0.1:{}/mcp?token={}", load_settings().port, token)
}

fn write_endpoint_file(port: u16) {
    if let Some(path) = endpoint_file_path() {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let payload = json!({
            "port": port,
            "pid": std::process::id(),
            "url": format!("http://127.0.0.1:{}/mcp", port),
        });
        let _ = std::fs::write(&path, payload.to_string());
    }
}

/// MCP 端点设置：`~/.flashdir/mcp-settings.json`
#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
pub struct McpSettings {
    /// 是否启用本机端点（关闭后彻底不监听）
    pub enabled: bool,
    /// 起始端口（被占用时自动顺延）
    pub port: u16,
}

impl Default for McpSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            port: DEFAULT_MCP_PORT,
        }
    }
}

fn settings_file_path() -> Option<std::path::PathBuf> {
    Some(flashdir_dir()?.join("mcp-settings.json"))
}

pub fn load_settings() -> McpSettings {
    settings_file_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|t| serde_json::from_str::<McpSettings>(&t).ok())
        .map(|s| McpSettings {
            enabled: s.enabled,
            port: if s.port < 1024 { DEFAULT_MCP_PORT } else { s.port },
        })
        .unwrap_or_default()
}

pub fn save_settings(s: &McpSettings) -> Result<(), String> {
    let path = settings_file_path().ok_or("无法定位配置目录")?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let text = serde_json::to_string_pretty(s).map_err(|e| e.to_string())?;
    std::fs::write(&path, text).map_err(|e| format!("写入设置失败: {}", e))
}

/// 设置 + 运行状态（桌面端设置页使用）
pub fn settings_json() -> Value {
    let s = load_settings();
    json!({
        "enabled": s.enabled,
        "port": s.port,
        "defaultPort": DEFAULT_MCP_PORT,
        "url": http_url_for_display(),
        "endpoint": endpoint_info(),
        "status": status_json(),
    })
}

fn remove_endpoint_file() {
    if let Some(path) = endpoint_file_path() {
        if let Some(info) = endpoint_info().as_object() {
            let ours = info
                .get("pid")
                .and_then(|p| p.as_u64())
                .map(|p| p == std::process::id() as u64)
                .unwrap_or(false);
            if ours {
                let _ = std::fs::remove_file(path);
            }
        }
    }
}

/// 绑定端点端口（base 起顺延 PORT_FALLBACKS 次）
async fn bind_endpoint(base_port: u16) -> Option<(tokio::net::TcpListener, u16)> {
    for offset in 0..PORT_FALLBACKS {
        let p = base_port.saturating_add(offset);
        match tokio::net::TcpListener::bind(("127.0.0.1", p)).await {
            Ok(l) => return Some((l, p)),
            Err(e) => eprintln!("[MCP] 端口 {} 不可用: {}", p, e),
        }
    }
    None
}

/// 连接处理循环；`watch` 为 true 时每秒检查设置，配置变化即返回（由外层重新绑定）
async fn serve_connections(
    listener: tokio::net::TcpListener,
    token: String,
    port: u16,
    watch: bool,
) {
    loop {
        if watch {
            let s = load_settings();
            if !s.enabled || s.port != port {
                return;
            }
        }
        match tokio::time::timeout(std::time::Duration::from_millis(1000), listener.accept()).await {
            Ok(Ok((stream, _))) => {
                let token = token.clone();
                tokio::spawn(async move {
                    conn_opened();
                    if let Err(e) = handle_http_conn(stream, &token).await {
                        if e != "client closed" {
                            eprintln!("[MCP] 连接结束: {}", e);
                        }
                    }
                    conn_closed();
                });
            }
            Ok(Err(e)) => {
                eprintln!("[MCP] accept 失败: {}", e);
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            }
            Err(_) => {} // 超时：回到循环顶部检查设置
        }
    }
}

/// 桌面端启动时调用：按设置绑定端点，并**热响应**开关与端口变化
pub async fn serve_endpoint() {
    let token = persistent_token();
    loop {
        let s = load_settings();
        if !s.enabled {
            remove_endpoint_file();
            tokio::time::sleep(std::time::Duration::from_millis(700)).await;
            continue;
        }
        let Some((listener, port)) = bind_endpoint(s.port).await else {
            tokio::time::sleep(std::time::Duration::from_millis(1000)).await;
            continue;
        };
        write_endpoint_file(port);
        eprintln!(
            "[MCP] HTTP 端点已就绪: http://127.0.0.1:{}/mcp（配置里可直接使用该地址）",
            port
        );
        serve_connections(listener, token.clone(), port, true).await;
        remove_endpoint_file();
        eprintln!("[MCP] MCP 设置已变更，端点重新绑定…");
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
}

/// 自测用端点：独立端口 + 不写端点文件 + 不接受设置热更新
async fn serve_endpoint_inner(
    base_port: u16,
    ready: Option<tokio::sync::oneshot::Sender<u16>>,
) {
    let token = persistent_token();
    let Some((listener, port)) = bind_endpoint(base_port).await else {
        return;
    };
    if let Some(tx) = ready {
        let _ = tx.send(port);
    }
    eprintln!("[MCP] 自测端点已就绪: 127.0.0.1:{}", port);
    serve_connections(listener, token, port, false).await;
}

fn http_reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        202 => "Accepted",
        204 => "No Content",
        400 => "Bad Request",
        401 => "Unauthorized",
        404 => "Not Found",
        405 => "Method Not Allowed",
        411 => "Length Required",
        _ => "OK",
    }
}

async fn write_http(
    w: &mut (impl tokio::io::AsyncWrite + Unpin),
    status: u16,
    content_type: &str,
    body: &str,
) -> Result<(), String> {
    use tokio::io::AsyncWriteExt;
    let head = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        status,
        http_reason(status),
        content_type,
        body.len()
    );
    w.write_all(head.as_bytes()).await.map_err(|e| e.to_string())?;
    w.write_all(body.as_bytes()).await.map_err(|e| e.to_string())?;
    w.flush().await.map_err(|e| e.to_string())?;
    Ok(())
}

/// 处理一个 HTTP 连接（MCP Streamable HTTP 的最小合规子集）
async fn handle_http_conn(stream: tokio::net::TcpStream, token: &str) -> Result<(), String> {
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

    let (read_half, mut write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half);

    let mut request_line = String::new();
    if reader
        .read_line(&mut request_line)
        .await
        .map_err(|e| e.to_string())?
        == 0
    {
        return Err("client closed".into());
    }
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("").to_uppercase();
    let target = parts.next().unwrap_or("/").to_string();

    // 头
    let mut headers: Vec<(String, String)> = Vec::new();
    loop {
        let mut line = String::new();
        let n = reader.read_line(&mut line).await.map_err(|e| e.to_string())?;
        if n == 0 || line == "\r\n" || line == "\n" {
            break;
        }
        if let Some((k, v)) = line.split_once(':') {
            headers.push((k.trim().to_lowercase(), v.trim().to_string()));
        }
    }
    let header = |name: &str| -> Option<&str> {
        headers
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    };

    let path = target.split('?').next().unwrap_or("/");
    let query = target.split_once('?').map(|(_, q)| q.to_string()).unwrap_or_default();

    // token 校验：Authorization: Bearer xxx 或 ?token=xxx
    let bearer_ok = header("authorization")
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(|t| t.trim() == token)
        .unwrap_or(false);
    let query_ok = query
        .split('&')
        .filter_map(|kv| kv.split_once('='))
        .any(|(k, v)| k == "token" && v == token);

    match method.as_str() {
        "OPTIONS" => {
            write_http(&mut write_half, 204, "text/plain", "").await?;
            Ok(())
        }
        "DELETE" => {
            // 会话终止（我们无状态，直接确认）
            write_http(&mut write_half, 204, "text/plain", "").await?;
            Ok(())
        }        "GET" => {
            // Host 可能打开 SSE 流等待服务端消息：保持连接并周期发心跳
            let wants_sse = header("accept")
                .map(|a| a.contains("text/event-stream"))
                .unwrap_or(false);
            if !wants_sse {
                write_http(&mut write_half, 405, "text/plain", "请使用 POST /mcp（MCP Streamable HTTP）").await?;
                return Ok(());
            }
            if !bearer_ok && !query_ok {
                write_http(&mut write_half, 401, "text/plain", "token 无效").await?;
                return Ok(());
            }
            let head = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nConnection: keep-alive\r\n\r\n";
            if write_half.write_all(head.as_bytes()).await.is_err() {
                return Ok(());
            }
            let _ = write_half.flush().await;
            for _ in 0..240 {
                tokio::time::sleep(std::time::Duration::from_secs(15)).await;
                if write_half.write_all(b": keep-alive\n\n").await.is_err() {
                    break;
                }
                let _ = write_half.flush().await;
            }
            Ok(())
        }
        "POST" => {
            if path != "/mcp" && path != "/" {
                write_http(&mut write_half, 404, "text/plain", "未知路径").await?;
                return Ok(());
            }
            if !bearer_ok && !query_ok {
                write_http(&mut write_half, 401, "application/json", "{\"error\":\"token 无效\"}").await?;
                return Ok(());
            }
            let Some(len) = header("content-length").and_then(|v| v.parse::<usize>().ok()) else {
                write_http(&mut write_half, 411, "text/plain", "需要 Content-Length").await?;
                return Ok(());
            };
            let mut body = vec![0u8; len];
            reader.read_exact(&mut body).await.map_err(|e| e.to_string())?;
            let text = String::from_utf8_lossy(&body).to_string();
            let messages: Vec<Value> = match serde_json::from_str::<Value>(&text) {
                Ok(Value::Array(arr)) => arr,
                Ok(v) => vec![v],
                Err(e) => {
                    let err = rpc_error(&Value::Null, -32700, &format!("JSON 解析失败: {}", e));
                    write_http(&mut write_half, 200, "application/json", &err.to_string()).await?;
                    return Ok(());
                }
            };
            let mut responses: Vec<Value> = Vec::new();
            for msg in messages {
                let line = msg.to_string();
                if let Some(resp) = handle_line(&line).await {
                    responses.push(serde_json::from_str(&resp).unwrap_or(Value::Null));
                }
            }
            if responses.is_empty() {
                write_http(&mut write_half, 202, "text/plain", "").await?;
                return Ok(());
            }
            let payload = if responses.len() == 1 {
                responses[0].to_string()
            } else {
                Value::Array(responses).to_string()
            };
            write_http(&mut write_half, 200, "application/json", &payload).await?;
            Ok(())
        }
        _ => {
            write_http(&mut write_half, 405, "text/plain", "unsupported method").await?;
            Ok(())
        }
    }
}

/* ─── 桥接：Host 的 stdio ↔ 本机 HTTP 端点 ─────────────────── */

fn endpoint_port() -> Option<u16> {
    let info = endpoint_info();
    info.get("port").and_then(|p| p.as_u64()).map(|p| p as u16)
}

/// 极简 HTTP POST（本机端点专用）：返回 (状态码, body)
async fn http_post(port: u16, path_with_query: &str, body: &str) -> Result<(u16, String), String> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .map_err(|e| format!("连接端点失败: {}", e))?;
    let req = format!(
        "POST {} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        path_with_query,
        port,
        body.len(),
        body
    );
    stream
        .write_all(req.as_bytes())
        .await
        .map_err(|e| e.to_string())?;
    let _ = stream.flush().await;
    let mut buf = Vec::new();
    stream.read_to_end(&mut buf).await.map_err(|e| e.to_string())?;
    let text = String::from_utf8_lossy(&buf).to_string();
    let (head, body) = text.split_once("\r\n\r\n").ok_or("响应格式错误")?;
    let status = head
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|s| s.parse::<u16>().ok())
        .unwrap_or(0);
    Ok((status, body.to_string()))
}

/// 探测端点是否活着（JSON-RPC ping）
async fn endpoint_alive(port: u16) -> bool {
    let token = persistent_token();
    let path = format!("/mcp?token={}", token);
    matches!(
        http_post(port, &path, r#"{"jsonrpc":"2.0","id":0,"method":"ping"}"#).await,
        Ok((200, _))
    )
}

/// 确保端点可用：不存在或不可达时拉起桌面端并等待
async fn ensure_endpoint() -> Option<u16> {
    if let Some(port) = endpoint_port() {
        if endpoint_alive(port).await {
            return Some(port);
        }
    } else {
    }
    launch_gui();
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(GUI_WAIT_TIMEOUT_MS);
    loop {
        if let Some(port) = endpoint_port() {
            if endpoint_alive(port).await {
                return Some(port);
            }
        }
        if std::time::Instant::now() >= deadline {
            return None;
        }
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    }
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
    // 关键：必须完全脱离 stdio。否则被拉起的桌面端会继承本进程的
    // stdin/stdout（也就是 MCP Host 的管道），Host 会一直等不到 stdout 关闭而卡住。
    match std::process::Command::new(&gui)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(_) => eprintln!("[MCP] 已拉起桌面端: {}", gui.display()),
        Err(e) => eprintln!("[MCP] 拉起桌面端失败: {}", e),
    }
}

/// 桥接：stdin 的 NDJSON 请求逐条转发到本机端点，响应写回 stdout
pub async fn serve_bridge() -> i32 {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

    let Some(port) = ensure_endpoint().await else {
        eprintln!(
            "[MCP] 等待桌面端端点超时（{}ms）。请先启动 FlashDir 桌面端，或用 --mcp 独立模式。",
            GUI_WAIT_TIMEOUT_MS
        );
        return 2;
    };
    let token = persistent_token();
    let path = format!("/mcp?token={}", token);
    eprintln!("[MCP] 已连接桌面端 127.0.0.1:{}（共享其索引/缓存与管理员权限）", port);

    let stdin = tokio::io::stdin();
    let mut lines = tokio::io::BufReader::new(stdin).lines();
    let mut stdout = tokio::io::stdout();

    while let Some(line) = lines.next_line().await.ok().flatten() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        match http_post(port, &path, trimmed).await {
            Ok((_status, body)) => {
                if body.trim().is_empty() {
                    // notification：无响应
                    continue;
                }
                // 归一化成单行（NDJSON 要求每条消息一行）
                if let Ok(v) = serde_json::from_str::<Value>(&body) {
                    let out = v.to_string();
                    if stdout.write_all(out.as_bytes()).await.is_err() {
                        break;
                    }
                    if stdout.write_all(b"\n").await.is_err() {
                        break;
                    }
                    let _ = stdout.flush().await;
                } else {
                    eprintln!("[MCP] 端点返回非 JSON: {}", body.chars().take(200).collect::<String>());
                }
            }
            Err(e) => {
                eprintln!("[MCP] 转发失败: {}", e);
                return 3;
            }
        }
    }
    eprintln!("[MCP] stdin 关闭，桥接退出");
    0
}

/* ─── 端点/桥接自测 ───────────────────────────────────────── */

/// 自测 HTTP 端点：启动端点 → 读端点文件 → 正常请求 / 错误 token / 方法校验
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

    // 用独立端口且不写端点文件，避免影响正在运行的桌面端
    let (tx, rx) = tokio::sync::oneshot::channel();
    tokio::spawn(async move { serve_endpoint_inner(47899, Some(tx)).await });
    let port = match tokio::time::timeout(std::time::Duration::from_secs(5), rx).await {
        Ok(Ok(p)) => p,
        _ => {
            eprintln!("  [FAIL] 自测端点未就绪");
            return 1;
        }
    };
    check("自测端点已监听（独立端口，不改端点文件）", port > 0, &format!("port={}", port));

    let token = persistent_token();
    let good = format!("/mcp?token={}", token);

    // 1) tools/list
    match http_post(port, &good, r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#).await {
        Ok((status, body)) => {
            let v: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
            check(
                "tools/list 返回工具清单",
                status == 200 && v["result"]["tools"].as_array().map(|a| a.len() >= 6).unwrap_or(false),
                &body.chars().take(120).collect::<String>(),
            );
        }
        Err(e) => check("tools/list 返回工具清单", false, &e),
    }

    // 2) tools/call list_volumes
    match http_post(
        port,
        &good,
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"list_volumes","arguments":{}}}"#,
    )
    .await
    {
        Ok((status, body)) => {
            let v: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
            let vols = v["result"]["content"][0]["text"]
                .as_str()
                .and_then(|t| serde_json::from_str::<Value>(t).ok())
                .map(|d| d["volumes"].is_array())
                .unwrap_or(false);
            check("tools/call list_volumes 正常", status == 200 && vols, &body.chars().take(120).collect::<String>());
        }
        Err(e) => check("tools/call list_volumes 正常", false, &e),
    }

    // 3) notification → 202
    match http_post(port, &good, r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#).await {
        Ok((status, _)) => check("notification 返回 202", status == 202, &format!("status={}", status)),
        Err(e) => check("notification 返回 202", false, &e),
    }

    // 4) 错误 token → 401
    match http_post(port, "/mcp?token=wrong", r#"{"jsonrpc":"2.0","id":3,"method":"ping"}"#).await {
        Ok((status, _)) => check("错误 token 返回 401", status == 401, &format!("status={}", status)),
        Err(e) => check("错误 token 返回 401", false, &e),
    }

    // 5) 批量请求
    match http_post(
        port,
        &good,
        r#"[{"jsonrpc":"2.0","id":4,"method":"ping"},{"jsonrpc":"2.0","id":5,"method":"ping"}]"#,
    )
    .await
    {
        Ok((status, body)) => {
            let v: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
            check(
                "批量请求返回数组响应",
                status == 200 && v.as_array().map(|a| a.len() == 2).unwrap_or(false),
                &body.chars().take(120).collect::<String>(),
            );
        }
        Err(e) => check("批量请求返回数组响应", false, &e),
    }

    if failures == 0 {
        eprintln!("[MCP] 端点自测全部通过 ✅");
        0
    } else {
        eprintln!("[MCP] 端点自测失败 {} 项 ❌", failures);
        1
    }
}

/// 自测桥接链路（需桌面端已运行）：直接对端点调用 diagnostics
pub async fn selftest_bridge() -> i32 {
    let Some(port) = ensure_endpoint().await else {
        eprintln!("[MCP] 未发现可用端点（桌面端未运行且无法拉起）");
        return 2;
    };
    let token = persistent_token();
    let path = format!("/mcp?token={}", token);
    let req = r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"diagnostics","arguments":{}}}"#;
    match http_post(port, &path, req).await {
        Ok((status, body)) => {
            let v: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
            let text = v["result"]["content"][0]["text"].as_str().unwrap_or("");
            let parsed: Value = serde_json::from_str(text).unwrap_or(Value::Null);
            if status == 200 && parsed["version"].is_string() {
                eprintln!(
                    "[MCP] 桥接链路正常（127.0.0.1:{}）→ {}",
                    port,
                    parsed["summary"].as_str().unwrap_or("")
                );
                0
            } else {
                eprintln!("[MCP] 桥接返回异常: {}", body.chars().take(200).collect::<String>());
                1
            }
        }
        Err(e) => {
            eprintln!("[MCP] 桥接失败: {}", e);
            3
        }
    }
}

/// HTTP 地址（含 token），供桌面端设置页展示
pub fn http_url_for_display() -> String {
    endpoint_url()
}

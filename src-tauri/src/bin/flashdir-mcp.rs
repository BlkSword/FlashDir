//! FlashDir MCP 服务器（原生 Rust 实现）
//!
//! - 传输：stdio，JSON-RPC 2.0，**换行分隔**（NDJSON）
//! - stdout 只输出协议消息，所有日志走 stderr
//! - 复用主程序引擎：scan / global_search / disk_cache / volumes
//!
//! 用法：把本二进制配置到 MCP Host（Claude Desktop / Cursor 等）：
//! ```json
//! { "mcpServers": { "flashdir": { "command": "C:\path\flashdir-mcp.exe" } } }
//! ```
//! 自测：`flashdir-mcp.exe --selftest`（校验协议与工具返回结构，退出码非 0 表示失败）

// 与 GUI 一致：大规模小对象分配用 mimalloc
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

use serde_json::{json, Value};
use std::io::{BufRead, Write};
use std::sync::Arc;

/// 支持的协议版本（按新→旧）；回包选择双方都支持的最新版本
const SUPPORTED_PROTOCOL_VERSIONS: [&str; 3] = ["2025-06-18", "2025-03-26", "2024-11-05"];
const SERVER_NAME: &str = "flashdir";

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
async fn handle_line(line: &str) -> Option<String> {
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
                flashdir::cancel::request();
            }
            _ => {}
        }
        return None;
    }
    let id = id.unwrap();

    let resp = match method {
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
    };

    Some(resp.to_string())
}

/* ─── 工具定义 ─────────────────────────────────────────────── */

fn tool_definitions() -> Vec<Value> {
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

fn item_json(item: &flashdir::scan::Item) -> Value {
    json!({
        "name": item.name,
        "path": item.path,
        "size": item.size,
        "isDir": item.is_dir,
        "mtime": item.mtime,
        "atime": item.atime,
    })
}

async fn call_tool(name: &str, args: &Value) -> Result<Value, String> {
    match name {
        "list_volumes" => {
            let vols = flashdir::volumes::list_volumes();
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
            let idx = flashdir::global_search::instance();
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

            let perf = flashdir::perf::PerformanceMonitor::instance();
            let started = std::time::Instant::now();
            let view = flashdir::scan::scan_directory_view(&path, force, perf, None)
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
            let filters = flashdir::global_search::parse_search_filter(&filter);
            let mut items: Vec<&flashdir::scan::Item> = view
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
                    flashdir::global_search::item_matches_filters(
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
                        flashdir::scan::format_size(view.total_size),
                        elapsed_ms,
                        source
                    ),
                    "path": view.path,
                    "totalItems": total_matched,
                    "totalSize": view.total_size,
                    "totalSizeFormatted": flashdir::scan::format_size(view.total_size),
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
            let stats = flashdir::disk_cache::DiskCache::instance().get_stats();
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
            let idx = flashdir::global_search::instance();
            let (index_kind, index_meta) = match idx.state() {
                flashdir::global_search::IndexState::Ready(meta) => (
                    "ready",
                    Some(json!({
                        "fileCount": meta.file_count,
                        "dirCount": meta.dir_count,
                        "driveCount": meta.drive_count,
                        "partial": meta.partial,
                        "failedDrives": meta.failed_drives.iter().map(|c| c.to_string()).collect::<Vec<_>>(),
                    })),
                ),
                flashdir::global_search::IndexState::Loading { drive, scanned } => (
                    "loading",
                    Some(json!({ "drive": drive.to_string(), "scanned": scanned })),
                ),
                flashdir::global_search::IndexState::Failed { reason } => {
                    ("failed", Some(json!({ "reason": reason })))
                }
                flashdir::global_search::IndexState::NotLoaded => ("notLoaded", None),
            };
            let cache = flashdir::disk_cache::DiskCache::instance().get_stats();
            // fs::mft_scanner 是私有模块；管理员判断用公开入口
            let is_admin = flashdir::fs::is_admin();
            let volume_list: Vec<Value> = flashdir::volumes::list_volumes()
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
async fn wait_index_ready(idx: &flashdir::global_search::GlobalIndex, timeout_ms: u64) -> bool {
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
    loop {
        if matches!(idx.state(), flashdir::global_search::IndexState::Ready(..)) {
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

async fn serve() {
    eprintln!(
        "[MCP] FlashDir {} 启动（stdio / JSON-RPC 2.0 / NDJSON）",
        env!("CARGO_PKG_VERSION")
    );

    // 后台加载持久化全局索引（与 GUI 相同），让 search_files 尽快可用
    {
        let idx = flashdir::global_search::instance();
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
async fn selftest() -> i32 {
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

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--selftest") {
        std::process::exit(selftest().await);
    }
    if args.iter().any(|a| a == "--help" || a == "-h") {
        eprintln!(
            "FlashDir MCP 服务器\n\n用法:\n  flashdir-mcp            以 stdio 方式运行 MCP 服务器\n  flashdir-mcp --selftest 运行协议与工具自测\n\n配置到 MCP Host（Claude Desktop / Cursor）:\n  {{ \"mcpServers\": {{ \"flashdir\": {{ \"command\": \"<path>\\flashdir-mcp.exe\" }} }} }}"
        );
        return;
    }
    serve().await;
}

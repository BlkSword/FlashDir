# FlashDir 原生 MCP 支持设计

> 目标：让 FlashDir 作为 **MCP（Model Context Protocol）服务器**，把本机磁盘可观测能力
> （扫描、全局索引搜索、重复文件、开发缓存、快照对比）暴露给 AI 客户端
> （Claude Desktop / Cursor / 其他 MCP Host），且**不引入 Node/Python 运行时**。

## 0. 实现状态（已完成并验证）

**单一产物**：MCP 是 `flashdir.exe` 的命令行模式，不再有第二个二进制。

```
Claude Desktop / Cursor
   │ 方式一：HTTP   POST http://127.0.0.1:47821/mcp?token=<持久 token>
   │ 方式二：stdio  flashdir.exe --bridge（自动拉起桌面端）
   ▼
flashdir.exe（桌面端内后台线程的 HTTP 端点）  ← 热索引 / 扫描缓存 / 管理员权限
```

| 模式 | 命令 | 说明 |
|------|------|------|
| 桌面端（默认） | `flashdir.exe` | 启动 GUI，并在后台开启 HTTP 端点 |
| 桥接（Host 用） | `flashdir.exe --bridge` | stdio ↔ 本机 HTTP；桌面端未运行会自动拉起 |
| 独立 stdio | `flashdir.exe --mcp` | 不依赖桌面端；权限继承 Host（通常非管理员） |
| 自测 | `--selftest` / `--selftest-endpoint` / `--selftest-bridge` | 11 / 6 / 链路 |
| 帮助 | `--mcp-help` | 打印当前 HTTP 地址与用法 |

**端点约定**

- 端口固定 `47821`（占用则顺延 47822…47825），仅监听 `127.0.0.1`
- token 持久化于 `~/.flashdir/mcp-token`（仅当前用户可读）→ 配置里的 URL 长期有效
- 支持 `Authorization: Bearer <token>` 或 `?token=`；无 token → 401
- 传输形态：MCP **Streamable HTTP** 最小合规子集 —— `POST /mcp`（单条/批量 JSON-RPC，
  返回 `application/json`）、`GET /mcp`（`Accept: text/event-stream` 时保持 SSE 心跳）、
  `DELETE /mcp`（204）、notification → 202
- 端点文件 `~/.flashdir/mcp-endpoint.json` 记录 `{port,pid,url}`，桥接据此发现端点
- 为什么不用命名管道：桌面端常以管理员运行，管道的强制完整性标签会阻止非管理员桥进程读写
- 为什么桥接要用 HTTP 而不是自定义握手：Host 侧只用一次协议实现（stdio），
  本机侧复用同一套 HTTP 语义，减少两套协议分叉

**实测**

- `--selftest` 11 项、`--selftest-endpoint` 6 项（含 401、202、批量）、`--selftest-bridge` 全通过
- `curl POST http://127.0.0.1:47821/mcp?token=…` → 200 且返回真实搜索结果；无 token → 401
- 自动拉起：桌面端未运行时桥接自动启动它并完成调用，退出码 0
- 修复的真问题：被拉起的桌面端会继承桥的 stdio 管道导致 Host 卡住 → 现在完全脱离 stdio


## 1. 为什么"原生 Rust"实现
## 1. 为什么"原生 Rust"实现

| 方案 | 问题 |
|------|------|
| 用官方 TypeScript/Python SDK 另写一个进程 | 需要用户装 Node/Python；与主程序状态（缓存/索引）割裂，搜索结果不一致 |
| **在现有 Rust 工程内实现协议 + 复用引擎**（本方案） | 零额外运行时；直接复用 `scan`/`global_search`/`disk_cache`/`duplicate_finder`/`dev_analyzer`/`diff_engine`；与 GUI 共享同一份 SQLite 缓存与全局索引 |

关键优势：**进程常驻**。MCP Host 会长期持有服务器进程，因此
- 第一次调用做全量扫描后，内存缓存（LRU）保持热态 → 后续调用毫秒级
- 全局索引只在启动时加载一次，`search_files` 直接命中内存索引

## 2. 协议与传输

- **JSON-RPC 2.0**，**stdio** 传输（Host 以子进程方式启动我们）
- **换行分隔**（NDJSON）：每条消息一行，消息内不得有裸换行（与 LSP 的 `Content-Length` 头不同）
- 关键约束：**stdout 只允许出现协议消息**；所有日志/诊断必须走 stderr
  （引擎内部已经全部用 `eprintln!`，符合要求）
- 协议版本协商：客户端在 `initialize` 里给 `protocolVersion`，我们支持
  `2025-06-18` / `2025-03-26` / `2024-11-05`，回包选择"双方都支持的最新版本"

### 消息流

```
Host → initialize            {protocolVersion, capabilities, clientInfo}
Srv  → result                {protocolVersion, capabilities:{tools:{}}, serverInfo:{name:"flashdir",version}}
Host → notifications/initialized
Host → tools/list            → {tools:[{name, description, inputSchema}]}
Host → tools/call            {name:"search_files", arguments:{query:"*.pdf size:>10MB", limit:50}}
Srv  → result                {content:[{type:"text", text:"<JSON>"}], isError:false}
Srv  → notifications/message （可选：索引进度、扫描耗时等日志）
```

错误码：`-32700` 解析失败、`-32600` 非法请求、`-32601` 方法不存在、
`-32602` 参数非法、`-32603` 内部错误；工具级错误用 `isError:true` + 文本说明
（不抛 JSON-RPC 错误，便于模型自我纠正）。

## 3. 进程与并发模型

- 新增 **console 二进制 `flashdir-mcp.exe`**（GUI 的 `flashdir.exe` 是
  `windows_subsystem="windows"`，没有 stdio，不能复用）
- 单线程读 stdin 分发；**扫描类请求串行化**（引擎的取消代号是全局的，
  并发扫描会互相干扰），其余只读查询可并发
- 支持 `notifications/cancelled` → 映射到 `cancel::request()`
- 启动时后台线程加载持久化全局索引（与 GUI 相同路径）；
  若索引不存在，P1 提供"按需构建 + 进度通知"

## 4. 工具面（Tool Surface）

统一约定：
- **只读**：不提供删除/移动/清理动作（与产品"全程只读"一致）
- 每个列表型工具都有 `limit`（默认 50，上限 1000）与 `truncated` / `total` 字段
- 返回 `content[0].text` 为紧凑 JSON（含 `summary` 便于模型直接引用）
- 路径参数校验：必须存在、必须是本地路径；非管理员时自动回退目录遍历并如实标注

### P0（已实现）

| 工具 | 入参 | 说明 |
|------|------|------|
| `list_volumes` | — | 各盘容量/可用/文件系统/是否 NTFS（GetDiskFreeSpaceEx） |
| `scan_directory` | `path`, `force?`, `sort?`, `limit?`, `filter?` | 走完整流水线（USN→内存→磁盘→推导→MFT/遍历），返回总量/文件数/目录数/耗时/来源 + Top N |
| `list_directory` | `path`, `sort?`, `direction?`, `limit?`, `offset?`, `filter?` | 分页列目录（过滤语法与 GUI 相同） |
| `search_files` | `query`, `limit?`, `offset?` | **全局索引搜索**（Everything 式语法），返回 `total` 与命中列表 |
| `cache_stats` | — | 磁盘缓存条目数/占用/上限、内存缓存统计 |
| `diagnostics` | — | 运行诊断（索引状态、缓存、驱动、版本） |

### P1（待确认后实现）

| 工具 | 入参 |
|------|------|
| `find_duplicates` | `path`, `min_size_bytes?`, `limit?` |
| `analyze_dev_cache` | `path`, `limit?` |
| `list_snapshots` / `save_snapshot` / `compare_snapshots` | `path` / `path` / `old_id,new_id` |
| `find_large_files` | `path`, `min_size_bytes`, `limit?`（= scan + `size:>` 过滤） |
| `disk_usage_trend` | `path`（基于快照总量序列） |
| `notifications/message` 进度通知、`logging/setLevel` | — |

### P2

- HTTP/SSE 传输（供无法启动子进程的 Host）
- `resources/list` 暴露常用扫描结果为资源（如 `flashdir://scan/C:/Users`）
- `prompts/list` 提供"分析这个目录为什么变大"这类提示模板

## 5. 配置示例（Host 侧）

```json
{
  "mcpServers": {
    "flashdir": {
      "command": "C:\path\to\flashdir-mcp.exe",
      "args": []
    }
  }
}
```

## 6. 测试方案

- **`--selftest`**：进程内跑一段脚本化会话
  （initialize → tools/list → tools/call 若干 → 校验 JSON 结构），
  返回非零退出码表示失败；CI 可直接跑，无需 Host
- 手工：Claude Desktop 配置后问"我的 C 盘哪里占空间最多"
- 协议层单测放在 bin 内（避免把引擎代码链进 lib 测试二进制——
  实测 lib 测试链入完整 tauri 运行时会因 DLL 缺失无法加载）

## 7. 风险与取舍

| 风险 | 处理 |
|------|------|
| stdout 被日志污染 → Host 解析失败 | 只允许协议消息写 stdout；`--selftest` 校验 stdout 纯 JSON |
| 扫描很慢（首次 MFT 全盘 3-5s，遍历模式分钟级） | 结果带 `source`/`elapsed_ms`；建议模型先 `search_files`（毫秒级）再按需 `scan_directory` |
| 大目录返回过大 | 强制 `limit` 截断 + `truncated`；文本 JSON 控制在数十 KB |
| 协议版本演进 | 版本协商 + 只实现稳定方法（initialize/tools/*）；未知方法返回 -32601 |
| 与 GUI 同时运行 | 共享同一 SQLite（WAL）与外部 blob 文件，已有并发保护（busy_timeout + 单写线程） |

## 8. 单实例模式（方案 C）实现说明

**最终架构**

```
Claude Desktop / Cursor
        │ stdio (JSON-RPC, NDJSON)
        ▼
flashdir-mcp.exe --bridge           ← 薄桥：只搬运字节
        │ 127.0.0.1:随机端口 + 一次性 token
        ▼
flashdir.exe（桌面端内后台线程）      ← 热索引 / 扫描缓存 / 管理员权限
```

**为什么不用命名管道**：桌面端通常以管理员运行，命名管道对象的强制完整性标签会
阻止非管理员进程（Host 启动的桥）读写；本机回环 + 仅当前用户可读的 token 文件
既跨完整性级别可用，也不对外开放（绑定 127.0.0.1 不触发防火墙提示）。

**三种形态（同一份 `mcp.rs`）**

| 形态 | 启动 | 特点 |
|------|------|------|
| 桥接（推荐） | `flashdir-mcp.exe --bridge` | 共享桌面端索引/缓存，继承管理员权限；桌面端未运行会自动拉起并等待 |
| 独立 stdio | `flashdir-mcp.exe` | 不依赖桌面端；权限继承 Host（通常非管理员）→ 扫描走目录遍历 |
| 本机端点 | 桌面端启动时自动开启 | 端点文件 `~/.flashdir/mcp-endpoint.json`（port/token/pid），仅当前用户可读 |

**关键实现点**

- 端点每连接独立任务（并发多客户端）；`stdin` 关闭时先排空响应再退出
- token 校验失败立即断开；握手超时 5s；等待桌面端上限 25s；命令索引就绪等待 3s
- 桌面端状态栏展示 `MCP 已连接 · 最近调用`（客户端名 + 累计次数），点击可复制配置
- 自测：`--selftest`（11 项）/`--selftest-endpoint`（6 项）/`--selftest-bridge`（需桌面端）

**实测**：`--selftest-bridge` → `管理员=true · 索引=ready（433,545 项）· 缓存 7 个目录`；
管道一次性调用（initialize + search_files）响应全部合法 JSON。

## 9. P1 工具与设置（已完成）

**13 个工具**（新增 7 个）：`find_large_files`、`find_duplicates`、`analyze_dev_cache`、
`list_snapshots`、`save_snapshot`、`compare_snapshots`、`disk_usage_trend`。

- 统一标注：`readOnlyHint`（仅 `save_snapshot` 为 false）、`destructiveHint: false`、`openWorldHint: false`
- 取数：优先内存缓存（`memory-cache`）→ 未命中才扫描；响应 `source` 字段标明来源
- 响应体积控制：列表类工具都有 `limit` 与 `truncated`，重复组每组最多 5 个路径

**设置（桌面端可热改）**

- `~/.flashdir/mcp-settings.json`：`{ "enabled": bool, "port": u16 }`
- 端点线程每秒检查设置：关闭 → 停止监听并删除端点文件；改端口 → 解绑后按新端口重绑
  （旧端口不再监听，1 秒内完成，无需重启桌面端）
- 桌面端设置弹窗（标题栏齿轮 / 命令面板"设置"）提供开关、端口、当前地址与复制地址

**实测**：关闭 → 端口拒绝连接 + 端点文件删除；改 47831 → 新端口 200 且旧端口关闭；改回 → 200。

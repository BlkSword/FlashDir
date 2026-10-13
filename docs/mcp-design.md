# FlashDir 原生 MCP 支持设计

> 目标：让 FlashDir 作为 **MCP（Model Context Protocol）服务器**，把本机磁盘可观测能力
> （扫描、全局索引搜索、重复文件、开发缓存、快照对比）暴露给 AI 客户端
> （Claude Desktop / Cursor / 其他 MCP Host），且**不引入 Node/Python 运行时**。

## 0. 实现状态

**单一产物**：MCP 能力全部由 `flashdir.exe` 提供，**没有第二个可执行文件**。
（设计早期曾计划新增 console 二进制 `flashdir-mcp.exe`，最终方案改为直接复用 GUI 可执行文件：
同一份 `src/mcp.rs` 承载三种形态，避免两份产物与两套协议实现。）

```
Claude Desktop / Cursor
   │ 形态 A：HTTP   POST http://127.0.0.1:47821/mcp?token=<持久 token>
   │ 形态 B：stdio  flashdir.exe --bridge（自动拉起桌面端）
   │ 形态 C：stdio  flashdir.exe --mcp（独立进程，不依赖桌面端）
   ▼
flashdir.exe
   ├─ 桌面端（默认）：GUI + 后台线程里的 HTTP 端点  ← 热索引 / 扫描缓存 / 管理员权限
   ├─ --bridge：stdio ↔ 本机端点，只搬运字节
   └─ --mcp：进程内直接跑协议与引擎，内存态私有
```

| 模式 | 命令 | 说明 |
|------|------|------|
| 桌面端（默认） | `flashdir.exe` | 启动 GUI，并在后台开启 HTTP 端点 |
| 桥接（Host 用） | `flashdir.exe --bridge` | stdio ↔ 本机 HTTP；桌面端未运行会自动拉起同目录的 `flashdir.exe`（完全脱离 stdio） |
| 独立 stdio | `flashdir.exe --mcp` | 不依赖桌面端；权限继承 Host（通常非管理员）；内存索引与扫描缓存为该进程私有 |
| 自测 | `--selftest` / `--selftest-endpoint` / `--selftest-bridge` | 以**退出码 0** 表示通过；断言数量以命令输出为准 |
| 帮助 | `--mcp-help` | 打印当前 HTTP 地址与用法 |

**端点约定**

- 端口固定 `47821`（被占用时在 47821–47825 之间顺延，最多 5 次绑定尝试），仅监听 `127.0.0.1`
- token 持久化于 `~/.flashdir/mcp-token`（32 个十六进制字符，底层为 CSPRNG）
  → 配置里的 URL 长期有效，跨桌面端重启无需改配置
  - 代码**没有对令牌文件单独设置 ACL**；它的可见范围仅是 `%USERPROFILE%\.flashdir\` 的继承权限，
    不是应用主动派生的访问控制
- 支持 `Authorization: Bearer <token>` 或 `?token=<token>`；两条路径都走常量时间比较
- **令牌校验对所有方法一致**：`POST` / `GET`(SSE) / `OPTIONS` / `DELETE` 在缺令牌或令牌错误时都返回 401。
  早期实现的 `OPTIONS` / `DELETE` 无条件回 204，等价于向未授权客户端确认"端点存在"，已修正。
  唯一的例外是 `POST` 在路径不匹配时先回 404（只暴露"这里不是 /mcp"，不涉及数据）
- 请求限额：`Content-Length` 缺失 → 411；请求体超过 4 MiB → 413；单个请求头超过 8 KiB、
  或头数量超过 100 → 431
- 传输形态：MCP **Streamable HTTP** 最小合规子集
  - `POST /mcp`：单条或批量（JSON 数组）JSON-RPC → `200 application/json`；
    整批都是 notification（无 id）→ `202`
  - `GET /mcp`：`Accept: text/event-stream` 时保持 SSE 心跳（每 15 秒一条，最多 240 次 ≈ 1 小时）
  - `DELETE /mcp`：`204`（本端点无状态，仅作确认）
- 端点文件 `~/.flashdir/mcp-endpoint.json` 记录 `{port,pid,url}`，**不含 token**；桥接据此发现端点
- 为什么不用命名管道：桌面端常以管理员运行，管道对象的强制完整性标签会阻止非管理员桥进程读写；
  本机回环 + 继承权限保护的 token 文件既跨完整性级别可用，也不对外开放（绑定 127.0.0.1 不触发防火墙提示）
- 为什么桥接用 HTTP 而不是自定义握手：Host 侧只需一套协议实现（stdio），本机侧复用同一套 HTTP 语义，
  避免两套协议分叉

**验收方式（不写死断言数）**

- `flashdir.exe --selftest`、`flashdir.exe --selftest-endpoint`：**退出码 0** 表示通过，断言数量以输出为准
- `curl POST http://127.0.0.1:47821/mcp?token=…` 应返回 200 与真实搜索结果；无 token / 错误 token 应返回 401
- 自动拉起：桌面端未运行时 `--bridge` 应自动启动它并完成调用，退出码 0
- 历史实测记录（3.5.1 修复轮之前）：`--selftest-bridge` 曾输出"管理员=true · 索引=ready · 缓存 N 个目录"；
  管道一次性调用（initialize + search_files）响应全部为合法 JSON
- 已修复的真问题：被拉起的桌面端会继承桥的 stdio 管道导致 Host 卡住 → 现在完全脱离 stdio

## 1. 为什么"原生 Rust"实现

| 方案 | 问题 |
|------|------|
| 用官方 TypeScript/Python SDK 另写一个进程 | 需要用户装 Node/Python；与主程序状态（缓存/索引）割裂，搜索结果不一致 |
| **在现有 Rust 工程内实现协议 + 复用引擎**（本方案） | 零额外运行时；直接复用 `scan`/`global_search`/`disk_cache`/`duplicate_finder`/`dev_analyzer`/`diff_engine` |

关于"共享状态"要分形态看（易被误解，这里明确）：

- **桥接 / GUI 端点**：客户端请求落进桌面端进程，**共享桌面端的内存索引、扫描缓存与管理员权限**；
- **`--mcp` 独立 stdio**：协议与引擎都在自己的进程里，**内存索引与扫描缓存是该进程私有的**，
  只与桌面端通过同一份 SQLite 缓存库（WAL）与 `blobs/` 文件**间接共享磁盘层数据**。

关键优势：**进程常驻**（Host 会长期持有服务器进程）。因此在同一形态内：

- 第一次调用做全量扫描后，内存缓存（LRU）保持热态 → 后续调用毫秒级；
- 独立进程模式下索引只在启动时载入一次，`search_files` 直接命中该进程内存索引。

## 2. 协议与传输

- **JSON-RPC 2.0**；stdio 形态使用 **NDJSON**（每条消息一行，消息内不得有裸换行）
- 关键约束：**stdout 只允许出现协议消息**；所有日志/诊断必须走 stderr（引擎内部统一用 `eprintln!`）
- 协议版本协商：客户端在 `initialize` 里给 `protocolVersion`。客户端版本若在支持列表
  （`2025-06-18` / `2025-03-26` / `2024-11-05`）内，**原样回显该版本**；不在列表内（含未提供）
  则回退到列表中最新的 `2025-06-18`。即：不是"双方都支持的最新版本"，而是"客户端版本在列表内则回显，
  否则给最新支持版本"

### 消息流

```
Host → initialize            {protocolVersion, capabilities, clientInfo}
Srv  → result                {protocolVersion, capabilities:{tools:{listChanged:false}}, serverInfo:{name:"flashdir",version}}
Host → notifications/initialized
Host → tools/list            → {tools:[{name, description, inputSchema, annotations}]}
Host → tools/call            {name:"search_files", arguments:{query:"*.pdf size:>10MB", limit:50}}
Srv  → result                {content:[{type:"text", text:"<JSON>"}], isError:false}
```

服务端**不主动发通知**：`notifications/message` / `notifications/progress` / `logging/setLevel`
都未实现。接收方向只处理两条 notification —— `notifications/initialized`（忽略）与
`notifications/cancelled`（转全局取消请求）。

错误码实际只用到两个：

| 码 | 场景 |
|----|------|
| `-32700` | JSON 解析失败（stdio 行解析失败、HTTP 请求体解析失败） |
| `-32601` | 方法不存在（`initialize` / `ping` / `tools/list` / `tools/call` / `resources/list` / `prompts/list` 之外） |

`-32600`（非法请求）、`-32602`（参数非法）、`-32603`（内部错误）**当前实现从不产生**：
参数缺失或非法一律走工具级错误 —— `result.isError: true` + 文本说明
（便于模型自我纠正，而不是抛 JSON-RPC 错误让 Host 中断会话）。
`resources/list` 与 `prompts/list` 目前返回空数组。

## 3. 进程与并发模型

- **不新增二进制**：三种形态都在 `flashdir.exe` 内（GUI 的 `windows_subsystem="windows"` 只影响
  默认形态；`--mcp` / `--bridge` 在创建窗口之前的启动路径里直接分流，因此不需要 console 版本）
- **stdio 形态**：单线程逐行读 stdin、串行处理；每条响应立即写出并 flush（Host 逐行读取）
- **HTTP 端点**：每个连接一个独立 tokio 任务，因此**可以并发扫描**；
  实现里**没有**"扫描类请求串行化"这道闸门
- **取消是全局的**：`notifications/cancelled` 映射到 `cancel::request()`，后者记录"取消到当前代次为止"，
  于是**会取消所有在飞的扫描**——同一客户端并发发起多个扫描时，一次取消会互相干扰。
  （取消代号本身是全局的，这一点在 `cancel.rs` 的注释里就是这么定义的。）
- 索引加载：
  - `--mcp`：启动时后台线程调用 `load_persisted()`（全表载入）；
  - GUI 端点/桥接：索引由桌面端加载，请求直接命中；
  - `search_files` 在索引尚未就绪时会等最多 **3 秒**，仍不就绪则返回 `{ready:false, hint:…}`
- 桥接等待桌面端就绪的上限是 **25 秒**（`GUI_WAIT_TIMEOUT_MS`）；超时返回退出码 2
- **没有握手超时**：早期设计里的"握手超时 5s"（`HANDSHAKE_TIMEOUT_MS`）**从未实现**，
  该常量是死代码并已被删除；现在的超时只有上面两个（索引就绪 3s、等待桌面端 25s）
- 被拉起的桌面端以 `stdin/stdout/stderr = null` 启动，**完全脱离 Host 的管道**

## 4. 工具面（Tool Surface）

统一约定：

- **只读**：不提供删除/移动/清理动作（与产品"全程只读"一致）
- 列表型工具的 `limit` 默认值与上下限**各工具不同**（见下表）；`truncated` 只有部分工具返回
- 返回 `content[0].text` 为紧凑 JSON（含 `summary` 便于模型直接引用）
- 参数名一律是 schema 里的 **camelCase**（`minSizeBytes`、`oldId`、`newId`），不是 `min_size_bytes` / `old_id`
- 路径参数须为存在的本地目录（不存在/不是目录时由扫描层返回错误文本，工具结果为 `isError:true`）；
  非管理员时自动回退目录遍历并如实标注
- 参数越界一律 **clamp**（不报错）；缺必填参数则 `isError:true`

### 13 个工具

| 工具 | 入参 | 说明 |
|------|------|------|
| `list_volumes` | — | 各盘容量/可用/文件系统/是否 NTFS（GetDiskFreeSpaceEx） |
| `scan_directory` | `path`, `force?`, `sort?`, `direction?`, `limit?`, `filter?` | 走完整流水线（USN→内存→内存推导→磁盘推导→磁盘→MFT/遍历），返回总量/文件数/目录数/耗时/来源 + Top N |
| `list_directory` | `path`, `force?`, `sort?`, `direction?`, `limit?`, `offset?`, `filter?` | 分页列目录（过滤语法与 GUI 相同） |
| `search_files` | `query`, `limit?`, `offset?` | **全局索引搜索**（Everything 式语法），返回 `total` 与命中列表 |
| `cache_stats` | — | **仅磁盘缓存**统计：条目数、占用、上限、最早条目时间。**不含内存缓存统计** |
| `diagnostics` | — | 运行诊断（版本、是否管理员、索引状态与条目数、磁盘缓存统计、卷列表） |
| `find_large_files` | `path`, `minSizeBytes?`, `limit?` | 大于指定体积（默认 100MB）的文件，按体积降序 |
| `find_duplicates` | `path`, `minSizeBytes?`, `limit?` | 按内容哈希检测重复文件，返回重复组与可回收空间（每组最多 5 个路径） |
| `analyze_dev_cache` | `path`, `limit?` | 开发类缓存占用（类别、占比、Top 项） |
| `list_snapshots` | `path` | 该目录的历史快照列表 |
| `save_snapshot` | `path` | 保存当前目录快照（写入本应用快照库，不修改用户文件） |
| `compare_snapshots` | `path`, `oldId?`, `newId?`, `limit?` | 对比两份快照或"快照与当前状态"（`newId:"current"`），返回净变化与增删改条目 |
| `disk_usage_trend` | `path`, `limit?` | 基于快照序列的体积趋势（时间点、相邻变化、净变化百分比） |

### limit 的默认值与上下限

| 工具 | 默认 | 下限 | 上限 |
|------|-----:|-----:|-----:|
| `search_files` | 50 | 1 | 1000（另有 `offset`，默认 0，上限 1000000） |
| `scan_directory` | 50 | 1 | 1000 |
| `list_directory` | 50 | 1 | 1000 |
| `find_large_files` | 50 | 1 | 1000 |
| `find_duplicates` | 20 | 1 | 200 |
| `analyze_dev_cache` | 10 | 1 | 50 |
| `compare_snapshots` | 20 | 1 | 200（每类条目） |
| `disk_usage_trend` | 30 | 2 | 200（时间点个数） |

`list_volumes` / `cache_stats` / `diagnostics` / `list_snapshots` / `save_snapshot` 不接受 `limit`。
（"所有列表工具 limit 默认 50、上限 1000"的旧说法不成立。）

### 工具标注（annotations）

`tools/list` 会为每个工具补 MCP 标准标注：

- `readOnlyHint`：仅 `save_snapshot` 为 `false`，其余为 `true`
- `destructiveHint`: `false`（全部）
- `idempotentHint`：`save_snapshot` 为 `false`（每次调用都会生成新快照 id），其余为 `true`
- `openWorldHint`: `false`（全部）

## 5. 配置示例（Host 侧）

HTTP 方式（推荐，端口固定、配置长期有效）：

```json
{
  "mcpServers": {
    "flashdir": {
      "url": "http://127.0.0.1:47821/mcp?token=<本机令牌>"
    }
  }
}
```

stdio 方式（桥接，共享桌面端索引与权限）：

```json
{
  "mcpServers": {
    "flashdir": {
      "command": "C:\\path\\to\\flashdir.exe",
      "args": ["--bridge"]
    }
  }
}
```

需要独立进程、不希望拉起桌面端时，把 `args` 换成 `["--mcp"]`（此时内存态私有、通常非管理员）。
桌面端"MCP 配置"弹窗可一键复制上述两种配置（自动填入实际路径、端口与令牌）。

## 6. 测试方案

- **`--selftest`**：进程内跑一段脚本化会话
  （initialize → tools/list → tools/call 若干 → 校验 JSON 结构），
  以**退出码 0/非 0** 表示通过/失败；CI 可直接跑，无需 Host
- **`--selftest-endpoint`**：起一个独立端口（不写端点文件、不接受设置热更新），
  校验工具清单、`list_volumes`、notification 的 202、错误令牌的 401、批量请求的数组响应
- **`--selftest-bridge`**：验证 stdio ↔ 本机端点的桥接链路（需桌面端运行）
- 断言数量会随测试集扩充而变化，因此 CI 与文档都以"退出码 0"为准，不写死项数
- 手工：Claude Desktop 配置后问"我的 C 盘哪里占空间最多"
- 协议层单测放在 lib 内（`mcp.rs` 末尾的 `#[cfg(test)] mod tests`），随 `cargo test --lib` 一起跑；
  早期曾把协议单测放进 bin 以规避"lib 测试链入完整 tauri 运行时后 DLL 缺失"的问题，现在不再需要该规避

## 7. 风险与取舍

| 风险 | 处理 |
|------|------|
| stdout 被日志污染 → Host 解析失败 | 只允许协议消息写 stdout；`--selftest` 校验响应是合法 JSON |
| 扫描很慢（首次 MFT 全盘 3-5s，遍历模式分钟级） | 结果带 `source`/`elapsed_ms`；建议模型先 `search_files`（毫秒级）再按需 `scan_directory` |
| 大目录返回过大 | `limit` 截断 + `truncated`；文本 JSON 控制在数十 KB |
| 请求体/请求头被用来打爆内存 | 4 MiB 请求体上限（413）+ 8 KiB 单头 / 100 头（431），先校验后分配 |
| 令牌被计时爆破 | Bearer 与 query 两条路径都用常量时间比较 |
| 协议版本演进 | 版本协商（列表内回显，否则最新支持版本）+ 只实现稳定方法；未知方法返回 -32601 |
| 与 GUI 同时运行 | 共享同一 SQLite（WAL）与外部 blob 文件，已有并发保护（busy_timeout + 单写线程） |
| 并发扫描互相干扰 | **当前未做串行化**：HTTP 端每连接一个任务；一次 `notifications/cancelled` 会取消所有在飞扫描。客户端应避免并发发起多个长扫描 |

## 8. 架构说明（三种形态、同一份实现）

**最终架构**

```
Claude Desktop / Cursor
        │ stdio (JSON-RPC, NDJSON)
        ▼
flashdir.exe --bridge                ← 薄桥：只搬运字节
        │ 127.0.0.1:<端口> + 持久 token
        ▼
flashdir.exe（桌面端内后台线程）      ← 热索引 / 扫描缓存 / 管理员权限
```

**为什么不用命名管道**：桌面端通常以管理员运行，命名管道对象的强制完整性标签会
阻止非管理员进程（Host 启动的桥）读写；本机回环 + 仅当前用户可读（由目录继承权限保障）的
token 文件既跨完整性级别可用，也不对外开放（绑定 127.0.0.1 不触发防火墙提示）。

**三种形态（同一份 `mcp.rs`、同一个可执行文件）**

| 形态 | 启动 | 特点 |
|------|------|------|
| 桥接（推荐） | `flashdir.exe --bridge` | 共享桌面端索引/缓存，继承管理员权限；桌面端未运行会自动拉起并等待（上限 25s） |
| 独立 stdio | `flashdir.exe --mcp` | 不依赖桌面端；权限继承 Host（通常非管理员）→ 扫描走目录遍历；内存态私有 |
| 本机端点 | 桌面端启动时自动开启 | 端点文件 `~/.flashdir/mcp-endpoint.json`（`{port,pid,url}`，不含 token） |

**关键实现点**

- 端点每连接一个独立任务（可并发多客户端）；关闭/改端口由每秒一次的设置检查驱动，
  **只停止 accept 并删除端点文件，不会主动断开已建立的连接**（SSE 流最长约 1 小时）
- 令牌校验失败立即 401；请求体/请求头超限分别 413/431；等待桌面端上限 25s；索引就绪等待 3s
- 桌面端状态栏展示 `MCP 已连接 · 最近调用`（客户端名 + 累计次数），点击可复制配置
- 自测：`--selftest` / `--selftest-endpoint` / `--selftest-bridge`（需桌面端）

## 9. 工具集与设置

**13 个工具**：`list_volumes`、`search_files`、`scan_directory`、`list_directory`、`cache_stats`、
`diagnostics`、`find_large_files`、`find_duplicates`、`analyze_dev_cache`、`list_snapshots`、
`save_snapshot`、`compare_snapshots`、`disk_usage_trend`（入参与 limit 见第 4 节）。

- 统一标注：见第 4 节"工具标注"
- 取数：先查进程内内存缓存，未命中才走扫描流水线。响应里 `source` 字段标明来源：
  `scan_directory` / `list_directory` 用扫描层来源（`memory` / `disk` / `usn` / `usn-disk` /
  `memory-derived` / `disk-derived` / `scan`），`save_snapshot` 命中进程内缓存时为 `memory-cache`
- 响应体积控制：列表类工具都有 `limit`；`search_files` / `scan_directory` / `list_directory` /
  `find_large_files` / `find_duplicates` 额外带 `truncated`

**设置（桌面端可热改）**

- `~/.flashdir/mcp-settings.json`：`{ "enabled": bool, "port": u16 }`
- 端点线程每秒检查设置：关闭 → 停止 accept（不再监听新连接）并删除端点文件；
  改端口 → 退出当前监听循环后按新端口重绑（旧端口不再接受新连接，约 1 秒内完成，无需重启桌面端）
- 桌面端设置弹窗（标题栏齿轮 / 命令面板"设置"）提供开关、端口、当前地址与复制地址

# FlashDir Release Notes

## Unreleased（十一）—— 原生 MCP 服务器（P0）

- 新增 **`flashdir-mcp.exe`**：stdio + JSON-RPC 2.0（NDJSON）的 MCP 服务器，
  纯 Rust 实现，无 Node/Python 依赖；与桌面端共享磁盘缓存与全局索引
- 工具（6 个，全部只读）：`list_volumes`、`search_files`（Everything 语法 + 命中总数 + offset 分页）、
  `scan_directory`、`list_directory`、`cache_stats`、`diagnostics`
- 协议：版本协商（2025-06-18 / 2025-03-26 / 2024-11-05）、`notifications/cancelled` → 取消扫描、
  标准错误码（-32700/-32601…）；stdout 只输出协议消息，日志全部走 stderr
- 启动即后台加载持久化索引；`search_files` 会短暂等待索引就绪（最多 3s）再返回
- `--selftest` 脚本化自测（11 项检查，退出码即结果）；实测 stdio 端到端 5 条响应全部合法 JSON
- 设计文档：`docs/mcp-design.md`（含 P1/P2 规划：重复文件、开发缓存、快照、HTTP/SSE、resources）
- 界面：目录树去掉加载圆圈与占位文字（刷新时保留旧内容，不再闪空）

## Unreleased（十）—— 搜索"查看更多"能力

此前搜索结果是硬上限：命令面板 60 条、主搜索视图 1000 条，且没有总数、无法继续加载。

### 后端
- `search_with_filter_paged(query, limit, offset) -> (结果, 命中总数)`：
  命中总数在 top-K 收集过程中顺带统计（每线程计数后归并），不受 limit/offset 影响
- `global_search(query, limit, offset)`：limit 上限提到 20 万，响应新增 `total` / `truncated`
- 新增单测：分页不重叠、最后一页余量、越界返回空、总数与 limit 无关

### 前端
- 结果视图 `SearchResults.vue` 重写：
  - **虚拟滚动**（只渲染可视窗口内的行，5 万条也流畅）
  - 头部显示"命中 N 项 · 已显示前 M 项 · 索引规模 · 耗时"
  - **加载更多**：按钮 + 滚到底部自动续加载（1000 条/页）
  - **导出全部**：最多 5 万条导出为 CSV（带 BOM，Excel 直接打开）
  - 键盘：↑↓ / PgUp PgDn / Enter 打开 / Esc 返回；追加分页不重置滚动位置
- 命令面板：显示"共 N 项命中"，`Ctrl+Enter` 打开完整结果视图（面板仍只取 60 条做快速跳转）

## Unreleased（九）—— 搜索响应结构修复 + 窗口/布局健壮性

### 修复：命令面板报 `g.value.map is not a function`，搜索不可用
- 根因：后端 `global_search` 返回的是**对象**
  `{ ready, state, results, indexSize?, sampleNames? }`，而前端（命令面板与主搜索框）
  直接把它当数组用（`results.value = rows || []` → `rows.map(...)` 崩溃），
  组件渲染抛错、命令面板直接不可用
- 修复：新增 `utils/globalSearchApi.js` 统一归一化响应；两处调用点都改用它
- 顺带用上后端已有的诊断字段：索引未就绪 → 明确提示并触发构建；
  有索引但无匹配 → 提示"索引 N 项无匹配 + 索引内名称示例"
- **新增契约测试**（lib 内，可被 `cargo test --lib` 覆盖）：
  把 `GlobalSearchResponse` 从 bin 的 commands.rs 移到 lib 的 global_search.rs，
  并断言 `results` 是数组、字段为 camelCase；卷列表同样断言为数组

### 窗口与布局健壮性
- 工作区读数可信度校验：`SPI_GETWORKAREA` 在远程桌面/显示旋转/DPI 虚拟化下可能返回
  不可信值（实测同机出现过 1256x2376 的"竖屏"工作区），现在与 Tauri 显示器信息交叉校验，
  不可信则直接最大化
- 窄窗口响应式：≤1100px 收窄两侧栏；≤880px 隐藏检查器；≤620px 隐藏目录树，
  始终保证文件表可用（云桌面/竖屏/分屏场景）

## Unreleased（八）—— 小屏/高 DPI 窗口适配

### 修复 2：状态栏不贴底 / 与洞察坞"黏连"（真正的根因）
- 骨架此前用固定 6 行的 grid，而"扫描进度条"是 `v-if`：不渲染时子元素只有 5 个，
  于是**所有行错位一格** —— 主区域落到 `auto` 行、洞察坞落到 `1fr` 行、状态栏落到 `auto` 行，
  最后一行（24px）空置 → 状态栏浮在离窗口底部 24px 处，洞察坞下方还多出 34px 空隙
- 改为 **flex 列骨架**（标题栏/工具栏/进度条/状态栏 `flex: 0 0 auto`，主体 `flex: 1 1 auto; min-height: 0`），
  对子元素数量变化免疫；并补回标题栏 32px、工具栏 38px 的固定高度
- 实测（视口 1228x588）：标题栏 0-32 · 工具栏 32-70 · 主体 70-319 · 洞察坞 319-466（内容 348-466）·
  状态栏 466-490（**贴底 = true**，无空隙）

### 修复 1：底部状态栏与洞察坞内容被切在屏幕外
- 现象：云桌面环境逻辑可视区仅 1228x588（200% 缩放），而窗口默认 1200x800，
  窗口底部超出屏幕 → 洞察坞只露出标签行、状态栏完全看不见
- 修复：启动时读取显示器工作区（`SystemParametersInfoW(SPI_GETWORKAREA)`），
  窗口超出则先收敛尺寸并居中；随后二次校验，若因 DPI/边框差异仍超出则最大化，
  由系统保证窗口完整落在工作区内
- 布局同步收紧：洞察坞高度改为 `min(196px, 30vh)`（最小 88px），
  窄窗口（≤1240px）自动隐藏状态栏次要信息段，避免主信息被挤掉
- `minHeight` 600 → 520，保证小屏也能完整显示
- 目录树"最近扫描"按路径去重（此前同一路径会重复出现）

## Unreleased（七）—— 搜索修复、布局修复与界面精简

### 搜索（用户反馈"无法正常搜索"）
- **`!` 前缀否定此前根本没被解析**：只实现了 `NOT`，`!tmp` 被当成字面量 `"!tmp"` 去匹配，
  结果恒为空（界面与文档一直宣传 `!tmp`）。现已支持，与 `NOT` 等价，并加单测
- **过滤匹配"相对扫描根的路径"**：此前扫 `C:/Windows` 时输入 `windows` 会命中全部条目
  （每个条目绝对路径都含 C:/Windows），看起来像"过滤没生效"；现在按相对路径匹配
- **主搜索框支持回车做全局搜索**：新增搜索结果视图（名称/路径/大小/时间，双击打开、
  ↑↓ 选择、Esc 返回），索引未建立时给出提示并自动触发构建
- 命令面板 `Ctrl+K`：`Tab` 可在"文件搜索 / 命令"之间来回切换（此前只能单向进入命令模式，
  用户会以为搜索坏了）；右键"全局搜索同名文件"会预填关键字

### 布局
- **底栏有时不在窗口底部 / 与洞察坞"黏连"**：主区域行改为 `minmax(0, 1fr)`（可压缩、内部滚动），
  洞察坞固定高度但随视口收敛（`min(208px, 36vh)`，最小 96px），保证状态栏恒在窗口底部；
  状态栏换用更明确的底色与分隔线
- 洞察坞去掉手动拖拽调整（保留 `Ctrl+J` 隐藏），并移除与工具栏"热图"重复的"体积构成"标签

### 健壮性
- 非 Tauri 环境或 IPC 异常时不再中断模块加载（`listen`/`invoke` 全部加防护）
- 新增**可见错误条**：组件渲染错误会在底部显示一条错误提示（生产构建会移除 console.*，
  此前会静默白屏，极难排查）
- 生产压缩从 terser 换回 Vite 默认 esbuild；移除已不再使用的 ant-design 空分包

### 清理
- 删除 `design/` 目录（设计样例已完成使命）

## Unreleased（六）—— 界面收尾：可调洞察坞 / 真热图 / 增长趋势 / 面板重排 / 树键盘导航

### 修复
- **洞察坞被卡在底部、无法调整**：新增顶部拖拽条（双击复位），最小高度 120px，
  上限自动按窗口高度收敛（始终给文件表留出 ≥150px）；高度写入 localStorage 记忆；
  面板内容各自内部滚动，不再挤压主表

### 新增
- **体积构成改为真 squarified treemap**（Bruls/Huizing/van Wijk）：面积严格 ∝ 体积，
  格子长宽比优化，按容器尺寸自适应标签（名称+大小 / 仅大小 / 无标签），点击进入子目录
- **增长趋势面板**（`Ctrl+3`）：以历史快照的真实总量画趋势柱（含相邻差值），
  一键对比最近两次 / 对比当前，给出净变化与新增·删除·修改 Top 8（可点击定位）
- **目录树键盘导航**：`↑↓` 移动、`→` 展开或进入子项、`←` 折叠或回父项、
  `Enter` 扫描、`Home/End` 首尾，焦点行有强调色描边

### 重排
- 三个功能面板按新设计规范重写：
  - 开发缓存：类别行（占比条 + 项数 + 占比）+ 展开显示 Top 项，能反查路径的直接打开
  - 重复文件：按组折叠（大小 × 数量 → 可回收空间），组内列出全部路径与修改时间
  - 快照对比：快照列表可勾选两份对比 + 新增/删除双列 diff + 摘要（净变化/增删改计数）

## Unreleased（五）—— 界面重构（指挥台方向）

### 全新界面
- 结构：范围条（卷容量条 + 命令入口 + 面板开关）/ 工具栏（导航 + 扫描控制 + 路径面包屑 +
  过滤语法框 + 列表·热图 + 导出 CSV）/ 常驻目录树（无限层级、占父目录占比条、最近扫描）/
  文件表（`Alt+1..6` 排序、键盘导航、右键菜单、分页）/ 检查器 / 洞察坞 / 状态栏
- 设计令牌：深色与浅色两套变量 + `prefers-color-scheme` 跟随系统，可手动切换并记忆；
  12px/23px 密度、3px 圆角、1px 分隔线、单一强调色 + 功能热力色、等宽数字、
  内联单色 SVG 图标；不使用渐变/玻璃拟态/emoji/大圆角
- 命令面板 `Ctrl+K`：文件搜索与命令同框；新增快捷键（见 README「界面」）
- 洞察坞：体积构成热图 / 大文件 / 重复文件 / 快照对比 / 开发缓存，`Ctrl+1..5` 切换
- 状态栏可解释：结果来源（内存命中 / 磁盘 blob / USN 增量 / MFT 直读 / 上层推导）、
  已校验 USN、索引规模、非管理员降级入口
- **全程只读**：不提供删除动作，右键仅"打开位置 / 复制路径 / 在此过滤 / 查重复 / 保存快照"

### 新增能力
- `get_volumes`：Win32 枚举卷容量/卷标/文件系统/类型，供容量条与扫描目标选择（含单测）
- 访问时间：MFT `$FILE_NAME.AccessTime` 与 walker `ftLastAccessTime` 解析进条目，
  分页支持按访问时间排序；若卷未启用访问时间更新则表头标注并回退显示修改时间
- 扫描结果回传 `cacheSource`；磁盘 blob 版本号提升到 2（旧缓存自动失效重扫）

### 清理
- 删除旧组件（FileList/Sidebar/TreeNode/RightPanel/StatsTab/HistoryList/GlobalSearchDropdown）
- 前端移除 ant-design-vue 运行时依赖与全局注册，样式全部走自有设计令牌

## Unreleased（四）—— 缓存读取与索引内存再优化

### 磁盘缓存：blob 移出 SQLite
- blob 改存 `~/.flashdir/blobs/<fnv64>.bin`（含 magic/version/path 头，原子写、读取校验），
  SQLite 只留元信息：实测 72MB 文件顺序读 **24ms**、305k 条完整加载 **158ms**
- 缓存命中 **0.83s → 0.40-0.50s**；WAL 峰值 **274MB → 186KB**；
  VACUUM 后 DB **519MB → 371MB**；淘汰/TTL/clear 同步删文件，启动清理孤儿文件
- 修 bug：全量扫描前的级联失效会把 `C:/` 之下所有目录缓存一起删掉
  （实测扫 C:/ 后 C:/Windows、C:/Users 的 blob 全被清空）

### 全局索引：再省 25% 内存
- `IndexEntry` 去掉 `name`/`ext` 字段（name 序列化时由 path 派生、ext 过滤时现算）
- 路径索引 key 由 `String` 改为 **128 位哈希**（命中后用 arena 内 path 校验）
- GUI 常驻 **673MB → 506MB**（最初 938MB-1.5GB）；索引构建 **1.08s → 0.67s**

## Unreleased（三）—— 性能专项（实测驱动）

> 所有改动都有 before/after 实测；完整数据见 README「性能实测」。

### 全局搜索（114 万条真实索引）
- 改为**每线程 top-K 堆 + 归并**，不再"克隆全部命中再排序截断"：
  `NOT *.tmp` **706ms → 30ms**、单字符 `a` **448ms → 38ms**
- `dir:` 过滤去掉逐条 `to_lowercase()`（百万次 String 分配）
- 索引改 **arena**（`Vec<IndexEntry>` + `path→u32` + `首字符→Vec<u32>`），
  不再复制 name/ext/桶内路径 → GUI 常驻 **938MB → 705MB**、
  分桶查询 **12ms → 2ms**、索引构建 **1.89s → 1.08s**

### 磁盘缓存
- **每目录一行 blob**（bincode）替代"每条目一行 + 3 索引"：
  C:/Windows 首扫 **47s → 3.4s**（写缓存 42s → 0.9s）、
  缓存命中 **1.1-7.6s → 0.8s**
- 缓存写入**后台线程**（FIFO + flush），GUI 扫描不再等待落盘
- 删除 4 个从未被查询使用的索引；SQL 排序改内存排序（省 ~0.7s/30 万行）
- 不再逐条预格式化 `sizeFormatted`（40 万条省 ~55ms，DB 行更小）

### MFT 扫描
- `HashMap<FRN, Entry>` → **记录号即下标的 Vec**；记录解析 rayon 并行
- 路径构建：children_map + DFS（逐子节点 clone 父路径）→ **父链 + 记忆化**
- C:/Windows 端到端 **6.45s → 3.4s**（读+解析 2.2s→1.3s、建路径 0.2s）

### USN 增量
- 阈值按实测校准：增量约 1.67ms/条 vs 全量 2.4-3.5s →
  `MAX_USN_CHANGES` **5000 → 1200**（超过直接全量，更快更稳）
- 新增"USN 不可用原因"诊断日志，避免无声退回 mtime 新鲜度

## Unreleased（二）—— 实机验证中发现的 USN 深层缺陷

> 这一批问题都是"编译通过 + 实机跑一遍"才暴露出来的：USN 增量此前从未真正成功过。

- **`FSCTL_READ_USN_JOURNAL` 常量少了 METHOD_NEITHER 位**：应为 `0x000900BB`，
  原值 `0x000900B8`（METHOD_BUFFERED）被驱动直接拒绝并返回
  `ERROR_INVALID_FUNCTION`，导致增量链路永远走不通。
- **`USN_JOURNAL_DATA.MaxUsn` 不是位置**：它是 Journal 的 USN 上限常量
  （NTFS 上恒为 2^63-2^16）。检查点字段改为 `NextUsn`（下一个待分配 USN），
  并新增范围校验：校验点低于 `LowestValidUsn` 或高于 `NextUsn` 时判为
  窗口失效 → 全量扫描（历史版本写入的错误值会被自动纠正）。
- **USN 判定"窗口失效"后必须真正跳过缓存**：此前只打日志、仍回落到 mtime 缓存，
  继续返回旧数据；现在置位 `force_full_scan` 直接走全量 MFT。
- **FRN→路径解析要还原"记录发生时"的祖先名称**：若父目录在同一增量窗口内被
  改名，早期记录（删除/修改）会解析到改名后的新路径而匹配不上缓存，删除被
  静默丢弃。现在按记录的 USN 回溯改名历史取旧名。
- **单条 MFT 记录解析补 `$DATA` 大小回退**：`$FILE_NAME.RealSize` 对常驻/小文件
  常为 0，全量解析器有回退而单条解析器没有，导致增量更新把文件大小写成 0。
- 旧库兼容：没有 `index_meta` 时按"全盘索引"处理，避免升级后误显示"部分目录"。
- 前端依赖：`tailwindcss@^3.4.29` 在 npm 上不存在（3.x 最新为 3.4.19），
  `npm ci` 必然 404；已修正版本并重新生成 lockfile（vite 7.3.6 / vue 3.5.43）。
- **`Cargo.toml` 缺少 `custom-protocol` feature**：Tauri 用它判定 dev/prod，
  直接 `cargo build --release` 会生成"dev 模式"二进制（只嵌 `devUrl`，
  打开后显示 localhost 拒绝连接）。已补上标准 feature 声明，
  并在 README 明确构建命令。

## Unreleased —— 正确性 / 新鲜度 / 性能修复

### 扫描新鲜度（重要）
- **USN 增量真正生效**：修正 `FSCTL_READ_USN_JOURNAL` 输出解析 —— 缓冲区前 8 字节
  是"下次读取起点"，之前从 offset 0 解析会把该值当成 `RecordLength`，导致增量被
  静默跳过、却把过期缓存当作最新结果回写。
- **按目录记录已校验 USN**：`scan_meta.verified_usn` / 内存缓存条目记录
  "这份数据已校验到哪个 USN"，只有非零时才做增量；Journal 回滚或变更 > 5000 条时
  回退全量 MFT 扫描，不再返回陈旧数据。
- **增量循环拉取**：单次读取不再只取一个 4MB 批次就推进检查点（那会永久丢失
  窗口之外的变更），改为循环追平 Journal。
- **只应用被扫描目录内的变更**，修复"扫 A 目录却混进 B 目录新增文件"的污染问题。
- **缓存路径格式统一为绝对路径**：修复 USN 增量与缓存 key 不一致导致的
  删除失效、重复条目、相对路径外泄。
- **子目录缓存推导加 mtime 约束**：父缓存写入时间早于子目录 mtime 时不再推导，
  避免子目录永远返回父快照里的旧数据。
- 新增工具栏 **刷新** 按钮：忽略内存/磁盘/USN 缓存强制重扫。

### 性能
- 分页/排序/过滤/目录树改为共享 `Arc<Vec<Item>>`，不再每次请求深拷贝全量条目。
- 磁盘缓存容量统计改为字节级真实记账，淘汰按"整份目录"回收至 75%，
  修复"计数只增不减 → 每次写入都淘汰缓存"的抖动。
- 全局搜索：候选集先做 O(n) 部分选择再排序；不再对百万级候选全量排序。
- 重命令（重复文件哈希、开发者分析、快照 diff、全局搜索、索引追加）下沉到
  `spawn_blocking`，不再阻塞 IPC 主线程。
- 名称排序改为免分配的 Unicode 小写比较。

### 其他修复
- 本地过滤真正支持 Everything 式语法（`ext:` / `size:` / `type:` / `dir:` /
  `mtime:` / `NOT` / 通配符），与工具栏提示、README 一致。
- 全局搜索不再把 `NOT 文本` 当作分桶关键字；未识别的 `key:value` 不再被丢弃；
  过滤条件为空时返回空结果，而不是退化成"返回全量中最大的若干项"。
- Top 5 大文件独立按大小挑选，不再受当前排序影响。
- 目录树前缀基于 canonicalize 路径构造，修复用户输入大小写不一致时目录树为空。
- 快照只序列化条目列表；移除"用当前页 100 条当全量快照"的错误回退。
- GUI 历史记录恢复写入（分页重构后曾丢失）。
- 全局索引区分"全盘构建"与"部分目录"（`partial`），恢复 `failed_drives` 统计。
- MFT 记录编号在短读时不再错位；目录遍历失败会告警而不是静默漏扫；
  移除 `FIND_FIRST_EX_CASE_SENSITIVE`。
- 取消改为"代（generation）"模型：新扫描不再清掉在飞扫描的取消请求。
- 提权重启成功后旧进程立即退出，避免双实例/双托盘。

## FlashDir v3.4.2 Release Notes

## What's New

- **New UI**: observability-dashboard layout with brand, overview cards, wider side panel
- **Directory tree**: lazy-loaded, starts collapsed, collapses with one click, bounded scroll area
- ~~**Treemap**~~: 已在后续重构中移除（README 已同步）
- **Duplicate file detection**: size + content hash grouping
- ~~**Directory change watching**~~: 已移除；USN 能力改为服务于"扫描时的增量校验"
- **Scan cancellation**: cancel long scans from the toolbar
- **Runtime diagnostics**: cache/index/USN/permission status panel
- **About dialog**: project info and links

## Performance

- Directory aggregation optimized from O(files × depth) to O(files + dirs)
- Disk cache switched from BLOB snapshots to item-level SQLite rows
- Subdirectory scans derived from upper-level memory/disk caches
- Frontend uses backend paging, no longer sends full item lists
- Global search index:
  - batch restore from SQLite
  - parallel MFT scanning across volumes
  - streamed persistence without full Vec clone
  - cached extension field for faster `ext:` filtering
  - incremental disk index updates

## Fixes

- Fixed frontend crash on large directory scans
- Filtered MFT `<record_xxx>` placeholders from results/tree
- Fixed global search status stuck at "0 items"
- Fixed history/checkpoint atomic writes
- Cleaned dead code and stale dependencies

## Build

- Platform: Windows
- Requires Windows 10/11
- Admin recommended for MFT direct scan

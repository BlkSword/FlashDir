<div align="center">

<img src="src-tauri/icons/128x128@2x.png" alt="FlashDir" width="96" height="96" />

# FlashDir

**磁盘可观测性平台 —— 不止于"谁占了我的磁盘"，而是"我的磁盘在过去一周发生了什么变化"**

FlashDir 是一款面向 Windows 的磁盘空间分析与可观测性工具：
直接读取 NTFS 主文件表（$MFT），全盘 64 万+文件约 6 秒扫描完成；
USN Journal 增量刷新让重复扫描接近秒级；内置开发者目录分析、快照对比、
重复文件检测、Everything 式智能过滤与跨盘全局文件搜索。

[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-2021%2B-orange.svg)](https://www.rust-lang.org)
[![Tauri](https://img.shields.io/badge/tauri-2.0-blue.svg)](https://tauri.app)
[![Version](https://img.shields.io/badge/version-3.4.2-green.svg)](src-tauri/Cargo.toml)

</div>

---

## 为什么用 FlashDir？

### 它是 Everything + SpaceSniffer 的融合体

| 能力 | Everything | WizTree | SpaceSniffer | **FlashDir** |
|------|-----------|---------|-------------|-------------|
| MFT 直读扫描 | ✅ | ✅ | ❌ | ✅ |
| USN 增量刷新 | ✅ | ❌ | ❌ | ✅ |
| 目录树 / 分页列表 | 一般 | ✅ | ✅ (Treemap 最佳) | ✅ |
| 开发者目录识别 | ❌ | ❌ | ❌ | ✅ |
| 快照对比 / 增长追踪 | ❌ | ❌ | ❌ | ✅ |
| Everything 式过滤 | ✅ | ❌ | ❌ | ✅ |
| 跨盘全局搜索 | ✅ | ❌ | ❌ | ✅ |
| 开源 | ❌ | ❌ | ❌ | ✅ |

### 性能

| 工具 | 扫描 C 盘全盘（64 万文件） |
|------|--------------------------|
| Windows 资源管理器 | 数分钟（经常卡死） |
| PowerShell `gci -r` | ~5 分钟 |
| **FlashDir（目录遍历）** | ~3 分钟 |
| **FlashDir（管理员 MFT）** | **~6 秒** 🏆 |
| **FlashDir（USN 增量）** | **< 50ms** 🚀 |

> 实际耗时受磁盘类型、文件系统碎片程度和系统负载影响。

---

## 快速开始

### 桌面应用（GUI）

从 [Releases](https://github.com/BlkSword/FlashDir/releases) 下载 `FlashDir.exe`，双击运行。

1. 输入目录路径或点击**浏览**选择
2. 点击**扫描**
3. 在右侧面板切换视图：
   - **总览** — 总量/文件数/目录数 + Top 5 大文件
   - **开发者** — 自动识别 18 类开发工具目录的空间占用
   - **快照** — 保存扫描历史、对比任意两次扫描的增长变化
   - **重复** — 按大小 + 内容哈希识别重复文件，估算可回收空间
4. 工具栏的 **刷新** 按钮会忽略内存/磁盘/USN 缓存，强制重新扫描当前目录
   （日常"扫描"会优先走缓存与 USN 增量校验）
4. 点击顶部工具栏的搜索框，或使用 **Ctrl+K** 打开**全局搜索**：
   - 跨盘搜索所有已索引文件
   - 支持 `*.pdf`、`report*`、`*2024`、`ext:zip`、`size:>1GB` 等语法
   - 首次使用需建立全盘索引（约几秒至几十秒）
5. 点击工具栏右侧的**诊断**按钮，可查看缓存、索引、USN 检查点、权限与数据目录状态，便于排查问题。

> 💡 以**管理员身份运行**可启用 MFT 直读模式，扫描速度通常提升 **30-60 倍**，同时全局搜索也能索引所有 NTFS 卷。

### 命令行工具（CLI）

CLI 与 GUI 共用同一个 Rust 后端，结果完全一致。

```bash
# 构建 CLI
cd src-tauri && cargo build --release --bin cli

# 快速扫描
./target/release/cli.exe C:\Users\Downloads

# 全盘扫描 Top 50
./target/release/cli.exe C:\ --top 50

# 禁用 MFT，使用目录遍历
./target/release/cli.exe C:\Users --no-mft

# 强制刷新缓存
./target/release/cli.exe C:\ --no-cache
```

输出示例：

```
$ cli.exe C:\Windows --top 10

SIZE     TYPE       NAME
--------------------------------------------------------------------
 10.2 GB <DIR>      WinSxS
 1.81 GB <DIR>      System32
 1.18 GB <DIR>      servicing
 1.13 GB <DIR>      LCU
 705 MB <DIR>      amd64_microsoft-windows-dynamic-image_...
 702 MB <DIR>      SysWOW64
 384 MB <DIR>      Installer
 353 MB <DIR>      n
 309 MB <DIR>      spool
 308 MB <DIR>      drivers

1 个文件 | 总计 15.1 GB | 扫描耗时 4.95 秒
```

---

## 界面

界面按"专业工具"方向设计：信息密度优先、键盘可达、状态可解释，**全程只读**（不提供任何删除动作）。

```
┌ 范围条：卷容量条 · 命令入口(Ctrl+K) · 面板开关 ───────────────┐
├ 工具栏：← → ↑ · 扫描/取消/刷新/强制/浏览 · 路径面包屑 · 过滤 · 列表/热图 · 导出
├──────────┬────────────────────────────────────┬──────────────┤
│ 目录树    │ 文件表（名称/大小/占父目录/修改/访问/提示） │ 检查器        │
│ 无限层级  │ 排序 Alt+1..6 · 键盘导航 · 右键菜单 · 分页   │ 详情/快捷操作 │
│ ↑↓←→ 导航 │                                                │               │
├──────────┴────────────────────────────────────┴──────────────┤
│ 洞察坞（固定高度，随窗口自动收敛；Ctrl+J 隐藏）：大文件 · 增长趋势 · 重复 · 快照 · 开发缓存 │
├ 状态栏：MFT直读 · USN已校验 · 缓存层级 · 索引规模 · 降级入口 ────┤
```

- **主题**：默认跟随系统（`prefers-color-scheme`），可切换深色/浅色并记忆
- **过滤语法常驻可见**：与全局搜索同一套语法（`ext:` `size:` `dir:` `mtime:` `type:` `!`），空结果给出语法提示
- **状态可解释**：状态栏固定显示本次结果的来源（内存命中 / 磁盘 blob / USN 增量 / MFT 直读 / 上层推导）、
  已校验 USN、全局索引规模；未以管理员运行时会提示并给出"以管理员重启"
- **访问时间列**：读 NTFS `$FILE_NAME.AccessTime`；若该卷未启用访问时间更新（Windows 默认策略），
  表头会标注"未启用"并回退显示修改时间，不伪装成热度
- **体积构成**：工具栏"热图"切换整屏 squarified treemap（Bruls 算法），面积严格 ∝ 体积，点击进入子目录
- **增长趋势**：基于历史快照的真实趋势柱 + 对比结果（新增/删除/修改 Top 8），数值以标签为准
- **窗口自适应工作区**：启动时检测显示器工作区，窗口超出（小屏 / 高 DPI / 云桌面）时先收敛尺寸、
  必要时最大化，保证洞察坞内容与状态栏始终完整可见
- **主搜索框两种用法**：输入即**过滤当前目录**（匹配名称 + 相对扫描根的路径，所以扫 `C:/Windows`
  时输入 `windows` 不会再"全部命中"）；按 **Enter** 用全局索引搜索整个磁盘并在主区域显示结果
- **过滤语法**：`ext:` `name:` `dir:` `size:` `mtime:` `type:` `*.ext` `前缀*` 以及 `!xxx` / `NOT xxx` 否定
  （此前只实现了 `NOT`，`!tmp` 会被当字面量导致结果恒空，已修复）
- **状态可解释**：状态栏固定显示本次结果来源（内存命中 / 磁盘 blob / USN 增量 / MFT 直读 / 上层推导）、
  已校验 USN、全局索引规模；未以管理员运行时会提示并给出"以管理员重启"

### 快捷键

| 快捷键 | 作用 |
|--------|------|
| `Ctrl+K` | 命令面板 / 全局文件搜索（`Tab` 切命令模式） |
| `Ctrl+F` | 聚焦过滤框 |
| `F5` / `Ctrl+Shift+R` | 刷新（USN 快路径） / 强制全量重扫 |
| `Esc` | 取消扫描 / 关闭浮层 |
| `↑ ↓` `Enter` `Space` `Backspace` | 选择 / 打开 / 预览 / 上一级 |
| `Alt+1..6` | 按对应列排序 |
| `Ctrl+B` `Ctrl+J` `Ctrl+I` | 目录树 / 洞察坞 / 检查器开关 |
| `Ctrl+1..5` | 切换洞察标签（大文件/增长趋势/重复/快照/开发缓存） |
| `Enter`（搜索框内） | 用全局索引搜索整个磁盘 |
| 树内 `↑ ↓ ← →` `Enter` `Home` `End` | 目录树键盘导航（移动/展开折叠/扫描） |

## 核心功能

### 🔎 全局文件搜索

工具栏内置 Everything 风格的**跨盘全局搜索**。首次启动时后台构建一次全盘索引（读取各 NTFS 卷的 $MFT），之后常驻内存，搜索仅为内存过滤，毫秒级返回结果。

| 语法 | 示例 | 效果 |
|------|------|------|
| 扩展名简写 | `*.pdf` / `.pdf` | 仅显示 .pdf 文件 |
| 前缀匹配 | `report*` | 文件名以 report 开头 |
| 后缀匹配 | `*2024` | 文件名以 2024 结尾 |
| 包含匹配 | `*mid*` | 文件名包含 mid |
| `ext:zip` | `ext:zip` | 按扩展名过滤 |
| `name:report` | `name:annual` | 文件名包含关键字 |
| `size:>100MB` | `size:>1GB` | 按大小过滤 |
| `mtime:>7d` | `mtime:<1h` | 按修改时间过滤 |
| `type:file` | `type:dir` | 仅文件或仅目录 |
| `dir:Downloads` | `dir:"Program Files"` | 路径包含指定目录 |
| 否定 | `NOT *.tmp` | 排除 .tmp 文件 |
| 组合 | `report *.pdf size:>1MB` | 多条件同时满足 |

- 索引支持 SQLite 持久化缓存，重启后可秒级恢复
- 主界面扫描完成后自动将结果追加到全局索引
- 结果支持打开文件、打开所在文件夹、复制路径/文件名

### 🔍 本地智能过滤

在主界面文件列表的搜索框中，也可使用 Everything 风格的查询语法实时过滤当前目录结果：

| 语法 | 示例 | 效果 |
|------|------|------|
| `ext:zip` | `ext:zip` | 仅显示 .zip 文件 |
| `size:>100MB` | `size:>500MB` | 大于 500MB 的文件 |
| `size:<10KB` | `size:<10KB` | 小于 10KB 的文件 |
| `type:dir` | `type:dir` | 仅显示目录 |
| `type:file` | `type:file ext:mp4` | 组合过滤 |
| `dir:node_modules` | `dir:node_modules` | 路径中包含 node_modules |
| `mtime:>7d` | `mtime:<1h` | 按修改时间过滤 |
| 纯文本 | `年报` | 按文件名或路径搜索 |
| 否定 | `NOT .tmp` | 排除匹配项 |

### 🛠️ 开发者磁盘分析

自动识别 18 类常见开发工具目录和缓存，按类别聚合空间占用：

| 类别 | 检测目标 |
|------|---------|
| 📦 Node.js | `node_modules/` |
| 🦀 Rust | `target/`、`.cargo/registry`、`.rustup/` |
| 🐍 Python | `.venv/`、`venv/`、`__pycache__/` |
| 🐘 Java/Gradle | `.gradle/`、`build/` |
| 📚 Maven | `.m2/repository/` |
| 🔷 .NET | `bin/`、`obj/`、`.nuget/packages/` |
| 🔵 Go | `go/pkg/mod/` |
| 🐳 Docker | Docker Desktop 镜像和数据卷 |
| 🐧 WSL | `ext4.vhdx` 虚拟磁盘 |
| 📱 Android | Android SDK、AVD |
| ⚡ Electron | Electron 二进制缓存 |
| 🗃️ npm | `npm-cache/_cacache/` |
| 🔀 Git | `.git/objects/` |
| 💻 VS Code | 工作区存储 |
| + 更多 | pip、NuGet 等 |

每类别显示总大小、占比百分比、Top 5 最大子项。

### 📸 快照对比与增长追踪

- **保存快照**：将当前扫描结果存档到 SQLite 数据库
- **快照列表**：查看同一目录的所有历史快照，按时间倒序
- **一键对比**：选择任意两个快照，计算精确的文件级差异
- **差异报告**：
  - 🟢 新增文件（含总大小）
  - 🔴 删除文件（含总大小）
  - 🟡 大小变化（旧→新，含 delta）
  - 📊 净变化量和增长率百分比
- 自动清理：每目录最多 50 个快照 + 30 天 TTL

---

### 🔁 重复文件检测

- 基于“先按大小分组，再对候选文件做内容哈希”的两阶段算法
- 避免对大文件全集做无意义哈希
- 输出重复文件组、组内文件列表、单组可回收空间与总可回收空间
- 使用 Rayon 并行计算文件哈希，支持设置最小文件大小过滤小文件
- 双击文件可调用系统默认程序打开

## 性能实测

测试环境：Windows 10 Pro，消费级 NVMe SSD，管理员 MFT 模式。

| 场景 | 规模 | 实测耗时 |
|------|------|---------|
| 全量 MFT 扫描（含缓存写入）C:\Windows | 305k 条 | **~3.4s**（读取+解析 1.3-2.2s / 建路径 0.2s / 聚合 0.3s / 写缓存 ~0.9s） |
| 全量 MFT 扫描 C:\Users | 290k 条 | **~2.4s** |
| 目录遍历模式（非管理员）C:\Windows | 305k 条 | ~60s |
| 内存缓存命中（同进程内翻页/排序/过滤） | 305k 条 | **~0ms**（共享 Arc，零拷贝） |
| 上层目录缓存推导（子目录） | — | **~0ms** |
| 磁盘缓存命中（外部 blob 读取 + 反序列化 + 内存排序） | 305k 条 | **~0.4-0.5s**（其中读 72MB 文件仅 ~24ms、完整加载 ~160ms） |
| USN Journal 增量（少量变更） | — | **~0.3s**（含进程启动；增量本身 <0.1s） |
| 全局搜索：分桶命中（report / node_modules） | 114 万条索引 | **1.6-3.1ms** |
| 全局搜索：全量过滤（size:>1GB / *.pdf） | 114 万条索引 | **5-30ms** |
| 全局搜索：海量命中（NOT *.tmp / 单字符） | 114 万条索引 | **35-40ms**（改造前 448-706ms） |

> **缓存与内存：**
> - 磁盘缓存 = SQLite 元信息 + `~/.flashdir/blobs/<hash>.bin`（每目录一个 bincode 文件）：
>   顺序读写（72MB 约 24ms），不占 DB/WAL；写入在后台线程完成，GUI 扫描无需等待
> - 内存缓存 LRU：30 个目录 / 200MB；磁盘缓存 500MB / 7 天（淘汰时同步删除 blob 文件）
> - 全局索引（114 万条）常驻内存约 **500MB**（arena + 路径 128 位哈希 + 首字符桶存下标），
>   索引构建约 **0.7s**

---

## 工作原理

FlashDir 采用**四级扫描流水线**，自动选择最优策略：

```
scan_directory_view()
  ├─ 第零级 —— USN Journal 增量校验 ⚡（最高优先级）
  │     只要该目录缓存记录了"已校验 USN"，就先读取 Journal 增量：
  │     目录 mtime 无法反映"文件内容被修改"，只有 USN 能捕捉这类变更
  │     仅应用落在被扫描目录内的变更，避免其它目录的变更污染结果
  │     变更超过 5000 条 / Journal 已回滚 → 自动回退全量 MFT 扫描
  │
  ├─ 第一级 —— 内存缓存（LRU，共享 Arc 零拷贝）
  │     最多 30 个目录 / 200MB
  │
  ├─ 第二级 —— 磁盘缓存（SQLite：每目录一行 bincode blob）
  │     写入后台线程异步完成；最多 500MB / 7 天过期（按最久未访问整份淘汰）
  │
  ├─ 第三级 —— 上层目录缓存推导（内存/磁盘，入口级过滤）
  │     父缓存写入时间必须 >= 子目录自身 mtime，否则视为过期
  │
  ├─ 第四级 A —— NTFS $MFT 直接读取 ⚡（Windows 管理员 + NTFS）
  │     记录号即下标（Vec）而非 HashMap；记录解析 rayon 并行
  │     路径构建走"父链 + 记忆化"，O(n) 摊还
  │     通过 $MFT 自身的 $DATA data runs 处理 MFT 碎片
  │     从未命名 $DATA 属性读取真实文件大小
  │     优先使用 Win32 长名，避免 DOS 8.3 短名
  │     扫描完成后保存 USN 检查点，并写入该目录的"已校验 USN"
  │
  └─ 第四级 B —— FindFirstFileExW 快速遍历（Windows 普通）
        原生 API，零额外系统调用，比 PowerShell 快约 3 倍
```

> 目录遍历（非管理员）无法读取 USN Journal，因此这类缓存
> `verified_usn = 0`，只能按目录 mtime 判断新鲜度；需要"内容级新鲜度"时
> 请以管理员身份运行，或点击工具栏 **刷新** 强制全量重扫。

### MFT 直读原理

在 NTFS 卷上，每个文件的元数据（名称、大小、父目录、时间戳）都存储在**主文件表（Master File Table）**中。FlashDir 直接顺序读取 $MFT，而不是递归打开成千上万个子目录。这与 [Everything](https://www.voidtools.com/) 的核心原理一致。

与早期实现相比，当前版本额外处理了：
- **MFT 碎片**：通过 `FSCTL_GET_NTFS_FILE_RECORD` 读取 `$MFT` 自身记录，解析 `$DATA` data runs，按碎片位置读取全部记录
- **文件大小修正**：当 `$FILE_NAME.RealSize == 0` 时，回退到未命名 `$DATA` 属性读取真实大小
- **长名优先**：优先使用 Win32 / Win32+DOS 命名空间，避免显示 DOS 8.3 短名
- **父 FRN 修正**：只取低 48 位记录号，去掉高 16 位序列号
- **环保护**：DFS 路径解析时维护 visited 集合，避免 MFT 父链中的环导致重复统计

### USN Journal 增量更新

每次 MFT 扫描完成后，FlashDir 会保存一个 USN 检查点，并把"扫描完成时该卷
的 USN 位置"记录到该目录的缓存元数据里（`verified_usn`）。后续扫描这个目录时：

1. 取该目录缓存（优先内存，其次磁盘）作为基底，读取 `verified_usn`
2. 从该 USN 位置开始循环读取 Journal 增量（输出缓冲区前 8 字节是下次读取起点，
   之后才是 `USN_RECORD` 数组）
3. 通过 MFT FRN → 路径解析引擎把父目录引用号还原成完整路径
4. **只保留落在被扫描目录子树内的变更**，避免把卷上其它目录的改动混进结果
5. 二阶段应用算法：Phase 1 处理删除/重命名旧名，Phase 2 处理创建/重命名新名/数据变更
6. 重新聚合目录大小，写回两级缓存，并把 `verified_usn` 推进到本次读到的位置

因为是"按目录记录已校验 USN"，所以判断是严谨的：不会出现"缓存快照比检查点更旧、
却从检查点之后读增量"导致漏变更的情况。若 Journal 已回滚（最早可读 USN 已超过
校验点）或变更超过 5000 条，会自动回退到全量 MFT 扫描。

### 快照差异引擎

```
diff(old_items, new_items) → SnapshotDiff
  ├─ Phase 1: HashMap<path, &Item> 索引（O(n)）
  ├─ Phase 2: 遍历 new → 分类为 added / modified
  ├─ Phase 3: 遍历 old → 分类为 removed
  └─ 聚合: added_total / removed_total / modified_delta / net_change / growth_percent
```

---

## 项目架构

```
FlashDir/
├── src-tauri/
│   ├── app/                          # Vue 3 前端（仅 GUI）
│   │   └── src/
│   │       ├── App.vue               # 根布局（右侧四标签面板）
│   │       ├── components/
│   │       │   ├── Toolbar.vue       # 路径输入 + 全局搜索入口
│   │       │   ├── GlobalSearchDropdown.vue # Everything 式跨盘全局搜索
│   │       │   ├── FileList.vue      # 可排序虚拟列表
│   │       │   ├── StatsTab.vue       # 文件类型分布图表
│   │       │   ├── DevAnalyzer.vue   # 开发者工具目录分析
│   │       │   ├── SnapshotCompare.vue # 快照对比与增长追踪
│   │       │   ├── Sidebar.vue       # 目录树导航
│   │       │   ├── StatusBar.vue     # 状态栏（显示管理员/MFT模式/全局索引状态）
│   │       │   ├── RightPanel.vue    # 右侧面板容器
│   │       │   ├── TreeNode.vue      # 树节点组件
│   │       │   └── HistoryList.vue   # 扫描历史
│   │       ├── composables/
│   │       │   ├── useTauri.js       # Tauri IPC 封装
│   │       │   └── useGlobalSearch.js# 全局搜索状态单源
│   │       ├── utils/
│   │       │   ├── format.js         # 格式化工具
│   │       │   └── format.js         # 格式化 / 防抖等通用工具
│   │       ├── main.js               # Vue 入口
│   │       └── style.css             # 全局样式
│   │
│   ├── src/                          # Rust 后端（GUI + CLI 共享库）
│   │   ├── lib.rs                    # 库入口
│   │   ├── main.rs                   # Tauri GUI 入口
│   │   ├── commands.rs               # Tauri IPC 命令
│   │   ├── scan.rs                   # 核心扫描引擎 + USN 增量闭环
│   │   ├── global_search.rs          # 跨盘全局文件搜索索引与过滤语法
│   │   ├── disk_cache.rs             # SQLite 缓存（含多版本快照表）
│   │   ├── dev_analyzer.rs           # 开发者目录识别引擎
│   │   ├── diff_engine.rs            # 快照差异引擎
│   │   ├── duplicate_finder.rs       # 重复文件检测
│   │   ├── perf/mod.rs               # 性能监控
│   │   ├── fs/
│   │   │   ├── mod.rs                # 平台抽象层
│   │   │   ├── mft_scanner.rs        # NTFS $MFT 读取 + FRN 路径解析
│   │   │   ├── usn_journal.rs        # USN Journal 增量读取
│   │   │   ├── windows_walker.rs     # FindFirstFileExW 零额外 syscall
│   │   │   └── fallback_walker.rs    # 非 Windows 平台回退
│   │   └── bin/
│   │       └── cli.rs                # CLI 终端工具
│   │
│   └── icons/                        # 应用图标
│
└── README.md
```

### 技术栈

| 层级 | 技术 |
|------|------|
| 核心引擎 | Rust 2021 · Tokio 异步 · Rayon 并行 |
| 桌面 GUI | Tauri 2.0 · Vue 3 · Vite · Ant Design Vue · Canvas API |
| 文件系统 | NTFS $MFT 直读 · USN Journal 增量 · FRN 路径解析 · FindFirstFileExW 快速遍历 |
| 缓存 | LRU（内存）· SQLite + bincode（磁盘 + 快照多版本） |
| 分析引擎 | KnownPattern 分类器（18 类）· HashMap O(n) 差异引擎 |
| 可视化 | 分页文件表 · 占比条 · 统计面板 · 目录树懒加载 |
| 过滤搜索 | Everything-style 语法解析 · 本地 `ext:`/`size:`/`type:`/`dir:` + 全局 `*.pdf`/`prefix*`/`*suffix`/`NOT` |
| 内存优化 | mimalloc 分配器 · SmartString 栈存储 · Arc 共享 |

---

## 开发指南

### 环境要求

- Windows 10/11
- Rust 1.70+（含 `wasm32-unknown-unknown` 目标）
- Node.js 18+

### 构建全部

```bash
# 1) 前端（必须先构建，Rust 侧会在编译期内嵌 app/dist）
cd src-tauri/app && npm ci && npm run build && cd ../..

# 2) 安装根依赖（Tauri CLI）并构建桌面端 + CLI
npm install
npm run tauri:build

# 产物
src-tauri/target/release/flashdir.exe   # GUI 桌面端
src-tauri/target/release/cli.exe        # 命令行工具
```

> ⚠️ 如果不用 Tauri CLI、直接 `cargo build`，**必须带上 `--features custom-protocol`**：
> ```bash
> cd src-tauri
> cargo build --release --features custom-protocol
> ```
> 该 feature 用于让 Tauri 走生产分支、内嵌 `frontendDist`；不启用时
> `generate_context!` 会按 dev 模式只嵌 `devUrl`，运行后会显示
> “localhost 拒绝连接”。

### GUI 开发

```bash
cd src-tauri/app && npm install
npm run dev            # 启动开发服务器 + Tauri 窗口
npm run tauri:build    # 生产构建 → src-tauri/target/release/bundle/
```

### CLI 开发

```bash
cd src-tauri
cargo build --release --bin cli
# → target/release/cli.exe
```

---

## 配置

```rust
// 扫描
const MAX_THREADS: usize = cpu * 2;      // 最多 32 个并行线程

// 内存缓存
const MAX_CACHE_ENTRIES: usize = 30;     // 最多缓存 30 个目录
const MAX_CACHE_SIZE_MB: usize = 200;    // 最大 200 MB

// 磁盘缓存
const MAX_DISK_CACHE_MB: usize = 500;    // 最大 500 MB
const CACHE_EXPIRE_DAYS: i64 = 7;        // 7 天过期

// USN 增量更新
// 实测增量约 1.67ms/条（每条要随机读 MFT 记录 + 最终整块重写 blob），
// 全量 MFT 扫描约 2.4-3.5s → 约 2000 条时持平，取 1200 留余量
const MAX_USN_CHANGES: usize = 1200;     // 单次增量超过此数直接全量 MFT（更快也更稳）
// 每个目录缓存记录自己的"已校验 USN"（scan_meta.verified_usn）

// 快照
const MAX_SNAPSHOTS_PER_PATH: usize = 50;// 每目录最多 50 个快照
const SNAPSHOT_EXPIRE_DAYS: i64 = 30;    // 快照保留 30 天

// 数据目录
// ~/.flashdir/cache_v2.db     — 磁盘缓存(scan_meta/scan_items) + 快照(snapshots)
//                                + 全局索引(global_index/index_meta)
// ~/.flashdir/history.json    — 扫描历史
// ~/.flashdir/usn_checkpoint_<盘符>.json — USN 检查点

// 平台
// 当前仅支持 Windows 10/11；fs/fallback_walker.rs 只提供目录遍历层回退
```

---

## 关于 FlashDir

FlashDir 是开源的 Windows 磁盘可观测性工具，核心目标是回答三个问题：

1. **磁盘空间被什么占用了？**
2. **这些占用在过去一段时间发生了什么变化？**
3. **我能不能在全部磁盘中瞬间找到想要的文件？**

它融合了 Everything 的文件名索引能力、WizTree 的 MFT 直读效率，以及 SpaceSniffer 的可视化表达，同时加入了开发者目录识别、快照对比、重复文件检测等面向工程实践的功能。

FlashDir 由 Rust 驱动核心性能，Vue 3 承载界面交互，Tauri 2 构建轻量桌面应用。项目完全开源，定位隐私友好：所有数据仅保存在本地，不上传、不追踪、无遥测。

## 常见问题

### 1. 点击扫描结果中的文件报错 `os error 123`

这是早期版本的已知问题：前端把文件路径当相对路径拼接，生成了 `C:/Users/C:/Users/...` 这种非法路径。当前版本已修复，请更新到最新 release。

### 2. MFT 模式没有启用 / 扫描很慢

MFT 直读需要同时满足：
- Windows 系统
- NTFS 文件系统
- 以**管理员身份**运行

不满足时会自动回退到 `FindFirstFileExW` 目录遍历，速度会慢很多。

### 3. CLI 和桌面端结果不一致

CLI（`cli.exe`）和桌面端（`flashdir.exe`）共用同一个 Rust 后端库，结果应当一致。如果发现不一致，通常是以下原因：
- 缓存不同：桌面端和 CLI 的缓存互通，但一个用了 `--no-cache` 而另一个没有
- 权限不同：一个以管理员运行，另一个没有
- 扫描时间不同：期间文件发生了变化

### 4. C 盘扫描结果出现重复目录

早期 MFT 路径解析没有处理父链中的环，可能导致根目录被重复遍历。当前版本已加入 visited 集合去重。

---


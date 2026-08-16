<div align="center">

<img src="src-tauri/icons/128x128@2x.png" alt="FlashDir" width="96" height="96" />

# FlashDir

**磁盘可观测性平台 —— 不止于"谁占了我的磁盘"，而是"我的磁盘在过去一周发生了什么变化"**

直接读取 NTFS 主文件表（$MFT），全盘 64 万+文件约 6 秒扫描完成。
USN Journal 增量刷新、开发者工具自动识别、多版本快照对比、Everything 式智能过滤，以及跨盘全局文件搜索。

[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-2021%2B-orange.svg)](https://www.rust-lang.org)
[![Tauri](https://img.shields.io/badge/tauri-2.0-blue.svg)](https://tauri.app)
[![Version](https://img.shields.io/badge/version-3.4.1-green.svg)](src-tauri/Cargo.toml)

</div>

---

## 为什么用 FlashDir？

### 它是 Everything + SpaceSniffer 的融合体

| 能力 | Everything | WizTree | SpaceSniffer | **FlashDir** |
|------|-----------|---------|-------------|-------------|
| MFT 直读扫描 | ✅ | ✅ | ❌ | ✅ |
| USN 增量刷新 | ✅ | ❌ | ❌ | ✅ |
| Treemap 可视化 | ❌ | ✅ | ✅ (最佳) | ✅ |
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
   - **📊 统计** — 文件类型分布 + Top 5 大文件
   - **🗺️ 热图** — Canvas Treemap，点击钻入子目录
   - **🛠️ 开发者** — 自动识别 18 类开发工具目录的空间占用
   - **📸 快照** — 保存扫描历史、对比任意两次扫描的增长变化
   - **🔁 重复** — 按大小 + 内容哈希识别重复文件，估算可回收空间
4. 点击顶部工具栏的搜索框，或使用 **Ctrl+K** 打开**全局搜索**：
   - 跨盘搜索所有已索引文件
   - 支持 `*.pdf`、`report*`、`*2024`、`ext:zip`、`size:>1GB` 等语法
   - 首次使用需建立全盘索引（约几秒至几十秒）
5. 点击工具栏右侧的**诊断**按钮，可查看缓存、索引、USN 检查点、权限与数据目录状态，便于排查问题。
6. 点击工具栏的**监听**按钮，可对当前目录进行近实时变更监听（基于 USN 增量刷新，约 5 秒一次）。

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

### 🗺️ Treemap 热图

- 按大小比例布局的矩形色块图，一眼识别空间大户
- 颜色编码：按文件扩展名自动配色（📄 zip=棕色 📄 mp4=红色 📄 js=黄色 📁 目录=灰色）
- **点击钻入**：点击任意目录方块，放大查看该目录内部的文件分布
- **面包屑导航**：支持回退到上级目录
- Canvas 高性能渲染，10 万+文件流畅交互
- 鼠标悬停显示文件名和精确大小

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

### 👁️ 目录变更监听

- 点击工具栏“监听”按钮，对当前扫描目录启动近实时监听
- 内部复用 `scan_directory` 的 USN 增量刷新路径，低频扫描开销小
- 检测到变更后通过 `dir-changes` 事件推送新增/删除/修改数量
- 再次点击按钮停止监听

### 🔁 重复文件检测

- 基于“先按大小分组，再对候选文件做内容哈希”的两阶段算法
- 避免对大文件全集做无意义哈希
- 输出重复文件组、组内文件列表、单组可回收空间与总可回收空间
- 使用 Rayon 并行计算文件哈希，支持设置最小文件大小过滤小文件
- 双击文件可调用系统默认程序打开

## 性能实测

测试环境：Windows 10 Pro，消费级 NVMe SSD。

| 扫描目标 | 文件+目录数 | 目录遍历 | **MFT 管理员模式** | 提速 |
|---------|------------|----------|-------------------|------|
| `C:\Windows` | ~305,000 | ~60s | **~5s** | 🔥 ~10× |
| `C:\Users` | ~223,000 | ~5s | **~4s** | 🔥 ~1× |
| `C:\` 全盘 | ~676,000 | ~180s | **~6-9s** | 🔥 ~30× |
| 项目目录 | ~33,000 | 0.5s | — | — |
| `node_modules` | ~23,000 | 0.4s | — | — |

> **缓存性能：**
> - 内存缓存命中 < 1ms
> - 磁盘缓存命中 < 5ms
> - **USN Journal 增量刷新 < 50ms**（少量变更时，首次 MFT 扫描后生效）

---

## 工作原理

FlashDir 采用**四级扫描流水线**，自动选择最优策略：

```
scan_directory()
  ├─ 第一级 —— 内存缓存（LRU）
  │     < 1ms 命中，最多 30 个目录 / 200MB
  │
  ├─ 第二级 —— 磁盘缓存（SQLite + bincode 序列化）
  │     < 5ms 命中，最多 500MB / 7 天过期
  │
  ├─ 第三级 —— USN Journal 增量更新 ⚡
  │     读取上次扫描后的变更文件，通过 MFT FRN 链解析路径
  │     二阶段应用算法（先删后增），< 50ms 返回最新结果
  │     变更超过 5000 条或检查点超过 1 小时时自动回退
  │
  ├─ 第四级 A —— NTFS $MFT 直接读取 ⚡（Windows 管理员 + NTFS）
  │     通过 $MFT 自身的 $DATA data runs 处理 MFT 碎片
  │     从未命名 $DATA 属性读取真实文件大小
  │     优先使用 Win32 长名，避免 DOS 8.3 短名
  │     扫描完成后保存 USN 检查点，供增量更新使用
  │
  └─ 第四级 B —— FindFirstFileExW 快速遍历（Windows 普通）
        原生 API，零额外系统调用，比 PowerShell 快约 3 倍
```

### MFT 直读原理

在 NTFS 卷上，每个文件的元数据（名称、大小、父目录、时间戳）都存储在**主文件表（Master File Table）**中。FlashDir 直接顺序读取 $MFT，而不是递归打开成千上万个子目录。这与 [Everything](https://www.voidtools.com/) 的核心原理一致。

与早期实现相比，当前版本额外处理了：
- **MFT 碎片**：通过 `FSCTL_GET_NTFS_FILE_RECORD` 读取 `$MFT` 自身记录，解析 `$DATA` data runs，按碎片位置读取全部记录
- **文件大小修正**：当 `$FILE_NAME.RealSize == 0` 时，回退到未命名 `$DATA` 属性读取真实大小
- **长名优先**：优先使用 Win32 / Win32+DOS 命名空间，避免显示 DOS 8.3 短名
- **父 FRN 修正**：只取低 48 位记录号，去掉高 16 位序列号
- **环保护**：DFS 路径解析时维护 visited 集合，避免 MFT 父链中的环导致重复统计

### USN Journal 增量更新

首次 MFT 扫描完成后，FlashDir 保存一个 USN（更新序列号）检查点。后续扫描时：

1. 从磁盘缓存加载过期的扫描结果（即使 mtime 不匹配）
2. 读取 USN Journal 中检查点之后的增量变更记录
3. 通过 MFT FRN → 路径解析引擎，将 USN 记录的父目录引用号转换为完整路径
4. 二阶段应用算法：Phase 1 处理删除/重命名旧名，Phase 2 处理创建/重命名新名/数据变更
5. 重新聚合目录大小，写回两级缓存

这就是 Everything 在文件变更后秒级刷新索引的技术——现在 FlashDir 也用上了。

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
│   │       │   ├── Treemap.vue       # Canvas Treemap 热图
│   │       │   ├── DevAnalyzer.vue   # 开发者工具目录分析
│   │       │   ├── SnapshotCompare.vue # 快照对比与增长追踪
│   │       │   ├── Sidebar.vue       # 目录树导航
│   │       │   ├── StatusBar.vue     # 状态栏（显示管理员/MFT模式/全局索引状态）
│   │       │   ├── RightPanel.vue    # 右侧面板容器
│   │       │   ├── TreeNode.vue      # 树节点组件
│   │       │   └── HistoryList.vue   # 扫描历史
│   │       ├── composables/
│   │       │   ├── useTauri.js       # Tauri IPC 封装
│   │       │   ├── useSortWorker.js  # 列表排序/过滤工具
│   │       │   └── useGlobalSearch.js# 全局搜索状态单源
│   │       ├── utils/
│   │       │   ├── format.js         # 格式化工具
│   │       │   ├── smartFilter.js    # Everything 式智能过滤
│   │       │   └── scanBinary.js     # 自定义二进制扫描结果解码
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
| 可视化 | Canvas Treemap · 统计面板 |
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
# 安装依赖并构建桌面端 + CLI
npm install
npm run tauri:build

# 产物
src-tauri/target/release/flashdir.exe   # GUI 桌面端
src-tauri/target/release/cli.exe        # 命令行工具
```

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
const MAX_USN_CHANGES: usize = 5000;     // 超过此数回退到全量 MFT
const CHECKPOINT_EXPIRE_SECS: i64 = 3600;// 检查点超过 1 小时过期

// 快照
const MAX_SNAPSHOTS_PER_PATH: usize = 50;// 每目录最多 50 个快照
const SNAPSHOT_EXPIRE_DAYS: i64 = 30;    // 快照保留 30 天

// 数据目录
// ~/.flashdir/cache_v2.db     — 磁盘缓存 + 快照
// ~/.flashdir/history.json    — 扫描历史
// ~/.flashdir/usn_checkpoint_<盘符>.json — USN 检查点
```

---

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


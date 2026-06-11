<div align="center">

<img src="src-tauri/icons/128x128@2x.png" alt="FlashDir" width="96" height="96" />

# FlashDir

**磁盘可观测性平台 —— 不止于"谁占了我的磁盘"，而是"我的磁盘在过去一周发生了什么变化"**

直接读取 NTFS 主文件表（$MFT），全盘 64 万+文件不到 6 秒扫描完成。
USN Journal 增量刷新、开发者工具自动识别、多版本快照对比、Everything 式智能过滤。

[![License](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-2021%2B-orange.svg)](https://www.rust-lang.org)
[![Tauri](https://img.shields.io/badge/tauri-2.0-blue.svg)](https://tauri.app)
[![Version](https://img.shields.io/badge/version-3.3.0-green.svg)](src-tauri/Cargo.toml)

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
| 开源 | ❌ | ❌ | ❌ | ✅ |

### 性能

| 工具 | 扫描 C 盘全盘（64 万文件） |
|------|--------------------------|
| Windows 资源管理器 | 数分钟（经常卡死） |
| PowerShell `gci -r` | ~5 分钟 |
| **FlashDir（普通模式）** | ~3 分钟 |
| **FlashDir（管理员 MFT）** | **5.8 秒** 🏆 |
| **FlashDir（USN 增量）** | **< 50ms** 🚀 |

---

## 快速开始

### 桌面应用（GUI）

从 [Releases](https://github.com/BlkSword/FlashDir/releases) 下载 `FlashDir.exe`，双击运行。

1. 输入目录路径或点击**浏览**选择
2. 点击**扫描**
3. 在右侧面板切换视图：
   - **📊 统计** — 文件类型分布 + Top 5 大文件
   - **🗺️ 热图** — Squarified Treemap，点击钻入子目录
   - **🛠️ 开发者** — 自动识别 18 类开发工具目录的空间占用
   - **📸 快照** — 保存扫描历史、对比任意两次扫描的增长变化

> 💡 以**管理员身份运行**可启用 MFT 直读模式，扫描速度最高提升 **76 倍**。

### 命令行工具（CLI）

```bash
# 构建
cd src-tauri && cargo build --release --bin cli

# 快速扫描
./target/release/cli.exe C:\Users\Downloads

# 全盘扫描 Top 50，JSON 输出
./target/release/cli.exe C:\ --top 50 --json

# 管理员模式下全盘 MFT 极速扫描
./target/release/cli.exe C:\ --top 20
```

输出示例：

```
$ cli.exe C:\Windows --top 10

SIZE     TYPE       NAME
--------------------------------------------------------------------
 9.59 GB <DIR>      WinSxS
 4.44 GB <DIR>      system32
 1.33 GB <DIR>      SysWOW64
 1.22 GB <DIR>      servicing
 1.13 GB <DIR>      LCU
  705 MB <DIR>      amd64_microsoft-windows-dynamic-image_...
  644 MB <DIR>      Microsoft.NET
  601 MB <DIR>      Fonts
  563 MB <DIR>      DriverStore

共 330,993 项 | 总计 20.5 GB | 扫描耗时 0.76 秒
```

---

## 核心功能

### 🔍 Everything 式智能过滤

在搜索框输入 Everything 风格的查询语法，实时过滤扫描结果：

| 语法 | 示例 | 效果 |
|------|------|------|
| `ext:zip` | `ext:zip` | 仅显示 .zip 文件 |
| `size:>100MB` | `size:>500MB` | 大于 500MB 的文件 |
| `size:<10KB` | `size:<10KB` | 小于 10KB 的文件 |
| `type:dir` | `type:dir` | 仅显示目录 |
| `type:file` | `type:file ext:mp4` | 组合过滤 |
| `dir:node_modules` | `dir:node_modules` | 路径中包含 node_modules |
| 纯文本 | `年报` | 按文件名搜索 |

### 🗺️ Squarified Treemap 热图

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

## 性能实测

测试环境：Windows 10 Pro，消费级 NVMe SSD。

| 扫描目标 | 文件+目录数 | 普通模式 | **MFT 管理员模式** | 提速 |
|---------|------------|----------|-------------------|------|
| `C:\Windows` | 330,993 | 57.8s | **0.76s** | 🔥 76× |
| `C:\Users` | 170,139 | 5.19s | — | — |
| `C:\` 全盘 | 645,901 | ~180s | **5.82s** | 🔥 31× |
| 项目目录 | 32,785 | 0.48s | — | — |
| `node_modules` | 23,289 | 0.39s | — | — |

> **缓存性能：** 
> - 内存缓存命中 < 1ms
> - 磁盘缓存命中 < 5ms
> - **USN Journal 增量刷新 < 50ms**（少量变更时，首次 MFT 扫描后生效）

---

## 工作原理

FlashDir 采用**四级扫描流水线**，自动选择最优策略：

```
scan_directory()
  ├─ 第一级 —— 内存缓存（LRU + DashMap）
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
  ├─ 第四级 A —— NTFS $MFT 直接读取 ⚡（Windows 管理员）
  │     顺序读取主文件表，64 万文件约 6 秒
  │     扫描完成后保存 USN 检查点，供增量更新使用
  │
  └─ 第四级 B —— FindFirstFileExW 快速遍历（Windows 普通）
        原生 API，零额外系统调用，比 PowerShell 快约 3 倍
```

### MFT 直读原理

在 NTFS 卷上，每个文件的元数据（名称、大小、父目录、时间戳）都存储在**主文件表（Master File Table）**中。FlashDir 直接顺序读取 $MFT，而不是递归打开成千上万个子目录。这与 [Everything](https://www.voidtools.com/) 的核心原理一致。

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
│   │       │   ├── Toolbar.vue       # 路径输入 + Everything 式搜索
│   │       │   ├── FileList.vue      # 可排序虚拟列表
│   │       │   ├── Charts.vue        # 文件类型分布图表
│   │       │   ├── Treemap.vue       # Squarified Treemap 热图 🆕
│   │       │   ├── DevAnalyzer.vue   # 开发者工具目录分析 🆕
│   │       │   ├── SnapshotCompare.vue # 快照对比与增长追踪 🆕
│   │       │   ├── Sidebar.vue       # 目录树导航
│   │       │   ├── StatusBar.vue     # 状态栏
│   │       │   └── HistoryList.vue   # 扫描历史
│   │       ├── composables/
│   │       │   ├── useTauri.js       # Tauri IPC 封装
│   │       │   ├── useSortWorker.js  # Web Worker 排序卸载
│   │       │   └── useWasmSort.js    # WASM 排序模块
│   │       ├── utils/
│   │       │   ├── format.js         # 格式化工具
│   │       │   └── smartFilter.js    # Everything 式智能过滤 🆕
│   │       └── directives/           # v-lazy 懒加载
│   │
│   ├── src/                          # Rust 后端（GUI + CLI 共享库）
│   │   ├── lib.rs                    # 库入口
│   │   ├── main.rs                   # Tauri GUI 入口
│   │   ├── commands.rs               # Tauri IPC 命令（21 个）
│   │   ├── scan.rs                   # 核心扫描引擎 + USN 增量闭环
│   │   ├── disk_cache.rs             # SQLite 缓存（含多版本快照表）
│   │   ├── dev_analyzer.rs           # 开发者目录识别引擎（18 类） 🆕
│   │   ├── diff_engine.rs            # 快照差异引擎 🆕
│   │   ├── binary_protocol.rs        # bincode 二进制序列化
│   │   ├── perf/mod.rs               # 性能监控
│   │   ├── fs/
│   │   │   ├── mod.rs                # 平台抽象层
│   │   │   ├── mft_scanner.rs        # NTFS $MFT 读取 + FRN 路径解析
│   │   │   ├── usn_journal.rs        # USN Journal 增量读取
│   │   │   ├── windows_walker.rs     # FindFirstFileExW 零额外 syscall
│   │   │   ├── iocp_scanner.rs       # IOCP 异步 I/O 扫描器
│   │   │   └── fallback_walker.rs    # 非 Windows 平台回退
│   │   └── bin/
│   │       └── cli.rs                # CLI 终端工具（2.9MB 单文件）
│   │
│   └── wasm-sort/                    # WASM 排序模块
│
└── README.md
```

### 技术栈

| 层级 | 技术 |
|------|------|
| 核心引擎 | Rust 2021 · Tokio 异步 · Rayon 并行 |
| 桌面 GUI | Tauri 2.0 · Vue 3 · Vite · Ant Design Vue · Chart.js · Canvas API |
| 文件系统 | NTFS $MFT 直读 · USN Journal 增量 · FRN 路径解析 · IOCP 异步 I/O |
| 缓存 | DashMap + LRU（内存）· SQLite + bincode（磁盘 + 快照多版本） |
| 分析引擎 | KnownPattern 分类器（18 类）· HashMap O(n) 差异引擎 |
| 可视化 | Squarified Treemap (Canvas) · Chart.js 环形图/柱状图 |
| 过滤搜索 | Everything-style 语法解析 · `ext:` `size:` `type:` `dir:` |
| 内存优化 | mimalloc 分配器 · SmartString 栈存储 · Arc 共享 · ahash |
| 排序卸载 | 三级回退：WASM（Rust）→ Web Worker（JS）→ 同步 JS |

---

## 开发指南

### 环境要求

- Rust 1.70+（含 `wasm32-unknown-unknown` 目标）
- Node.js 18+

### GUI

```bash
cd src-tauri/app && npm install
npm run dev            # 启动开发服务器 + Tauri 窗口
npm run tauri:build    # 生产构建 → target/release/bundle/
```

### CLI

```bash
cd src-tauri
cargo build --release --bin cli
# → target/release/cli.exe（2.9 MB 便携单文件）
```

### WASM 排序模块

```bash
cd src-tauri/wasm-sort
./build.sh                  # macOS / Linux
powershell -File build.ps1  # Windows
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

## License

MIT © [BlkSword](https://github.com/BlkSword)

<div align="center">

<img src="src-tauri/icons/128x128@2x.png" alt="FlashDir" width="96" height="96" />

# FlashDir

**极速磁盘空间分析工具 —— 秒级定位谁在吃掉你的硬盘**

直接读取 NTFS 主文件表（$MFT），全盘 64 万+文件不到 6 秒扫描完成。

[![License](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-2021%2B-orange.svg)](https://www.rust-lang.org)
[![Tauri](https://img.shields.io/badge/tauri-2.0-blue.svg)](https://tauri.app)
[![Version](https://img.shields.io/badge/version-3.3.0-green.svg)](src-tauri/Cargo.toml)

</div>

---

## 为什么用 FlashDir？

Windows 资源管理器计算文件夹大小需要**好几分钟**，PowerShell 的 `Get-ChildItem -Recurse` 逐个目录遍历。它们的共同瓶颈在于**递归式 I/O**——每个子目录都是一次独立的磁盘操作。

FlashDir 直接读取 **NTFS 的 $MFT（主文件表）**——一张包含卷上所有文件元数据的扁平大表。无需递归、无需逐文件系统调用，只需一次顺序读取，跑满磁盘带宽。

| 工具 | 扫描 C 盘全盘（64 万文件） |
|------|--------------------------|
| Windows 资源管理器 | 数分钟（经常卡死） |
| PowerShell `gci -r` | ~5 分钟 |
| **FlashDir（普通模式）** | ~3 分钟 |
| **FlashDir（管理员 MFT）** | **5.8 秒** 🏆 |

---

## 快速开始

### 桌面应用（GUI）

从 [Releases](https://github.com/BlkSword/FlashDir/releases) 下载 `FlashDir.exe`，双击运行。

1. 输入目录路径或点击**浏览**选择
2. 点击**扫描**
3. 结果按大小排序展示，支持图表和目录树导航

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

## 性能实测

测试环境：Windows 10 Pro，消费级 NVMe SSD。

| 扫描目标 | 文件+目录数 | 普通模式 | **MFT 管理员模式** | 提速 |
|---------|------------|----------|-------------------|------|
| `C:\Windows` | 330,993 | 57.8s | **0.76s** | 🔥 76× |
| `C:\Users` | 170,139 | 5.19s | — | — |
| `C:\` 全盘 | 645,901 | ~180s | **5.82s** | 🔥 31× |
| 项目目录 | 32,785 | 0.48s | — | — |
| `node_modules` | 23,289 | 0.39s | — | — |

> **缓存性能：** 内存缓存 < 1ms · 磁盘缓存 < 5ms · USN Journal 增量刷新 < 50ms（少量变更时）

---

## 工作原理

FlashDir 采用**多级扫描流水线**，自动选择最优策略：

```
scan_directory()
  ├─ 第一级 —— 内存缓存（LRU + DashMap）
  │     < 1ms 命中，最多 30 个目录 / 200MB
  │
  ├─ 第二级 —— 磁盘缓存（SQLite + bincode 序列化）
  │     < 5ms 命中，最多 500MB / 7 天过期
  │
  ├─ 第三级 —— USN Journal 增量更新
  │     仅读取上次扫描后的变更文件（< 50ms）
  │
  ├─ 第四级 A —— NTFS $MFT 直接读取 ⚡（Windows 管理员）
  │     顺序读取主文件表，64 万文件约 6 秒
  │
  └─ 第四级 B —— FindFirstFileExW 快速遍历（Windows 普通）
        原生 API，零额外系统调用，比 PowerShell 快约 3 倍
```

### MFT 的优势

在 NTFS 卷上，每个文件的元数据（名称、大小、父目录、时间戳）都存储在一张叫**主文件表（Master File Table）**的扁平数据库中。FlashDir 直接顺序读取 $MFT，而不是递归打开成千上万个子目录。这与 [Everything](https://www.voidtools.com/) 的核心原理一致。

### USN Journal 增量更新

首次扫描完成后，FlashDir 保存一个 USN（更新序列号）检查点。后续扫描只需读取 USN Journal 中自该检查点之后的变更记录，无需重新扫描整个卷。这就是 Everything 在文件变更后秒级刷新索引的核心技术。

---

## 功能特性

### GUI（桌面应用）

- **一键扫描**：输入路径，即刻分析
- **可排序文件列表**：按大小 / 名称 / 类型排序，虚拟滚动支撑 10 万+行
- **可视化图表**：文件类型分布（环形图）+ Top 5 大文件（柱状图），基于 Chart.js
- **目录树导航**：可展开的文件夹层级，自动聚合子目录大小
- **浏览导航**：前进 / 后退 / 上级目录，完整浏览历史
- **实时搜索**：输入即过滤，结果即时更新
- **扫描历史**：历史记录一键回访
- **管理员模式提示**：检测 MFT 不可用时显示 Tag，一键以管理员身份重启

### CLI（命令行）

- **表格输出**：对齐的人类可读终端表格
- **JSON 输出**：`--json` 配合 `jq` 等工具做脚本化处理
- **排序选项**：`--sort size`（默认，从大到小）/ `--sort name`（按名称）
- **Top-N 截断**：`--top 20` 只看最大的 20 项
- **缓存控制**：`--no-cache` 强制重扫 / `--no-mft` 禁用 MFT
- **单文件便携**：仅 2.9MB，无运行时依赖

---

## 项目架构

```
FlashDir/
├── src-tauri/
│   ├── app/                     # Vue 3 前端（仅 GUI）
│   │   └── src/
│   │       ├── App.vue          # 根布局
│   │       ├── components/      # Toolbar / FileList / Charts / Sidebar 等
│   │       ├── composables/     # useTauri / useSortWorker / useWasmSort
│   │       └── directives/      # v-lazy（IntersectionObserver 懒加载）
│   │
│   ├── src/                     # Rust 后端（GUI + CLI 共享库）
│   │   ├── lib.rs              # 库入口
│   │   ├── main.rs             # Tauri GUI 入口
│   │   ├── commands.rs         # Tauri IPC 命令处理
│   │   ├── scan.rs             # 核心扫描引擎 + 缓存流水线
│   │   ├── disk_cache.rs       # SQLite 持久化缓存
│   │   ├── binary_protocol.rs  # bincode 二进制序列化
│   │   ├── perf/mod.rs         # 性能监控
│   │   ├── fs/
│   │   │   ├── mod.rs          # 平台抽象层
│   │   │   ├── mft_scanner.rs  # NTFS $MFT 直接读取
│   │   │   ├── usn_journal.rs  # USN Journal 增量更新
│   │   │   ├── windows_walker.rs   # FindFirstFileExW 零额外 syscall 遍历
│   │   │   └── fallback_walker.rs  # 非 Windows 平台回退
│   │   └── bin/
│   │       └── cli.rs          # CLI 终端工具
│   │
│   └── wasm-sort/              # WASM 排序模块（GUI 卸载排序到 Rust）
│
└── README.md
```

### 技术栈

| 层级 | 技术 |
|------|------|
| 核心引擎 | Rust 2021 · Tokio 异步 · Rayon 并行 |
| 桌面 GUI | Tauri 2.0 · Vue 3 · Vite · Ant Design Vue · Chart.js |
| 文件系统 | NTFS $MFT · USN Journal · FindFirstFileExW · std::fs |
| 缓存 | DashMap + LRU（内存）· SQLite + bincode（磁盘） |
| 内存优化 | mimalloc 分配器 · SmartString 栈存储 · Arc 共享 |
| 排序卸载 | Web Worker（JS）· WASM（Rust → wasm-bindgen） |

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

## 缓存配置

```rust
// 内存缓存
const MAX_CACHE_ENTRIES: usize = 30;   // 最多缓存 30 个目录
const MAX_CACHE_SIZE_MB: usize = 200;  // 最大 200 MB

// 磁盘缓存
const MAX_DISK_CACHE_MB: usize = 500;  // 最大 500 MB
const CACHE_EXPIRE_DAYS: i64 = 7;      // 7 天过期

// USN 检查点
// 存储位置：~/.flashdir/usn_checkpoint_<盘符>.json
// 闲置超过 1 小时后过期
```

---

## License

MIT © [BlkSword](https://github.com/BlkSword)

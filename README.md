# FlashDir

一个基于 Rust 开发的极速系统占用查看工具，用于分析目录与文件的占用情况和可视化占用情况。

## 主要特点

- **便携版本**：单个 EXE 文件，双击即可运行，无需安装
- **极速扫描**：并行扫描 + LRU 缓存，10 万文件秒级响应
- **智能缓存**：基于目录修改时间自动失效，最多缓存 30 个目录（200MB）
- **持久化历史**：历史记录自动保存到本地文件，重启不丢失
- **可视化图表**：使用 Chart.js 展示文件类型分布和 Top 5 大文件
- **目录树导航**：支持文件夹层级浏览和前进返回功能
- **虚拟滚动**：大数据量列表流畅渲染

## 使用说明

### 快速上手

1. 双击 `FlashDir.exe` 启动应用
2. 点击「浏览」按钮选择要扫描的目录，或直接输入路径
3. 点击「扫描」按钮开始分析

### 开发模式运行

```bash
# 安装依赖
cd src-tauri/app && npm install

# 启动开发模式（热重载）
npm run dev
```

### 构建生产版本

```bash
# 构建前端资源
npm run build

# 构建安装包
npm run tauri:build
```

构建产物位于 `src-tauri/target/release/bundle/` 目录。

## 技术栈

| 层级 | 技术 |
|------|------|
| **后端** | Rust 2021 + Tauri 2.0 + Tokio + Rayon + DashMap + LRU |
| **前端** | Vue 3 + Vite + Ant Design Vue + Chart.js |
| **架构** | 桌面应用，通过 Tauri invoke API 通信 |

### 核心依赖

**后端 (Rust)**
- `tauri 2.0` - 跨平台桌面应用框架
- `tokio` - 异步运行时
- `rayon` - 数据并行库
- `dashmap` - 并发 HashMap
- `lru` - LRU 缓存实现
- `crossbeam` - 多线程通道
- `num_cpus` - CPU 核心数检测
- `mimalloc` - 高性能内存分配器
- `parking_lot` - 高性能锁
- `smartstring` - 紧凑字符串存储
- `ahash` - 高速哈希算法

**前端 (Vue 3)**
- `ant-design-vue` - UI 组件库
- `chart.js` - 图表可视化
- `vite` - 构建工具

## 项目结构

```
FlashDir/
├── src-tauri/              # Rust 后端代码
│   ├── app/                # Vue 3 前端应用
│   │   ├── src/
│   │   │   ├── App.vue              # 主组件
│   │   │   ├── components/          # UI 组件
│   │   │   │   ├── Toolbar.vue      # 工具栏
│   │   │   │   ├── Sidebar.vue      # 目录树
│   │   │   │   ├── FileList.vue     # 文件列表
│   │   │   │   ├── Charts.vue       # 统计图表
│   │   │   │   ├── StatusBar.vue    # 状态栏
│   │   │   │   └── HistoryList.vue  # 历史记录
│   │   │   ├── composables/
│   │   │   │   └── useTauri.js      # Tauri API 封装
│   │   │   └── main.js              # 应用入口
│   │   ├── package.json             # 前端依赖
│   │   └── vite.config.js           # Vite 配置
│   ├── src/                # Rust 源码
│   │   ├── main.rs         # Tauri 入口
│   │   ├── commands.rs     # 命令处理器
│   │   └── scan.rs         # 扫描核心逻辑
│   ├── Cargo.toml          # Rust 依赖配置
│   └── tauri.conf.json     # Tauri 配置
└── README.md               # 项目说明文档
```

## 功能特性

### 目录扫描

扫描指定目录，递归分析所有子目录和文件的大小，支持按名称、大小、类型排序显示，分页浏览（50/100/200/500/1000 条/页）。

### 智能缓存机制

首次扫描后，系统会自动缓存目录结构到内存（DashMap 并发安全缓存）。当用户再次扫描同一目录时：

- 若目录无变化（修改时间未变）：直接使用缓存，秒级响应
- 若目录有变化：重新扫描并更新缓存
- 缓存限制：最多 30 个目录，总大小不超过 200MB
- 缓存淘汰：基于 LRU 策略自动清理最旧的缓存

### 持久化历史记录

- 自动保存最近 20 条扫描记录到 `~/.flashdir/history.json`
- 应用重启后历史记录自动恢复
- 支持一键清空历史记录
- 打开历史面板时自动刷新最新状态

### 可视化图表

使用 Chart.js 绘制：

- **文件类型分布**：按大小显示各文件类型占比（环形图）
- **Top 5 大文件**：展示占用空间最大的 5 个文件/文件夹（柱状图）

### 目录树导航

左侧显示可展开的目录树，支持：

- 点击文件夹进入子目录
- 前进/返回/上级目录导航
- 高亮当前选中路径

## Tauri 命令 API

| 命令 | 参数 | 返回值 | 说明 |
|------|------|--------|------|
| `scan_directory` | `path`, `force_refresh` | `ScanResult` | 扫描目录 |
| `get_history` | - | `HistoryItem[]` | 获取历史记录 |
| `clear_history` | - | - | 清空历史记录 |

## 性能优化

### 性能指标

| 指标 | 优化后 |
|------|--------|
| 10 万文件扫描 | ~0.8s |
| 缓存命中响应 | <5ms |
| 搜索响应时间 | <10ms |
| 内存峰值 | ~85MB |

### 已实施的优化

| 优化项 | 实现方式 | 效果 |
|--------|----------|------|
| **mimalloc 分配器** | 全局高性能内存分配 | 提升 ~10% |
| **SmartString** | 短字符串栈存储 | 节省 ~15MB |
| **LRU 缓存** | `lru` crate 替代排序淘汰 | O(1) 缓存读写和淘汰 |
| **Arc 共享缓存** | 零成本共享 ScanResult | 减少内存复制 |
| **轻量级历史** | 不存储完整 items | 节省 ~50MB |
| **parking_lot 锁** | 高性能 Mutex | 更快锁操作 |
| **动态线程池** | `(cpu*2).min(32)` 线程数 | 高核 CPU 充分利用 |
| **Channel 队列** | `crossbeam::channel` | 高竞争下性能更好 |
| **并发累加** | DashMap 并发累加目录大小 | 替代 fold/reduce 模式 |
| **条件字符串替换** | 检测后再替换 | 避免不必要的分配 |
| **VecDeque 历史记录** | 双端队列 | O(1) 入队/出队 |
| **Web Worker 排序** | 后台线程排序 | UI 不阻塞 |
| **分片树构建** | requestIdleCallback | 渐进渲染 |
| **CSS contain** | 隔离重排重绘 | 渲染加速 |
| **虚拟滚动** | Ant Design 内置虚拟列表 | 大数据量流畅渲染 |
| **图表数据指纹** | 检测数据变化 | 避免重复计算统计 |

### 缓存策略

```rust
// 缓存配置
const MAX_CACHE_ENTRIES: usize = 30;    // 最多 30 个目录
const MAX_CACHE_SIZE_MB: usize = 200;   // 最多 200MB

// 缓存失效条件
1. 目录修改时间（mtime）变化时自动失效
2. 手动强制刷新（force_refresh = true）
3. LRU 自动淘汰最少使用的条目
```

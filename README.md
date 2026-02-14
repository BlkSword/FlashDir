# FlashDir

一个基于 Rust + Tauri 2.0 开发的极速磁盘空间分析工具，用于分析目录与文件的占用情况和可视化展示。

## 性能指标

| 指标 | 数值 |
|------|------|
| **100 万+文件扫描** | ~40s |
| **搜索响应** | 毫秒级 |
| **缓存命中** | <5ms |
| **内存占用** | ~65MB（优化后） |

## 主要特点

- **便携版本**：单个 EXE 文件，双击即可运行，无需安装
- **极速扫描**：并行扫描 + 多级缓存，100 万文件 40 秒完成
- **毫秒级搜索**：实时过滤，输入即搜索
- **智能缓存**：内存缓存 + SQLite 持久化，重启不丢失
- **可视化图表**：Chart.js 展示文件类型分布和 Top 大文件
- **目录树导航**：可展开的文件夹层级浏览
- **虚拟滚动**：大数据量列表流畅渲染

## 快速上手

### 使用方法

1. 双击 `FlashDir.exe` 启动应用
2. 点击「浏览」按钮选择目录，或直接输入路径
3. 点击「扫描」开始分析

### 开发模式

```bash
# 安装依赖
cd src-tauri/app && npm install

# 启动开发模式
npm run dev
```

### 构建生产版本

```bash
npm run build
npm run tauri:build
```

构建产物位于 `src-tauri/target/release/bundle/`。

## 技术栈

| 层级 | 技术 |
|------|------|
| **后端** | Rust 2021 + Tauri 2.0 + Tokio + Rayon + DashMap |
| **前端** | Vue 3 + Vite + Ant Design Vue + Chart.js |
| **存储** | SQLite 持久化缓存 + LRU 内存缓存 |

### 核心依赖

**后端 (Rust)**
- `tauri 2.0` - 跨平台桌面应用框架
- `tokio` - 异步运行时
- `rayon` - 数据并行库
- `dashmap` - 并发 HashMap
- `lru` - LRU 缓存
- `rusqlite` - SQLite 数据库
- `mimalloc` - 高性能内存分配器
- `parking_lot` - 高性能锁
- `smartstring` - 紧凑字符串

**前端 (Vue 3)**
- `ant-design-vue` - UI 组件库
- `chart.js` - 图表可视化
- `vite` - 构建工具

## 项目结构

```
FlashDir/
├── src-tauri/
│   ├── app/                  # Vue 3 前端
│   │   └── src/
│   │       ├── App.vue
│   │       ├── components/   # UI 组件
│   │       └── composables/  # 组合式函数
│   ├── src/                  # Rust 后端
│   │   ├── main.rs          # 入口
│   │   ├── commands.rs      # 命令处理
│   │   ├── scan.rs          # 扫描逻辑
│   │   ├── disk_cache.rs    # 磁盘缓存
│   │   └── binary_protocol.rs # 二进制序列化
│   ├── wasm-sort/           # WASM 排序模块
│   └── tauri.conf.json
└── README.md
```

## 功能特性

### 目录扫描

递归分析目录结构，支持按名称、大小、类型排序，分页浏览（50/100/200/500/1000 条/页）。

### 多级缓存

- **内存缓存**：DashMap 并发安全，LRU 淘汰策略
- **磁盘缓存**：SQLite 持久化，应用重启后缓存保留
- **自动失效**：基于目录修改时间自动检测

### 可视化图表

- 文件类型分布（环形图）
- Top 5 大文件（柱状图）

### 目录树导航

- 可展开的文件夹层级
- 前进/返回/上级目录导航
- 高亮当前路径

## 性能优化

| 优化项 | 方式 | 效果 |
|--------|------|------|
| mimalloc | 全局内存分配器 | ~10% 提升 |
| SmartString | 短字符串栈存储 | 节省内存 |
| Rayon 并行 | 多线程扫描 | 充分利用多核 |
| DashMap | 并发累加目录大小 | 无锁竞争 |
| SQLite 缓存 | 持久化存储 | 重启不丢失 |
| Web Worker | 后台排序 | UI 不阻塞 |
| 虚拟滚动 | Ant Design 虚拟列表 | 大数据流畅 |

## 缓存配置

```rust
// 内存缓存
const MAX_CACHE_ENTRIES: usize = 30;  // 最多 30 个目录
const MAX_CACHE_SIZE_MB: usize = 200; // 最多 200MB

// 磁盘缓存
const MAX_DISK_CACHE_MB: usize = 500; // 最多 500MB
const CACHE_EXPIRE_DAYS: i64 = 7;     // 7 天过期
```

## License

MIT

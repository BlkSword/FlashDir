// FlashDir 核心库 —— 供 Tauri GUI 和 CLI 两个二进制共享
//
// 包含：
// - scan: 扫描引擎（MFT / 目录遍历 / 缓存 / 流式传输）
// - perf: 性能监控
// - disk_cache: SQLite 磁盘缓存
// - binary_protocol: bincode 二进制序列化
// - fs: 平台文件系统抽象（Windows 快速遍历器 / MFT 读取 / USN Journal）

pub mod scan;
pub mod perf;
pub mod disk_cache;
pub mod binary_protocol;
pub mod fs;
pub mod dev_analyzer;
pub mod diff_engine;
pub mod global_search;

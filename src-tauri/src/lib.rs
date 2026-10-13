// FlashDir 核心库 —— 供 Tauri GUI 和 CLI 两个二进制共享
//
// 包含：
// - scan: 扫描引擎（MFT / 目录遍历 / 缓存 / 流式传输）
// - perf: 性能监控
// - disk_cache: SQLite 磁盘缓存
// - fs: 平台文件系统抽象（Windows 快速遍历器 / MFT 读取 / USN Journal）

pub mod diag;

pub mod cancel;
pub mod scan;
pub mod volumes;
/// MCP（Model Context Protocol）服务器核心：stdio / 本机端点 / 桥接
#[cfg(feature = "mcp")]
pub mod mcp;
pub mod perf;
pub mod disk_cache;
pub mod fs;
pub mod dev_analyzer;
pub mod diff_engine;
pub mod global_search;
// FlashDir 的核心能力（$MFT 直读、USN Journal、全盘索引）全部建立在 NTFS/Win32 之上：
// fs 模块把 mft_scanner / usn_journal 整体按 `#[cfg(target_os = "windows")]` 门控，而
// scan.rs 与 global_search.rs 会无条件引用这些 Windows 专有符号（is_admin、
// check_mft_available、get_checkpoint、try_mft_scan_with_cancel、MftScanner、
// UsnCheckpoint、read_incremental_changes 等）。
//
// 因此这里显式声明平台边界：非 Windows 目标给出**一条清晰的错误**，而不是抛出一堆
// "cannot find function `is_admin` / `get_checkpoint`" 之类让人误以为是 bug 的报错。
// 这同时意味着 fs/fallback_walker.rs 与 scan.rs 里 #[cfg(not(target_os = "windows"))]
// 的分支在当前结构下无法参与编译（它们是"未来跨平台"的脚手架，不是可用回退路径）。
// 若要真正支持非 Windows，需要先把 USN/MFT 相关类型与函数从 Windows 专有模块中拆出、
// 并在 global_search.rs 的 USN 同步链路上补齐空实现 —— 那是一次独立的重构。
#[cfg(not(target_os = "windows"))]
compile_error!(
    "FlashDir 仅支持 Windows（x64）：MFT 直读与 USN 增量依赖 Win32 / NTFS。\
     详见 src/lib.rs 顶部关于平台边界的说明。"
);

pub mod duplicate_finder;

/// 缺陷修复回归测试集（仅在测试构建中编译）
#[cfg(test)]
mod tests_regression;

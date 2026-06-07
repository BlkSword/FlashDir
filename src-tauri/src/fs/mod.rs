// 文件系统操作模块
// 提供平台特定的快速目录遍历能力

#[cfg(target_os = "windows")]
mod windows_walker;
#[cfg(target_os = "windows")]
pub use windows_walker::*;

#[cfg(target_os = "windows")]
mod mft_scanner;
#[cfg(target_os = "windows")]
pub use mft_scanner::*;

#[cfg(target_os = "windows")]
mod usn_journal;
#[cfg(target_os = "windows")]
pub use usn_journal::*;

#[cfg(not(target_os = "windows"))]
mod fallback_walker;
#[cfg(not(target_os = "windows"))]
pub use fallback_walker::*;

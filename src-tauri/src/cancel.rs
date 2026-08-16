/// 全局扫描取消标记
///
/// FlashDir 目前是单扫描窗口模型，使用一个进程级 AtomicBool 即可。
/// 每次 `scan_directory` 开始时重置，取消时由前端命令置位。

use std::sync::atomic::{AtomicBool, Ordering};

static CANCEL_SCAN: AtomicBool = AtomicBool::new(false);

/// 请求取消当前扫描
pub fn request() {
    CANCEL_SCAN.store(true, Ordering::Relaxed);
}

/// 重置取消标记（每次扫描开始时调用）
pub fn reset() {
    CANCEL_SCAN.store(false, Ordering::Relaxed);
}

/// 当前是否已请求取消
pub fn is_requested() -> bool {
    CANCEL_SCAN.load(Ordering::Relaxed)
}

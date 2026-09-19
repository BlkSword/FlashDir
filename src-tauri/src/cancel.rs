// 全局扫描取消标记
//
// FlashDir 有多个入口可能并发触发扫描（主界面扫描、分页加载、目录树展开、
// 全局索引构建、USN 检查等）。早期实现使用单个 AtomicBool，并且每次
// `scan_directory` 开始时都 `reset()` —— 新扫描会把正在运行的长扫描的取消
// 请求悄悄清掉，导致"取消"按钮在并发场景下失效。
//
// 现改为"代（generation）"模型：
// - 每次扫描开始时领取一个单调递增的 scan_id；
// - 取消请求记录"取消到哪一代为止"（CANCELLED_UPTO）；
// - 扫描自身（包括 MFT 记录读取、目录遍历线程）只需判断
//   `is_requested_for(自己的 id)`。
//
// 语义：
// - 新扫描不会清除已有扫描的取消状态（旧扫描若已被取消，仍会尽快退出）；
// - 一次取消会停掉所有"在飞"的扫描（符合 UI 上"取消当前操作"的直觉）；
// - 取消之后新发起的扫描不受影响。

use std::sync::atomic::{AtomicU64, Ordering};

static GENERATION: AtomicU64 = AtomicU64::new(0);
static CANCELLED_UPTO: AtomicU64 = AtomicU64::new(0);

/// 开始一次扫描，返回该扫描的 id（从 1 开始单调递增）
pub fn begin() -> u64 {
    GENERATION.fetch_add(1, Ordering::SeqCst) + 1
}

/// 请求取消：取消所有已开始（id <= 当前最新 id）的扫描。
pub fn request() {
    let latest = GENERATION.load(Ordering::SeqCst);
    CANCELLED_UPTO.store(latest, Ordering::SeqCst);
}

/// 判断指定扫描是否已被取消
pub fn is_requested_for(scan_id: u64) -> bool {
    scan_id != 0 && scan_id <= CANCELLED_UPTO.load(Ordering::SeqCst)
}

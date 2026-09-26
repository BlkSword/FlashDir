//! 诊断与崩溃日志。
//!
//! release 构建使用 `panic = "abort"`：panic 与分配失败都会让进程立刻消失，
//! 桌面上没有任何提示（用户感知就是"闪退"）。这里做三件事：
//!
//! 1. panic hook：把 panic 内容、最近一次重活（breadcrumb）与回溯写进日志；
//! 2. 分配失败记录：分配器包装（见 main.rs）在拿到空指针时，先写一条
//!    不允许分配的日志，再交给标准库终止流程；
//! 3. 内存水位查询与"重活前守卫"：尽量不走到分配失败那一步。
//!
//! 日志位置：`~/.flashdir/diag.log`（环境变量 `FLASHDIR_DIAG_LOG` 可重定向，
//! 便于自测与支持时收集），单个文件超过 1MB 在下次启动时重建，不会长期占盘。

use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::sync::{Mutex, OnceLock};

/// 日志文件上限；超过后在下次打开时重建（崩溃日志只关心最近一次问题）。
const MAX_LOG_BYTES: u64 = 1024 * 1024;
/// 保留最近多少次重活记录，写进 panic 日志用于定位。
const MAX_BREADCRUMBS: usize = 24;
/// 分配失败日志的固定前缀（无分配路径只写这块静态内容）。
const OOM_PREFIX: &[u8] = b"ALLOC-FAIL bytes=";

static LOG_PATH: OnceLock<PathBuf> = OnceLock::new();
static LOG_FILE: OnceLock<Mutex<Option<std::fs::File>>> = OnceLock::new();
static LOG_HANDLE: AtomicIsize = AtomicIsize::new(0);
static BREADCRUMBS: Mutex<Vec<String>> = Mutex::new(Vec::new());
static INSTALLED: AtomicBool = AtomicBool::new(false);

fn home_dir() -> PathBuf {
    std::env::var_os("USERPROFILE")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("."))
}

/// 日志文件路径（`FLASHDIR_DIAG_LOG` 优先，便于自测重定向）。
pub fn log_path() -> PathBuf {
    LOG_PATH
        .get_or_init(|| {
            if let Ok(custom) = std::env::var("FLASHDIR_DIAG_LOG") {
                if !custom.trim().is_empty() {
                    return PathBuf::from(custom);
                }
            }
            let mut path = home_dir();
            path.push(".flashdir");
            path.push("diag.log");
            path
        })
        .clone()
}

fn open_log() -> Option<std::fs::File> {
    let path = log_path();
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            let _ = std::fs::create_dir_all(parent);
        }
    }
    // 超限则重建：先删后建，保证单文件不会无限增长
    if let Ok(meta) = std::fs::metadata(&path) {
        if meta.len() > MAX_LOG_BYTES {
            let _ = std::fs::remove_file(&path);
        }
    }
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .ok()
}

fn slot() -> &'static Mutex<Option<std::fs::File>> {
    LOG_FILE.get_or_init(|| {
        let file = open_log();
        #[cfg(target_os = "windows")]
        {
            use std::os::windows::io::AsRawHandle;
            if let Some(f) = file.as_ref() {
                LOG_HANDLE.store(f.as_raw_handle() as isize, Ordering::Relaxed);
            }
        }
        Mutex::new(file)
    })
}

/// 追加一行日志（同时输出到 stderr，方便命令行运行时直接看到）。
pub fn log_line(msg: &str) {
    let slot = slot();
    if let Ok(mut guard) = slot.lock() {
        if guard.is_none() {
            *guard = open_log();
        }
        if let Some(file) = guard.as_mut() {
            let ts = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
            let _ = writeln!(file, "[{ts}] {msg}");
            let _ = file.flush();
        }
    }
    eprintln!("[diag] {msg}");
}

/// 记录一次重活（扫描 / 索引构建 / 快照等）。
///
/// 立即落盘：崩溃发生时（尤其是分配失败）没有机会再补充上下文，
/// 所以每条记录既进内存环形缓冲，也直接写文件。
pub fn breadcrumb(op: &str) {
    if let Ok(mut ring) = BREADCRUMBS.lock() {
        if ring.len() >= MAX_BREADCRUMBS {
            ring.remove(0);
        }
        ring.push(op.to_string());
    }
    log_line(op);
}

/// 最近的重活记录（单行拼接）。
pub fn recent_ops() -> String {
    match BREADCRUMBS.lock() {
        Ok(ring) => ring.join(" | "),
        Err(_) => String::new(),
    }
}

/// 安装 panic hook（幂等）。必须在做任何重活之前调用。
pub fn install_crash_handler() {
    if INSTALLED.swap(true, Ordering::SeqCst) {
        return;
    }
    log_line(&format!(
        "启动 FlashDir {}（pid {}，日志 {}）",
        env!("CARGO_PKG_VERSION"),
        std::process::id(),
        log_path().display()
    ));
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let backtrace = std::backtrace::Backtrace::force_capture();
        let name = std::thread::current()
            .name()
            .map(|s| s.to_string())
            .unwrap_or_else(|| String::from("<unnamed>"));
        log_line(&format!("[PANIC] 线程 {name}: {info}"));
        log_line(&format!("[PANIC] 最近操作: {}", recent_ops()));
        log_line(&format!("[PANIC] 回溯: {backtrace}"));
        default_hook(info);
    }));
}

/// 分配失败：此刻禁止任何分配（很可能正处在分配器内部），
/// 只用静态缓冲 + 原始句柄写一条记录，然后交给标准库终止进程。
pub fn alloc_failed(layout: std::alloc::Layout) -> ! {
    write_oom_line(layout.size());
    std::alloc::handle_alloc_error(layout)
}

/// 把分配失败信息写进固定缓冲（无分配），返回写入长度。
fn oom_line(buf: &mut [u8; 64], bytes: u64) -> usize {
    let mut len = 0usize;
    for byte in OOM_PREFIX {
        buf[len] = *byte;
        len += 1;
    }
    // 手工十进制转换，避免格式化（可能分配）
    let mut digits = [0u8; 20];
    let mut used = 0usize;
    let mut value = bytes as u64;
    loop {
        digits[used] = b'0' + (value % 10) as u8;
        value /= 10;
        used += 1;
        if value == 0 || used == digits.len() {
            break;
        }
    }
    while used > 0 {
        used -= 1;
        buf[len] = digits[used];
        len += 1;
    }
    buf[len] = b'\n';
    len += 1;

    len
}

fn write_oom_line(bytes: usize) {
    let handle = LOG_HANDLE.load(Ordering::Relaxed);
    if handle == 0 {
        return;
    }
    let mut buf = [b' '; 64];
    let len = oom_line(&mut buf, bytes as u64);
    #[cfg(target_os = "windows")]
    unsafe {
        use windows_sys::Win32::Storage::FileSystem::WriteFile;
        let mut written: u32 = 0;
        WriteFile(
            handle,
            buf.as_ptr(),
            len as u32,
            &mut written,
            std::ptr::null_mut(),
        );
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = (buf, len);
    }
}

/// 可用物理内存（MB）。查询失败返回 None。
#[cfg(target_os = "windows")]
pub fn available_physical_mb() -> Option<u64> {
    use windows_sys::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
    unsafe {
        let mut status: MEMORYSTATUSEX = std::mem::zeroed();
        status.dwLength = std::mem::size_of::<MEMORYSTATUSEX>() as u32;
        if GlobalMemoryStatusEx(&mut status) == 0 {
            return None;
        }
        Some(status.ullAvailPhys / (1024 * 1024))
    }
}

#[cfg(not(target_os = "windows"))]
pub fn available_physical_mb() -> Option<u64> {
    None
}

/// 可用内存的可读文本（日志用）。
pub fn memory_text() -> String {
    match available_physical_mb() {
        Some(mb) => format!("{mb} MB"),
        None => String::from("未知"),
    }
}

/// 重活前的内存守卫：低于阈值就返回 Err（由界面提示），
/// 而不是让分配失败把进程直接干掉。
pub fn ensure_memory_available(need_mb: u64, what: &str) -> Result<u64, String> {
    match available_physical_mb() {
        Some(avail) if avail < need_mb => {
            let msg = format!(
                "可用内存不足：当前约 {avail} MB，{what}至少需要 {need_mb} MB。请关闭部分程序后重试。"
            );
            log_line(&format!("[GUARD] {msg}"));
            Err(msg)
        }
        Some(avail) => Ok(avail),
        None => Ok(u64::MAX),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 分配失败日志必须在"禁止分配"的前提下格式化出来（含边界值）。
    #[test]
    fn oom_line_formats_without_allocation() {
        let mut buf = [b' '; 64];
        let n = oom_line(&mut buf, 59097);
        let text = std::str::from_utf8(&buf[..n]).unwrap();
        assert!(text.starts_with("ALLOC-FAIL bytes=59097"), "实际: {text}");
        assert_eq!(buf[n - 1], 10, "必须以换行结尾");

        for bytes in [1u64, 9, 10, 99, 100, u64::MAX / 2] {
            let mut buf = [b' '; 64];
            let n = oom_line(&mut buf, bytes);
            let text = std::str::from_utf8(&buf[..n]).unwrap();
            assert!(text.starts_with("ALLOC-FAIL bytes="));
            assert_eq!(text.trim_end().len(), "ALLOC-FAIL bytes=".len() + bytes.to_string().len());
            assert_eq!(buf[n - 1], 10);
        }
    }

    /// 内存守卫：要求超过物理内存时必须拒绝（而不是继续跑然后闪退）。
    #[test]
    fn memory_guard_rejects_impossible_requirement() {
        let err = ensure_memory_available(u64::MAX / 1024 / 1024, "自测").unwrap_err();
        assert!(err.contains("可用内存不足"), "实际: {err}");
    }

    /// 内存守卫：0 阈值必须放行（水位低的机器不应被误拦）。
    #[test]
    fn memory_guard_allows_zero_requirement() {
        assert!(ensure_memory_available(0, "自测").is_ok());
    }

    /// 日志路径可解析（默认在用户目录下，也可用环境变量重定向）。
    #[test]
    fn log_path_is_resolvable() {
        let path = log_path();
        assert!(!path.as_os_str().is_empty());
    }
}
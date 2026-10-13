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
//! 便于自测与支持时收集），单个文件超过 1MB 会重建：启动时检查一次，
//! 长会话内再由 `log_line` 按累积字节数触发轮转（不做每行 stat），不会无限增长。

use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

/// 日志文件上限；超过后重建（崩溃日志只关心最近一次问题）。
const MAX_LOG_BYTES: u64 = 1024 * 1024;
/// 保留最近多少次重活记录，写进 panic 日志用于定位。
const MAX_BREADCRUMBS: usize = 24;
/// 分配失败日志的固定前缀（无分配路径只写这块静态内容）。
const OOM_PREFIX: &[u8] = b"ALLOC-FAIL bytes=";
/// 分配失败行的栈缓冲长度：时间戳(22) + 前缀(17) + 20 位十进制 + 换行，留足余量。
const OOM_BUF_LEN: usize = 96;
/// 估算每行日志的固定开销："[YYYY-MM-DD HH:MM:SS] " + 换行 ≈ 23 字节。
/// 只用于轮转阈值的粗略累计（宁可多算一点），不追求精确字节数。
const LOG_LINE_OVERHEAD: u64 = 24;

static LOG_PATH: OnceLock<PathBuf> = OnceLock::new();
static LOG_FILE: OnceLock<Mutex<Option<std::fs::File>>> = OnceLock::new();
static LOG_HANDLE: AtomicIsize = AtomicIsize::new(0);
/// 本次进程已写入日志的累计字节数（长会话轮转用，避免每行 stat）。
static LOG_BYTES_WRITTEN: AtomicU64 = AtomicU64::new(0);
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

/// 打开日志文件；`force_fresh` 为 true 时无条件截断重建（会话内轮转用）。
///
/// 同时把"已写字节数"基线与 OOM 用的原始句柄同步好：长会话里 `log_line`
/// 只靠这个计数判断是否需要轮转，不再对文件做 stat。
fn open_log_inner(force_fresh: bool) -> Option<std::fs::File> {
    let path = log_path();
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            let _ = std::fs::create_dir_all(parent);
        }
    }
    // 超限则重建：先删后建，保证单文件不会无限增长
    let existing = std::fs::metadata(&path).map(|meta| meta.len()).unwrap_or(0);
    let reset = force_fresh || existing > MAX_LOG_BYTES;
    if reset {
        let _ = std::fs::remove_file(&path);
    }
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .ok();

    // 轮转基线：进程启动时按已有文件长度起步，之后由 log_line 累加本次写入量
    LOG_BYTES_WRITTEN.store(if reset { 0 } else { existing }, Ordering::Relaxed);
    // OOM 路径只能拿原始句柄写文件，句柄要跟着轮转一起更新
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::io::AsRawHandle;
        LOG_HANDLE.store(
            file.as_ref().map_or(0, |f| f.as_raw_handle() as isize),
            Ordering::Relaxed,
        );
    }
    file
}

fn open_log() -> Option<std::fs::File> {
    open_log_inner(false)
}

fn slot() -> &'static Mutex<Option<std::fs::File>> {
    // 打开失败时的降级、句柄登记都在 open_log_inner 里统一处理
    LOG_FILE.get_or_init(|| Mutex::new(open_log()))
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

            // 长会话轮转：旧实现只在打开时检查一次 1MB，进程活多久日志就能涨多久。
            // 这里按累积写入量做一次廉价判断（不 stat），超限立刻截断重建。
            let line_bytes = msg.len() as u64 + LOG_LINE_OVERHEAD;
            let written = LOG_BYTES_WRITTEN.fetch_add(line_bytes, Ordering::Relaxed) + line_bytes;
            if written > MAX_LOG_BYTES {
                *guard = open_log_inner(true);
            }
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

/// 手工写入十进制（无分配）。`width` 为最小宽度，不足时左侧补 '0'。
/// 返回 false 表示缓冲不足（正常调用不会发生，纯属防御）。
fn write_decimal(buf: &mut [u8], len: &mut usize, mut value: u64, width: usize) -> bool {
    let mut digits = [0u8; 20];
    let mut used = 0usize;
    loop {
        digits[used] = b'0' + (value % 10) as u8;
        value /= 10;
        used += 1;
        if value == 0 || used == digits.len() {
            break;
        }
    }
    while used < width && used < digits.len() {
        digits[used] = b'0';
        used += 1;
    }
    while used > 0 {
        used -= 1;
        if *len >= buf.len() {
            return false;
        }
        buf[*len] = digits[used];
        *len += 1;
    }
    true
}

/// 把字段拼成固定宽度 "YYYY-MM-DD HH:MM:SS"（19 字节，无分配）。
fn timestamp_fields(out: &mut [u8; 19], parts: [u64; 6]) -> bool {
    let [y, mo, d, h, mi, s] = parts;
    let mut n = 0usize;
    write_decimal(out, &mut n, y, 4)
        && {
            out[n] = b'-';
            n += 1;
            write_decimal(out, &mut n, mo, 2)
        }
        && {
            out[n] = b'-';
            n += 1;
            write_decimal(out, &mut n, d, 2)
        }
        && {
            out[n] = b' ';
            n += 1;
            write_decimal(out, &mut n, h, 2)
        }
        && {
            out[n] = b':';
            n += 1;
            write_decimal(out, &mut n, mi, 2)
        }
        && {
            out[n] = b':';
            n += 1;
            write_decimal(out, &mut n, s, 2)
        }
        && n == out.len()
}

/// 取本地时间并格式化成 19 字节时间戳。
///
/// 分配失败路径禁止任何堆分配：这里只用 `GetLocalTime`（结果写进栈上结构体），
/// 不走 `chrono::Local::now()`（它可能为时区数据分配）。
fn timestamp_bytes(out: &mut [u8; 19]) -> bool {
    #[cfg(target_os = "windows")]
    let parts: Option<[u64; 6]> = unsafe {
        use windows_sys::Win32::Foundation::SYSTEMTIME;
        use windows_sys::Win32::System::SystemInformation::GetLocalTime;
        let mut st: SYSTEMTIME = std::mem::zeroed();
        GetLocalTime(&mut st);
        Some([
            st.wYear as u64,
            st.wMonth as u64,
            st.wDay as u64,
            st.wHour as u64,
            st.wMinute as u64,
            st.wSecond as u64,
        ])
    };
    #[cfg(not(target_os = "windows"))]
    let parts: Option<[u64; 6]> = None;

    match parts {
        Some(parts) => timestamp_fields(out, parts),
        None => false,
    }
}

/// 把分配失败信息写进固定缓冲（无分配），返回写入长度。
/// 旧实现没有时间戳：进程闪退后无法把这条记录对齐到具体时刻。
fn oom_line(buf: &mut [u8; OOM_BUF_LEN], bytes: u64) -> usize {
    let mut len = 0usize;

    // 时间戳：[YYYY-MM-DD HH:MM:SS] ，与 log_line 的行首格式一致
    let mut ts = [b'0'; 19];
    if timestamp_bytes(&mut ts) {
        buf[len] = b'[';
        len += 1;
        buf[len..len + ts.len()].copy_from_slice(&ts);
        len += ts.len();
        buf[len] = b']';
        len += 1;
        buf[len] = b' ';
        len += 1;
    }

    for byte in OOM_PREFIX {
        buf[len] = *byte;
        len += 1;
    }
    // 手工十进制转换，避免格式化（可能分配）
    let _ = write_decimal(buf, &mut len, bytes, 1);
    buf[len] = b'\n';
    len += 1;

    len
}

fn write_oom_line(bytes: usize) {
    let handle = LOG_HANDLE.load(Ordering::Relaxed);
    if handle == 0 {
        return;
    }
    let mut buf = [b' '; OOM_BUF_LEN];
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

    /// 分配失败日志必须在"禁止分配"的前提下格式化出来（含边界值），
    /// 并且必须带时间戳（旧实现没有，事后无法对齐崩溃时刻）。
    #[test]
    fn oom_line_formats_without_allocation() {
        let mut buf = [b' '; OOM_BUF_LEN];
        let n = oom_line(&mut buf, 59097);
        assert!(n <= OOM_BUF_LEN);
        let text = std::str::from_utf8(&buf[..n]).unwrap();
        assert!(text.starts_with('['), "OOM 行必须以时间戳开头，实际: {text}");
        assert!(
            text.contains("ALLOC-FAIL bytes=59097"),
            "实际: {text}"
        );
        assert_eq!(buf[n - 1], 10, "必须以换行结尾");

        for bytes in [0u64, 1, 9, 10, 99, 100, u64::MAX / 2, u64::MAX] {
            let mut buf = [b' '; OOM_BUF_LEN];
            let n = oom_line(&mut buf, bytes);
            assert!(n <= OOM_BUF_LEN);
            let text = std::str::from_utf8(&buf[..n]).unwrap();
            assert!(
                text.contains(&format!("ALLOC-FAIL bytes={bytes}")),
                "实际: {text}"
            );
            assert_eq!(buf[n - 1], 10);
        }
    }

    /// 时间戳拼接必须固定宽度且无分配（用固定字段自测，与系统时区无关）。
    #[test]
    fn timestamp_is_fixed_width() {
        let mut out = [0u8; 19];
        assert!(timestamp_fields(&mut out, [2025, 1, 2, 3, 4, 5]));
        assert_eq!(
            std::str::from_utf8(&out).unwrap(),
            "2025-01-02 03:04:05"
        );

        // 年份/月日补零；十进制不越界
        let mut out = [0u8; 19];
        assert!(timestamp_fields(&mut out, [999, 12, 31, 23, 59, 59]));
        assert_eq!(
            std::str::from_utf8(&out).unwrap(),
            "0999-12-31 23:59:59"
        );

        // 取本地时间也要能写满 19 字节（Windows 上永远成功；其他平台允许失败）
        let mut now = [0u8; 19];
        if timestamp_bytes(&mut now) {
            assert!(std::str::from_utf8(&now).unwrap().contains('-'));
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

    /// 长会话轮转（手动运行，需要独占进程级 `FLASHDIR_DIAG_LOG`）：
    /// `cargo test --lib -- --ignored --exact diag::tests::rotation_truncates_in_long_session`
    ///
    /// 旧实现只在打开时检查一次 1MB，长会话里日志可以无限增长；这里写够
    /// 三倍上限，验证文件在会话内被轮转截断。
    #[test]
    #[ignore = "需要独占 FLASHDIR_DIAG_LOG 环境变量，手动运行"]
    fn rotation_truncates_in_long_session() {
        if LOG_PATH.get().is_some() {
            eprintln!("[skip] 日志路径已被初始化，请单独运行本测试（--exact）");
            return;
        }
        let dir = std::env::temp_dir().join(format!("flashdir-diag-rotation-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("diag.log");
        std::env::set_var("FLASHDIR_DIAG_LOG", &path);

        let filler = "x".repeat(4096);
        let lines = 3 * (MAX_LOG_BYTES / 4096) as usize;
        for i in 0..lines {
            log_line(&format!("轮转自测 {i} {filler}"));
        }

        let len = std::fs::metadata(&path).unwrap().len();
        assert!(
            len <= MAX_LOG_BYTES + 8192,
            "长会话日志必须被轮转截断，实际 {len} 字节（上限 {MAX_LOG_BYTES}）"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
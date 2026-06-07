// Windows 快速目录遍历器
// 直接使用 FindFirstFileExW / FindNextFileW，从 WIN32_FIND_DATAW 中一次性获取
// 文件名、大小、是否为目录 —— 无需额外的 metadata() / file_type() 系统调用
//
// 对比 Rust 标准库 fs::read_dir：
//   - fs::read_dir 内部调用 FindFirstFileExW，但不暴露 WIN32_FIND_DATAW 中的 size
//   - 需要额外 entry.metadata() 才能拿到文件大小（每次都是一个 CreateFile + GetFileSize 系统调用）
//   - 遍历 100 万文件 = 100 万次多余的 syscall
//
// 本模块将 FindFirstFileExW 返回的所有信息一次性提取，消除冗余系统调用。

use std::io;
use std::path::{Path, PathBuf};
use std::ffi::OsString;
use std::os::windows::ffi::{OsStrExt, OsStringExt};

use windows_sys::Win32::Foundation::{GetLastError, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Storage::FileSystem::{
    FindFirstFileExW, FindNextFileW, FindClose,
    FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT,
    FIND_FIRST_EX_CASE_SENSITIVE, FIND_FIRST_EX_LARGE_FETCH,
    WIN32_FIND_DATAW,
};

/// 快速目录条目 —— 一次 FindFirstFileExW 调用获取全部信息
#[derive(Debug, Clone)]
pub struct FastDirEntry {
    /// 条目完整路径
    pub path: PathBuf,
    /// 文件名（不含路径）
    pub name: String,
    /// 文件大小（字节），目录为 0
    pub size: u64,
    /// 是否为目录
    pub is_dir: bool,
    /// 是否为符号链接 / 重解析点
    pub is_symlink: bool,
}

/// 使用 Windows 原生 API 快速遍历目录
///
/// 与 fs::read_dir 的区别：
/// - 使用 FindExInfoBasic：只返回基本信息（不包含短文件名），减少 I/O
/// - 使用 FIND_FIRST_EX_LARGE_FETCH：批量预取，减少内核往返
/// - 从 WIN32_FIND_DATAW 直接读取 size 和 attributes，零额外 syscall
pub fn read_dir_entries(dir_path: &Path) -> io::Result<Vec<FastDirEntry>> {
    // 构建搜索模式：<dir>\* 的 UTF-16 宽字符路径
    let search_pattern: Vec<u16> = dir_path
        .join("*")
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();

    unsafe {
        let mut find_data: WIN32_FIND_DATAW = std::mem::zeroed();

        // FindExInfoBasic = 1: 只获取基本信息（不包含 8.3 短名）
        const FIND_EX_INFO_BASIC: i32 = 1;
        // FindExSearchNameMatch = 1
        const FIND_EX_SEARCH_NAME_MATCH: i32 = 1;

        let handle = FindFirstFileExW(
            search_pattern.as_ptr(),
            FIND_EX_INFO_BASIC,
            &mut find_data as *mut _ as *mut _,
            FIND_EX_SEARCH_NAME_MATCH,
            std::ptr::null(),
            FIND_FIRST_EX_LARGE_FETCH | FIND_FIRST_EX_CASE_SENSITIVE,
        );

        if handle == INVALID_HANDLE_VALUE {
            let err = GetLastError();
            // ERROR_FILE_NOT_FOUND (2) / ERROR_PATH_NOT_FOUND (3) → 空目录
            if err == 2 || err == 3 {
                return Ok(Vec::new());
            }
            return Err(io::Error::from_raw_os_error(err as i32));
        }

        // 预估容量：多数目录条目数在 ~100 以内
        let mut entries = Vec::with_capacity(128);

        loop {
            let name = win32_find_data_to_name(&find_data);

            // 跳过 "." 和 ".."
            if name != "." && name != ".." {
                let is_dir = (find_data.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY) != 0;
                let is_symlink = (find_data.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT) != 0;

                let size = if is_dir {
                    0
                } else {
                    ((find_data.nFileSizeHigh as u64) << 32) | (find_data.nFileSizeLow as u64)
                };

                let full_path = dir_path.join(&name);

                entries.push(FastDirEntry {
                    path: full_path,
                    name,
                    size,
                    is_dir,
                    is_symlink,
                });
            }

            if FindNextFileW(handle, &mut find_data) == 0 {
                let err = GetLastError();
                if err == 18 {
                    // ERROR_NO_MORE_FILES — 正常结束
                    break;
                }
                // 其他错误视为部分读取成功
                break;
            }
        }

        FindClose(handle);
        Ok(entries)
    }
}

/// 从 WIN32_FIND_DATAW 提取文件名
unsafe fn win32_find_data_to_name(find_data: &WIN32_FIND_DATAW) -> String {
    let name_len = find_data.cFileName
        .iter()
        .position(|&c| c == 0)
        .unwrap_or(260);
    OsString::from_wide(&find_data.cFileName[..name_len])
        .to_string_lossy()
        .into_owned()
}

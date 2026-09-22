// 非 Windows 平台的目录遍历回退方案
// 使用标准库 fs::read_dir（在 Linux/macOS 上也已足够高效，
// getdents64 系统调用本身就会返回 d_type）
//
// 说明：FlashDir 当前是 Windows 优先项目（MFT / USN / FindFirstFileExW 均为
// Windows 专有）。本模块只覆盖"目录遍历"这一层回退，字段必须与
// windows_walker::FastDirEntry 保持一致（此前 struct 漏了 mtime 字段，
// 而调用处已经传了 mtime，非 Windows 下无法编译）。

use std::io;
use std::path::{Path, PathBuf};

/// 快速目录条目
#[derive(Debug, Clone)]
pub struct FastDirEntry {
    pub path: PathBuf,
    pub name: String,
    pub size: u64,
    pub is_dir: bool,
    pub is_symlink: bool,
    /// 修改时间（Unix 秒级时间戳，0 = 未知），与 Windows 遍历器保持一致
    pub mtime: i64,
    /// 访问时间（Unix 秒级时间戳）
    pub atime: i64,
}

/// 使用标准库遍历目录（非 Windows 平台）
pub fn read_dir_entries(dir_path: &Path) -> io::Result<Vec<FastDirEntry>> {
    let dir_iter = match std::fs::read_dir(dir_path) {
        Ok(iter) => iter,
        Err(e) => return Err(e),
    };

    let mut entries = Vec::with_capacity(128);

    for entry in dir_iter.filter_map(|e| e.ok()) {
        let entry_path = entry.path();

        let file_type = match entry.file_type() {
            Ok(ft) => ft,
            Err(_) => continue,
        };

        let is_dir = file_type.is_dir();
        let is_symlink = file_type.is_symlink();

        if is_symlink {
            continue;
        }

        let name = entry_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("?")
            .to_string();

        let (size, mtime, atime) = if is_dir {
            (0, 0, 0)
        } else {
            match entry.metadata() {
                Ok(m) => (
                    m.len(),
                    m.modified()
                        .ok()
                        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                        .map(|d| d.as_secs() as i64)
                        .unwrap_or(0),
                    m.accessed()
                        .ok()
                        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                        .map(|d| d.as_secs() as i64)
                        .unwrap_or(0),
                ),
                Err(_) => (0, 0, 0),
            }
        };

        entries.push(FastDirEntry {
            path: entry_path,
            name,
            size,
            is_dir,
            is_symlink,
            mtime,
            atime,
        });
    }

    Ok(entries)
}

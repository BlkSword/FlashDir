// 非 Windows 平台的目录遍历回退方案
// 使用标准库 fs::read_dir（在 Linux/macOS 上也已足够高效，
// getdents64 系统调用本身就会返回 d_type）

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

        let size = if is_dir {
            0
        } else {
            entry.metadata().map(|m| m.len()).unwrap_or(0)
        };

        entries.push(FastDirEntry {
            path: entry_path,
            name,
            size,
            is_dir,
            is_symlink,
        });
    }

    Ok(entries)
}

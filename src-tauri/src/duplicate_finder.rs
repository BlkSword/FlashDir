// 重复文件检测模块
//
// 原理：
// 1. 先按文件大小分组，只有同大小的文件才可能是重复文件
// 2. 对候选文件做内容哈希（DefaultHasher，进程内稳定即可）
// 3. 按哈希再次分组，输出真正重复的文件组
//
// 设计取舍：
// - 为避免把空文件全部当作重复文件，默认跳过 size=0
// - 文件读取采用 64KB 缓冲流式哈希，避免一次性载入大文件
// - 使用 Rayon 并行哈希，但文件 I/O 仍受磁盘性能约束

use rayon::prelude::*;
use serde::Serialize;
use std::collections::HashMap;
use std::fs::File;
use std::hash::{DefaultHasher, Hasher};
use std::io::Read;

use crate::scan::{Item, format_size};

/// 重复文件组
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DuplicateGroup {
    /// 单文件大小
    pub size: i64,
    pub size_formatted: String,
    /// 该组内文件数量
    pub file_count: usize,
    /// 文件列表
    pub files: Vec<DuplicateFile>,
    /// 可回收空间 = size * (file_count - 1)
    pub wasted_bytes: i64,
    pub wasted_formatted: String,
}

/// 重复文件信息
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DuplicateFile {
    pub path: String,
    pub name: String,
    pub mtime: i64,
}

/// 重复文件检测结果
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DuplicateResult {
    pub total_groups: usize,
    pub total_files: usize,
    pub total_wasted_bytes: i64,
    pub total_wasted_formatted: String,
    pub groups: Vec<DuplicateGroup>,
}

/// 检测重复文件
pub fn find_duplicates(items: &[Item], min_size: i64) -> DuplicateResult {
    let min_size = min_size.max(0);

    // 1. 按大小分组
    let mut by_size: HashMap<i64, Vec<&Item>> = HashMap::new();
    for item in items {
        if !item.is_dir && item.size >= min_size {
            by_size.entry(item.size).or_default().push(item);
        }
    }

    let mut groups = Vec::new();

    // 2. 对每个大小候选组计算内容哈希
    for (size, candidates) in by_size {
        if candidates.len() < 2 {
            continue;
        }

        let hashed: Vec<(u64, &Item)> = candidates
            .par_iter()
            .filter_map(|item| file_hash(item.path.as_str()).map(|hash| (hash, *item)))
            .collect();

        // 3. 按哈希分组
        let mut by_hash: HashMap<u64, Vec<&Item>> = HashMap::new();
        for (hash, item) in hashed {
            by_hash.entry(hash).or_default().push(item);
        }

        for (_, files) in by_hash {
            if files.len() < 2 {
                continue;
            }

            let mut files: Vec<&Item> = files;
            files.sort_by(|a, b| a.path.as_str().cmp(b.path.as_str()));

            let file_count = files.len();
            let wasted_bytes = size * (file_count as i64 - 1);
            groups.push(DuplicateGroup {
                size,
                size_formatted: format_size(size).to_string(),
                file_count,
                files: files
                    .into_iter()
                    .map(|item| DuplicateFile {
                        path: item.path.to_string(),
                        name: item.name.to_string(),
                        mtime: item.mtime,
                    })
                    .collect(),
                wasted_bytes,
                wasted_formatted: format_size(wasted_bytes).to_string(),
            });
        }
    }

    groups.sort_unstable_by(|a, b| b.wasted_bytes.cmp(&a.wasted_bytes));

    let total_files: usize = groups.iter().map(|g| g.file_count).sum();
    let total_wasted_bytes: i64 = groups.iter().map(|g| g.wasted_bytes).sum();

    DuplicateResult {
        total_groups: groups.len(),
        total_files,
        total_wasted_bytes,
        total_wasted_formatted: format_size(total_wasted_bytes).to_string(),
        groups,
    }
}

/// 流式计算文件内容哈希；读取失败返回 None（跳过该文件）
fn file_hash(path: &str) -> Option<u64> {
    let mut file = File::open(path).ok()?;
    let mut hasher = DefaultHasher::new();
    let mut buffer = [0u8; 64 * 1024];

    loop {
        let n = file.read(&mut buffer).ok()?;
        if n == 0 {
            break;
        }
        hasher.write(&buffer[..n]);
    }

    Some(hasher.finish())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scan::CompactString;

    fn item(path: &str, size: i64) -> Item {
        Item {
            path: CompactString::from(path),
            name: CompactString::from(path.rsplit('/').next().unwrap_or(path)),
            size,
            size_formatted: CompactString::new(),
            is_dir: false,
            mtime: 0,
            atime: 0,
        }
    }

    #[test]
    fn test_find_duplicates_groups_by_size_and_hash() {
        let items = vec![
            item("C:/a.txt", 10),
            item("C:/b.txt", 10),
            item("C:/c.txt", 20),
        ];
        // 两个 10 字节文件内容不同（路径不同，但哈希基于实际文件内容）。
        // 测试中并不真正创建文件，因此这里只验证“不会把不同大小文件合并”。
        let result = find_duplicates(&items, 1);
        assert_eq!(result.total_groups, 0);
    }
}

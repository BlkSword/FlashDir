// 重复文件检测模块
//
// 原理：
// 1. 先按文件大小分组，只有同大小的文件才可能是重复文件
// 2. 对候选文件做内容哈希（DefaultHasher，进程内稳定即可）作为粗筛
// 3. 同体积 + 同哈希的候选再做**逐字节内容比对**确认，才输出重复文件组
// 4. 识别硬链接：硬链接共享同一份数据，删除多余"副本"并不释放空间，
//    因此不计入可回收空间（wasted_bytes）
//
// 设计取舍：
// - 为避免把空文件全部当作重复文件，默认跳过 size=0
// - 哈希与比对都采用固定大小缓冲区分块读写，避免一次性载入大文件
// - 使用 Rayon 并行哈希，但文件 I/O 仍受磁盘性能约束
//
// 旧实现的两个缺陷：
// - 只用 DefaultHasher(u64) 作唯一判据：哈希碰撞（不同内容同摘要）会报出假重复。
//   现在哈希只用于缩小候选范围，最终由 files_equal 逐字节确认。
// - wasted_bytes = size * (count - 1) 把硬链接也算成可回收空间：硬链接指向同一份
//   数据，删除它不会释放空间，因此 nNumberOfLinks > 1 的文件不计入 wasted_bytes。

use rayon::prelude::*;
use serde::Serialize;
use std::collections::HashMap;
use std::fs::File;
use std::hash::{DefaultHasher, Hasher};
use std::io::Read;

use crate::scan::{Item, format_size};

/// 内容比对缓冲区大小（64KB）：逐块比较，内存占用与文件大小无关
const COMPARE_CHUNK: usize = 64 * 1024;

/// 重复文件组
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DuplicateGroup {
    /// 单文件大小
    pub size: i64,
    pub size_formatted: String,
    /// 该组内文件数量（含硬链接路径）
    pub file_count: usize,
    /// 文件列表
    pub files: Vec<DuplicateFile>,
    /// 可回收空间 = size * (可回收副本数 - 1)，硬链接不计入可回收副本数
    pub wasted_bytes: i64,
    pub wasted_formatted: String,
    /// 追加字段（向后兼容，老消费方忽略即可）：组内硬链接文件数（nNumberOfLinks > 1）。
    /// 这些文件与其他路径共享同一份数据，不计入 wasted_bytes。
    pub hardlink_count: usize,
}

/// 重复文件信息
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DuplicateFile {
    pub path: String,
    pub name: String,
    pub mtime: i64,
    /// 追加字段（向后兼容）：该路径是否为硬链接（Windows 上 nNumberOfLinks > 1；
    /// 非 Windows 平台恒为 false）
    pub hardlink: bool,
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
    /// 追加字段（向后兼容）：本次结果中识别出的硬链接文件总数（已被排除在
    /// total_wasted_bytes 之外）
    pub hardlink_files: usize,
    /// 追加字段（向后兼容）：存在硬链接时的人类可读说明，否则为 None（前端/MCP
    /// 的原有字段与文案都不受影响）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hardlink_note: Option<String>,
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
    let mut hardlink_files_total = 0usize;

    // 2. 对每个大小候选组计算内容哈希（仅作粗筛）
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

            // 3.5 字节级复核：DefaultHasher 只是 64 位摘要，碰撞会产生假重复。
            //     对"同体积 + 同哈希"的候选做真正的逐字节内容比对（固定缓冲区），
            //     只有内容完全一致的文件才归入同一重复组。
            for mut cluster in partition_by_content(files) {
                if cluster.len() < 2 {
                    continue;
                }
                cluster.sort_by(|a, b| a.path.as_str().cmp(b.path.as_str()));

                // 4. 硬链接识别：nNumberOfLinks > 1 的文件与其他路径共享同一份数据，
                //    删除这类"副本"不释放空间，必须从可回收空间里排除。
                let hardlink_flags: Vec<bool> = cluster
                    .par_iter()
                    .map(|item| is_hardlink(item.path.as_str()))
                    .collect();
                let hardlink_count = hardlink_flags.iter().filter(|f| **f).count();
                hardlink_files_total += hardlink_count;

                // 旧实现：wasted_bytes = size * (file_count - 1)，把硬链接也算成可回收。
                // 现在只按"非硬链接的文件（各自独占一份数据）"计数；只有 1 份时
                // 无可回收空间（不会出现负数）。
                let reclaimable_copies = cluster.len() - hardlink_count;
                let wasted_bytes = if reclaimable_copies > 1 {
                    size * (reclaimable_copies as i64 - 1)
                } else {
                    0
                };

                let file_count = cluster.len();
                groups.push(DuplicateGroup {
                    size,
                    size_formatted: format_size(size).to_string(),
                    file_count,
                    files: cluster
                        .into_iter()
                        .zip(hardlink_flags)
                        .map(|(item, hardlink)| DuplicateFile {
                            path: item.path.to_string(),
                            name: item.name.to_string(),
                            mtime: item.mtime,
                            hardlink,
                        })
                        .collect(),
                    wasted_bytes,
                    wasted_formatted: format_size(wasted_bytes).to_string(),
                    hardlink_count,
                });
            }
        }
    }

    // 主排序仍是"可回收空间降序"（契约不变），size 作为稳定的次级排序键
    groups.sort_unstable_by(|a, b| {
        b.wasted_bytes
            .cmp(&a.wasted_bytes)
            .then_with(|| b.size.cmp(&a.size))
    });

    let total_files: usize = groups.iter().map(|g| g.file_count).sum();
    let total_wasted_bytes: i64 = groups.iter().map(|g| g.wasted_bytes).sum();

    // 说明性文案：让上层（前端 / MCP summary）无需改动就能知道"已排除硬链接"
    let hardlink_note = if hardlink_files_total > 0 {
        Some(format!(
            "已排除 {} 个硬链接（与其它路径共享同一份数据，删除不释放空间）",
            hardlink_files_total
        ))
    } else {
        None
    };

    DuplicateResult {
        total_groups: groups.len(),
        total_files,
        total_wasted_bytes,
        total_wasted_formatted: format_size(total_wasted_bytes).to_string(),
        groups,
        hardlink_files: hardlink_files_total,
        hardlink_note,
    }
}

/// 把"同体积 + 同哈希"的候选按**真实内容**切成若干组（组内两两逐字节相等）。
///
/// 每个候选只与已有组的代表文件比较一次，复杂度 O(n × 组数)；
/// 读取失败的文件视为"不等于任何文件"，单独成组并在后续被丢弃 ——
/// 宁可漏报也不能把无法确认相等的文件报成重复。
fn partition_by_content<'a>(files: Vec<&'a Item>) -> Vec<Vec<&'a Item>> {
    let mut clusters: Vec<Vec<&'a Item>> = Vec::new();

    for item in files {
        let mut placed = false;
        for cluster in clusters.iter_mut() {
            if files_equal(cluster[0].path.as_str(), item.path.as_str()) {
                cluster.push(item);
                placed = true;
                break;
            }
        }
        if !placed {
            clusters.push(vec![item]);
        }
    }

    clusters
}

/// 逐字节比较两个文件内容是否完全相同。
///
/// - 使用两个固定大小缓冲区（64KB × 2）流式比较，内存占用与文件大小无关；
/// - 一旦发现不同立即返回 false（大文件通常在第一个块就能判定）；
/// - 任一文件打开/读取失败都返回 false：字节无法确认相等时不得当作重复文件。
fn files_equal(a: &str, b: &str) -> bool {
    // 同一路径（items 里可能重复出现同一条目）无需读盘
    if a == b {
        return true;
    }

    let (Ok(mut fa), Ok(mut fb)) = (File::open(a), File::open(b)) else {
        return false;
    };

    let mut ba = [0u8; COMPARE_CHUNK];
    let mut bb = [0u8; COMPARE_CHUNK];

    loop {
        let Some(na) = read_chunk(&mut fa, &mut ba) else {
            return false;
        };
        let Some(nb) = read_chunk(&mut fb, &mut bb) else {
            return false;
        };
        if na != nb {
            // 文件长度不同（例如扫描之后被改写）
            return false;
        }
        if na == 0 {
            return true;
        }
        if ba[..na] != bb[..nb] {
            return false;
        }
    }
}

/// 从文件中读取"尽可能填满缓冲区"的一块内容，返回实际字节数。
///
/// `Read::read` 允许短读，直接比较短读结果会让两个完全相同的文件看起来不同；
/// 这里循环读到填满或 EOF，保证两侧比较的是等长的数据块。
fn read_chunk(file: &mut File, buf: &mut [u8]) -> Option<usize> {
    let mut filled = 0usize;
    while filled < buf.len() {
        match file.read(&mut buf[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return None,
        }
    }
    Some(filled)
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

/// 是否为硬链接（同一份数据被多个路径引用）。
///
/// Windows：`GetFileInformationByHandle` 的 `nNumberOfLinks > 1`。
/// 非 Windows：没有等价且低成本的判定，恒返回 false（保持原有行为）。
#[cfg(target_os = "windows")]
fn is_hardlink(path: &str) -> bool {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_READ_ATTRIBUTES,
        FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, GetFileInformationByHandle,
        OPEN_EXISTING,
    };

    let wide: Vec<u16> = std::ffi::OsStr::new(path)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();

    // 只请求 FILE_READ_ATTRIBUTES：查询链接数不需要数据读权限，
    // 且共享读写/删除，避免占用其它进程正在使用的文件。
    let handle = unsafe {
        CreateFileW(
            wide.as_ptr(),
            FILE_READ_ATTRIBUTES,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            0,
        )
    };

    if handle == INVALID_HANDLE_VALUE {
        // 打不开就无法确认：按"非硬链接"处理（与旧行为一致，不让结果凭空变少）
        return false;
    }

    let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
    let ok = unsafe { GetFileInformationByHandle(handle, &mut info) };
    unsafe { CloseHandle(handle) };

    ok != 0 && info.nNumberOfLinks > 1
}

#[cfg(not(target_os = "windows"))]
fn is_hardlink(_path: &str) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scan::CompactString;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static COUNTER: AtomicUsize = AtomicUsize::new(0);

    /// 测试用临时目录：每个测试一个独立目录，Drop 时清理。
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let dir = std::env::temp_dir().join(format!(
                "flashdir_dup_test_{}_{}_{}",
                tag,
                std::process::id(),
                n
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("创建测试目录失败");
            TempDir(dir)
        }

        fn path(&self, name: &str) -> String {
            self.0.join(name).to_string_lossy().to_string()
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn item(path: &str, size: i64) -> Item {
        Item {
            path: CompactString::from(path),
            name: CompactString::from(path.rsplit(['/', '\\']).next().unwrap_or(path)),
            size,
            size_formatted: CompactString::new(),
            is_dir: false,
            mtime: 0,
            atime: 0,
        }
    }

    fn write_file(path: &str, data: &[u8]) {
        std::fs::write(path, data).expect("写测试文件失败");
    }

    /// 生成可复现的伪随机内容（seed 不同则内容不同）
    fn make_payload(len: usize, seed: u8) -> Vec<u8> {
        (0..len)
            .map(|i| (i as u8).wrapping_mul(31).wrapping_add(seed))
            .collect()
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

    /// 字节级复核必须跨多个 64KB 缓冲区工作，且能发现任意位置的差异。
    #[test]
    fn files_equal_compares_chunked_content() {
        let dir = TempDir::new("cmp");
        let a = dir.path("a.bin");
        let b = dir.path("b.bin");

        let len = COMPARE_CHUNK * 2 + 123; // 跨 3 个缓冲区
        let payload = make_payload(len, 7);
        write_file(&a, &payload);
        write_file(&b, &payload);
        assert!(files_equal(&a, &b), "内容相同应判定相等（跨多个块）");

        // 差异在最后一个字节
        let mut tail_diff = payload.clone();
        let last = tail_diff.len() - 1;
        tail_diff[last] ^= 0xFF;
        write_file(&b, &tail_diff);
        assert!(!files_equal(&a, &b), "最后 1 字节不同必须判定不等");

        // 差异在第二个块中间
        let mut mid_diff = payload.clone();
        mid_diff[COMPARE_CHUNK + 5] ^= 0xFF;
        write_file(&b, &mid_diff);
        assert!(!files_equal(&a, &b), "第二个块中的差异必须被发现");

        // 长度不同（扫描之后文件被改写等）
        write_file(&b, &payload[..payload.len() - 1]);
        assert!(!files_equal(&a, &b), "长度不同必须判定不等");

        // 任一侧读不到 → 不得当作相等（宁可漏报，不可误报）
        assert!(!files_equal(&a, &dir.path("missing.bin")));
    }

    /// 回归：旧实现只信任 DefaultHasher，同体积 + 同哈希即报重复，碰撞会假重复。
    /// 这里直接用字节比对验证"同体积但差 1 字节"的文件不会被归入同一组。
    #[test]
    fn find_duplicates_verifies_bytes_not_only_hash() {
        let dir = TempDir::new("dup");
        let payload = make_payload(COMPARE_CHUNK + 500, 3);

        let a = dir.path("a.bin");
        let b = dir.path("b.bin");
        let c = dir.path("c.bin");
        let d = dir.path("d.bin");
        write_file(&a, &payload);
        write_file(&b, &payload);
        write_file(&d, &payload);

        let mut diff = payload.clone();
        diff[COMPARE_CHUNK + 100] ^= 0x01; // 同体积，仅 1 字节不同
        write_file(&c, &diff);

        let size = payload.len() as i64;
        let items = vec![
            item(&a, size),
            item(&b, size),
            item(&c, size),
            item(&d, size),
        ];
        let res = find_duplicates(&items, 1);

        assert_eq!(res.total_groups, 1, "只应有一组真正的重复");
        assert_eq!(res.total_files, 3);
        let g = &res.groups[0];
        assert_eq!(g.file_count, 3);
        assert_eq!(g.wasted_bytes, size * 2, "3 份独立副本可回收 2 份");
        let paths: Vec<&str> = g.files.iter().map(|f| f.path.as_str()).collect();
        assert!(
            !paths.contains(&c.as_str()),
            "同体积仅差 1 字节的文件不得算作重复"
        );
        assert_eq!(res.hardlink_files, 0);
        assert!(res.hardlink_note.is_none());
        assert!(g.files.iter().all(|f| !f.hardlink));
    }

    /// 模拟"哈希碰撞"场景：4 个同体积文件落在同一个候选桶里，
    /// 字节级复核必须把它们切成两组（a 对 / b 对）。
    #[test]
    fn partition_by_content_splits_collision_bucket() {
        let dir = TempDir::new("collide");
        let len = COMPARE_CHUNK + 10;
        let pa = make_payload(len, 11);
        let pb = make_payload(len, 12);

        let a1 = dir.path("a1.bin");
        let a2 = dir.path("a2.bin");
        let b1 = dir.path("b1.bin");
        let b2 = dir.path("b2.bin");
        write_file(&a1, &pa);
        write_file(&a2, &pa);
        write_file(&b1, &pb);
        write_file(&b2, &pb);

        let n = len as i64;
        let items = vec![
            item(&a1, n),
            item(&a2, n),
            item(&b1, n),
            item(&b2, n),
        ];
        let refs: Vec<&Item> = items.iter().collect();
        let clusters = partition_by_content(refs);

        assert_eq!(clusters.len(), 2, "字节级复核应把碰撞桶切成两组");
        let mut counts: Vec<usize> = clusters.iter().map(|c| c.len()).collect();
        counts.sort_unstable();
        assert_eq!(counts, vec![2, 2]);
        for cluster in &clusters {
            assert!(files_equal(
                cluster[0].path.as_str(),
                cluster[1].path.as_str()
            ));
        }
        assert!(!files_equal(
            clusters[0][0].path.as_str(),
            clusters[1][0].path.as_str()
        ));
    }

    /// 回归：硬链接共享同一份数据，删除"多余副本"不释放空间 →
    /// wasted_bytes 必须为 0（旧实现按 size * (count - 1) 虚报可回收空间）。
    #[test]
    #[cfg(target_os = "windows")]
    fn hardlink_paths_are_excluded_from_wasted_bytes() {
        let dir = TempDir::new("hardlink");
        let payload = make_payload(4096, 21);
        let orig = dir.path("data.bin");
        let link = dir.path("link.bin");
        write_file(&orig, &payload);
        if std::fs::hard_link(&orig, &link).is_err() {
            eprintln!("[test] 当前文件系统不支持硬链接，跳过该用例");
            return;
        }

        let size = payload.len() as i64;
        let items = vec![item(&orig, size), item(&link, size)];
        let res = find_duplicates(&items, 1);

        assert_eq!(res.total_groups, 1, "同一份数据的两个路径仍是重复组");
        assert_eq!(res.total_files, 2);
        let g = &res.groups[0];
        assert_eq!(g.hardlink_count, 2, "两条路径的 nNumberOfLinks 都 > 1");
        assert_eq!(g.wasted_bytes, 0, "硬链接删除不释放空间，可回收必须为 0");
        assert_eq!(g.wasted_formatted, format_size(0).to_string());
        assert_eq!(res.total_wasted_bytes, 0);
        assert_eq!(res.hardlink_files, 2);
        assert!(res.hardlink_note.is_some(), "存在硬链接时应给出说明文案");
        assert!(g.files.iter().all(|f| f.hardlink));
    }

    /// 混合场景：硬链接不计入可回收副本，普通副本仍然计入。
    #[test]
    #[cfg(target_os = "windows")]
    fn mixed_hardlink_and_regular_copies() {
        let dir = TempDir::new("mixed");
        let payload = make_payload(8192, 33);
        let orig = dir.path("data.bin");
        let link = dir.path("link.bin");
        let copy1 = dir.path("copy1.bin");
        let copy2 = dir.path("copy2.bin");
        write_file(&orig, &payload);
        if std::fs::hard_link(&orig, &link).is_err() {
            eprintln!("[test] 当前文件系统不支持硬链接，跳过该用例");
            return;
        }
        write_file(&copy1, &payload);
        write_file(&copy2, &payload);

        let size = payload.len() as i64;
        let items = vec![
            item(&orig, size),
            item(&link, size),
            item(&copy1, size),
            item(&copy2, size),
        ];
        let res = find_duplicates(&items, 1);

        assert_eq!(res.total_files, 4);
        let g = &res.groups[0];
        assert_eq!(g.file_count, 4);
        assert_eq!(g.hardlink_count, 2, "data.bin 与 link.bin 是同一份数据");
        // 保守口径（宁可少报也不虚报）：只有 2 个普通副本参与可回收计算 → 1 份
        assert_eq!(g.wasted_bytes, size, "只按非硬链接副本计算可回收空间");
        assert_eq!(res.hardlink_files, 2);
    }

    /// 契约：公开字段名不能变（前端与 MCP 依赖）。
    #[test]
    fn result_json_keeps_backward_compatible_fields() {
        let dir = TempDir::new("contract");
        let payload = make_payload(1024, 5);
        let a = dir.path("a.bin");
        let b = dir.path("b.bin");
        write_file(&a, &payload);
        write_file(&b, &payload);

        let size = payload.len() as i64;
        let res = find_duplicates(&[item(&a, size), item(&b, size)], 1);
        let json = serde_json::to_value(&res).expect("序列化失败");

        for key in [
            "totalGroups",
            "totalFiles",
            "totalWastedBytes",
            "totalWastedFormatted",
            "groups",
            // 追加字段
            "hardlinkFiles",
        ] {
            assert!(json.get(key).is_some(), "缺少契约字段 {key}");
        }
        let g = &json["groups"][0];
        for key in [
            "size",
            "sizeFormatted",
            "fileCount",
            // serde 输出为 wastedBytes；MCP 工具层把它映射成 "wasted"（工具层由他人维护）
            "wastedBytes",
            "wastedFormatted",
            "files",
            // 追加字段
            "hardlinkCount",
        ] {
            assert!(g.get(key).is_some(), "缺少契约字段 groups[].{key}");
        }
        for key in ["path", "name", "mtime", "hardlink"] {
            assert!(
                g["files"][0].get(key).is_some(),
                "缺少契约字段 groups[].files[].{key}"
            );
        }
        // 无硬链接时说明字段不出现（保持旧响应形状）
        assert!(json.get("hardlinkNote").is_none());
    }
}

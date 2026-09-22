// NTFS MFT (Master File Table) 直接读取扫描器
//
// Everything 的核心原理：不递归遍历目录，而是直接顺序读取 NTFS 的 $MFT 文件。
// $MFT 是一张包含全盘所有文件元数据的扁平大表，通常 200-500MB，
// 顺序读取即可获得所有文件的路径、大小、属性，无需进入任何子目录。
//
// MFT 记录结构（每条 1KB）：
//   [FILE 签名][属性列表]
//   属性类型 0x10: $STANDARD_INFORMATION (文件标志)
//   属性类型 0x30: $FILE_NAME (文件名、父目录 FRN、文件实际大小)
//   属性类型 0x80: $DATA (数据位置/大小)
//
// 我们只需要 $FILE_NAME 属性即可获得：
//   - 文件名 (UTF-16LE)
//   - 父目录 FRN (File Reference Number，用于构建完整路径)
//   - 文件实际大小 (real_size)
//   - 文件属性 (可判断是否为目录)

use rayon::prelude::*;
use std::io;
use std::mem;
use std::path::PathBuf;

use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, GENERIC_READ, INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, ReadFile, SetFilePointerEx, FILE_SHARE_READ, FILE_SHARE_WRITE,
    OPEN_EXISTING,
};
use windows_sys::Win32::System::IO::DeviceIoControl;

// ─── NTFS 常量 ────────────────────────────────────────────

/// FSCTL_GET_NTFS_VOLUME_DATA = CTL_CODE(FILE_DEVICE_FILE_SYSTEM, 25, METHOD_BUFFERED, FILE_ANY_ACCESS)
const FSCTL_GET_NTFS_VOLUME_DATA: u32 = 0x00090064;

/// FSCTL_GET_NTFS_FILE_RECORD = CTL_CODE(FILE_DEVICE_FILE_SYSTEM, 26, METHOD_BUFFERED, FILE_ANY_ACCESS)
const FSCTL_GET_NTFS_FILE_RECORD: u32 = 0x00090068;

/// NTFS_FILE_RECORD_INPUT_BUFFER
#[repr(C)]
struct NtfsFileRecordInputBuffer {
    file_reference_number: i64,
}

/// NTFS_FILE_RECORD_OUTPUT_BUFFER（头部，实际缓冲区紧随其后）
/// C 结构布局：LARGE_INTEGER(8) + DWORD(4) + BYTE[1]，FileRecordBuffer 偏移为 12。
#[repr(C)]
struct NtfsFileRecordOutputBufferHeader {
    file_reference_number: i64,
    file_record_length: u32,
}

const FILE_RECORD_OUT_HEADER_SIZE: usize = 12;

/// NTFS_VOLUME_DATA_BUFFER 结构体
#[repr(C)]
struct NtfsVolumeData {
    volume_serial_number: i64,
    number_sectors: i64,
    total_clusters: i64,
    free_clusters: i64,
    total_reserved: i64,
    bytes_per_sector: u32,
    bytes_per_cluster: u32,
    bytes_per_file_record_segment: u32,
    clusters_per_file_record_segment: u32,
    mft_valid_data_length: i64,
    mft_start_lcn: i64,
    mft2_start_lcn: i64,
    mft_zone_start: i64,
    mft_zone_end: i64,
}

// ─── MFT 原始记录结构 ──────────────────────────────────────

/// MFT 文件记录签名 "FILE"
const MFT_FILE_SIGNATURE: u32 = 0x454C4946;

/// MFT 记录标志
const MFT_RECORD_IN_USE: u16 = 0x0001;
const MFT_RECORD_IS_DIR: u16 = 0x0002;

/// 属性类型常量
const ATTR_STANDARD_INFORMATION: u32 = 0x10;
const ATTR_FILE_NAME: u32 = 0x30;
const ATTR_DATA: u32 = 0x80;
const ATTR_END: u32 = 0xFFFFFFFF;

/// $FILE_NAME 属性内的内容偏移
const FN_PARENT_FRN: usize = 0x00;     // 父目录 FRN (u64)
const FN_CREATION_TIME: usize = 0x08;   // 创建时间 (u64, Windows FILETIME)
const FN_MODIFY_TIME: usize = 0x10;    // 修改时间 (u64)
const FN_MFT_CHANGE_TIME: usize = 0x18; // MFT 变更时间 (u64)
const FN_ACCESS_TIME: usize = 0x20;     // 访问时间 (u64)
const FN_ALLOCATED_SIZE: usize = 0x28;  // 分配大小 (u64)
const FN_REAL_SIZE: usize = 0x30;       // 实际大小 (u64) ← 我们要的文件大小
const FN_FLAGS: usize = 0x38;           // 文件属性标志 (u32)
const FN_REPARSE: usize = 0x3C;         // 重解析值 (u32)
const FN_NAME_LENGTH: usize = 0x40;     // 文件名长度 (u8)
const FN_NAME_NAMESPACE: usize = 0x41;  // 文件名命名空间 (u8)
const FN_NAME_START: usize = 0x42;      // 文件名开始 (UTF-16LE)

/// NTFS 文件属性标志（与 Win32 FILE_ATTRIBUTE_* 一致）
const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x10;
const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;

// ─── 解析后的 MFT 条目 ─────────────────────────────────────

/// 从 MFT 解析出的文件/目录条目
struct MftEntry {
    /// 文件名（不含路径）
    name: String,
    /// 父目录的 FRN
    parent_frn: u64,
    /// 文件实际大小（字节），目录为 0
    real_size: u64,
    /// 是否为目录
    is_dir: bool,
    /// 是否为重解析点（符号链接等）
    is_reparse: bool,
    /// 修改时间（Unix 秒级时间戳，来自 $FILE_NAME.ModifiedTime）
    mtime: i64,
    /// 访问时间（Unix 秒级时间戳，来自 $FILE_NAME.AccessTime；Windows 可能延迟更新）
    atime: i64,
}

/// Windows FILETIME（1601 年起 100ns 间隔）→ Unix 秒级时间戳
fn filetime_to_unix(ft: u64) -> i64 {
    ((ft as i64 - 116444736000000000) / 10_000_000).max(0)
}

/// MFT 记录索引：**按记录号直接下标访问**的扁平数组。
///
/// 早期用 HashMap<u64, MftEntry>，760k 条记录要哈希 760k 次 u64 键，
/// 且遍历顺序随机、缓存不友好；记录号本身就是连续下标，直接用 Vec。
/// （Vec 中 None 表示该记录未使用/无法解析。）
type MftIndex = Vec<Option<MftEntry>>;

/// MFT 扫描器 —— 打开 NTFS 卷并顺序读取 $MFT
pub struct MftScanner {
    volume_handle: isize,
    bytes_per_cluster: u64,
    mft_start_offset: u64,
    mft_valid_size: u64,
    mft_record_size: u32,
    /// $MFT 自身的 $DATA data runs；用于同时支持全量顺序读取和单条记录定位。
    /// 为空时回退到连续读取（兼容极少数解析失败的场景）。
    mft_data_runs: Vec<(u64, u64)>,
    /// 本次扫描的取消代号（见 crate::cancel 的 generation 模型）
    cancel_id: u64,
}

/// MFT 扫描的最终结果
pub struct MftScanResult {
    /// 文件列表（带完整路径）
    pub files: Vec<MftFileInfo>,
    /// 扫描的文件总数
    pub file_count: usize,
    /// 扫描的目录总数
    pub dir_count: usize,
    /// 读取的 MFT 数据总字节数
    pub data_read: u64,
    /// 读取 + 解析 MFT 记录耗时（秒）
    pub read_secs: f64,
    /// 构建完整路径耗时（秒）
    pub path_secs: f64,
}


/// 单个文件的 MFT 信息
#[derive(Debug, Clone)]
pub struct MftFileInfo {
    pub path: String,
    pub name: String,
    pub size: u64,
    pub is_dir: bool,
    /// 修改时间（Unix 秒级时间戳）
    pub mtime: i64,
    /// 访问时间（Unix 秒级时间戳，0 = 未知）
    pub atime: i64,
}

/// 单条 MFT 记录解析结果（用于 FRN → 路径解析）
#[derive(Debug, Clone)]
pub struct MftRecordInfo {
    pub name: String,
    pub parent_frn: u64,
    pub is_dir: bool,
    pub real_size: u64,
    /// 修改时间（Unix 秒级时间戳）
    pub mtime: i64,
    /// 访问时间（Unix 秒级时间戳）
    pub atime: i64,
}

impl MftScanner {
    /// 打开指定盘符的 NTFS 卷
    /// `drive_letter` 例如 'C' 表示 C: 盘
    pub fn open(drive_letter: char) -> io::Result<Self> {
        let volume_path = format!(r"\\.\{}:", drive_letter);
        Self::open_volume(&volume_path)
    }

    fn open_volume(volume_path: &str) -> io::Result<Self> {
        // 宽字符路径
        let wide_path: Vec<u16> = volume_path
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();

        unsafe {
            let handle = CreateFileW(
                wide_path.as_ptr(),
                GENERIC_READ,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                std::ptr::null_mut(),
                OPEN_EXISTING,
                0,
                0, // hTemplateFile: NULL
            );

            if handle == INVALID_HANDLE_VALUE {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    format!(
                        "无法打开卷 {}（需要管理员权限）",
                        volume_path
                    ),
                ));
            }

            // 获取 NTFS 卷信息
            let mut vol_data: NtfsVolumeData = mem::zeroed();
            let mut bytes_returned: u32 = 0;

            let success = DeviceIoControl(
                handle,
                FSCTL_GET_NTFS_VOLUME_DATA,
                std::ptr::null_mut(),
                0,
                &mut vol_data as *mut _ as *mut _,
                mem::size_of::<NtfsVolumeData>() as u32,
                &mut bytes_returned,
                std::ptr::null_mut(),
            );

            if success == 0 {
                CloseHandle(handle);
                return Err(io::Error::new(
                    io::ErrorKind::Other,
                    format!(
                        "获取 NTFS 卷信息失败: {}",
                        GetLastError()
                    ),
                ));
            }

            let mft_start_offset =
                (vol_data.mft_start_lcn as u64) * (vol_data.bytes_per_cluster as u64);
            let mft_valid_size = vol_data.mft_valid_data_length as u64;

            let mut scanner = Self {
                volume_handle: handle,
                bytes_per_cluster: vol_data.bytes_per_cluster as u64,
                mft_start_offset,
                mft_valid_size,
                mft_record_size: vol_data.bytes_per_file_record_segment,
                mft_data_runs: Vec::new(),
                cancel_id: 0,
            };
            // 提前解析并缓存 $MFT data runs，供全量扫描和单条记录读取共用。
            // 解析失败时保持空列表，后续回退到连续读取。
            scanner.mft_data_runs = scanner.read_mft_data_runs().unwrap_or_default();
            Ok(scanner)
        }
    }

    /// 绑定本次扫描的取消代号，使取消检查按"代"生效
    pub fn set_cancel_id(&mut self, cancel_id: u64) {
        self.cancel_id = cancel_id;
    }

    /// 执行 MFT 扫描，返回所有文件的元数据
    pub fn scan(&self) -> io::Result<MftScanResult> {
        // 第一步：顺序读取 $MFT 并解析记录（rayon 并行解析）
        let read_start = std::time::Instant::now();
        let index = self.read_all_records()?;
        let read_secs = read_start.elapsed().as_secs_f64();

        // 计数（扁平数组直接遍历，无哈希开销）
        let mut file_count = 0usize;
        let mut dir_count = 0usize;
        for entry in index.iter().flatten() {
            if entry.is_dir {
                dir_count += 1;
            } else {
                file_count += 1;
            }
        }

        // 第二步：用"父链 + 记忆化"构建完整路径（无 children_map / DFS clone）
        let path_start = std::time::Instant::now();
        let files = Self::build_path_hierarchy(&index);
        let path_secs = path_start.elapsed().as_secs_f64();

        Ok(MftScanResult {
            files,
            file_count,
            dir_count,
            data_read: self.mft_valid_size,
            read_secs,
            path_secs,
        })
    }


    /// 读取 $MFT 自身的 $DATA 属性 data runs，返回 [(start_lcn, cluster_count)]。
    /// $MFT 文件记录号是 0，通过 FSCTL_GET_NTFS_FILE_RECORD 读取可正确处理 MFT 碎片。
    fn read_mft_data_runs(&self) -> io::Result<Vec<(u64, u64)>> {
        let record_size = self.mft_record_size as usize;
        let out_buffer_size = FILE_RECORD_OUT_HEADER_SIZE + record_size + 64;
        let mut out_buffer: Vec<u8> = vec![0u8; out_buffer_size];

        let input = NtfsFileRecordInputBuffer {
            file_reference_number: 0,
        };

        let mut bytes_returned: u32 = 0;
        let success = unsafe {
            DeviceIoControl(
                self.volume_handle,
                FSCTL_GET_NTFS_FILE_RECORD,
                &input as *const _ as *const _,
                mem::size_of::<NtfsFileRecordInputBuffer>() as u32,
                out_buffer.as_mut_ptr() as *mut _,
                out_buffer.len() as u32,
                &mut bytes_returned,
                std::ptr::null_mut(),
            )
        };

        if success == 0 {
            return Err(io::Error::new(
                io::ErrorKind::Other,
                format!(
                    "读取 $MFT 文件记录失败: {}",
                    unsafe { GetLastError() }
                ),
            ));
        }

        if bytes_returned < mem::size_of::<NtfsFileRecordOutputBufferHeader>() as u32 {
            return Err(io::Error::new(
                io::ErrorKind::Other,
                "$MFT 文件记录输出缓冲区过小",
            ));
        }

        let header = unsafe {
            &*(out_buffer.as_ptr() as *const NtfsFileRecordOutputBufferHeader)
        };
        let record_len = header.file_record_length as usize;
        if record_len == 0 || record_len > out_buffer.len() - FILE_RECORD_OUT_HEADER_SIZE {
            return Err(io::Error::new(
                io::ErrorKind::Other,
                format!("$MFT 文件记录长度无效: len={}, out_len={}", record_len, out_buffer.len()),
            ));
        }

        let record_data_start = FILE_RECORD_OUT_HEADER_SIZE;
        let mut record_buffer = out_buffer[record_data_start..record_data_start + record_len].to_vec();
        apply_mft_fixup(&mut record_buffer);

        // 遍历属性，找到 $DATA (0x80) 非驻留属性
        let first_attr_offset = u16_from_le(&record_buffer[0x14..0x16]) as usize;
        let mut attr_offset = first_attr_offset;
        loop {
            if attr_offset + 16 > record_buffer.len() {
                break;
            }

            let attr_type = u32_from_le(&record_buffer[attr_offset..attr_offset + 4]);
            if attr_type == ATTR_END {
                break;
            }

            let attr_length = u32_from_le(&record_buffer[attr_offset + 4..attr_offset + 8]) as usize;
            if attr_length < 16 || attr_length == 0 || attr_offset + attr_length > record_buffer.len() {
                break;
            }

            let non_resident = record_buffer[attr_offset + 8];

            if attr_type == ATTR_DATA && non_resident != 0 {
                let mapping_pairs_offset = u16_from_le(&record_buffer[attr_offset + 0x20..attr_offset + 0x22]) as usize;
                let runs_start = attr_offset + mapping_pairs_offset;
                if runs_start >= record_buffer.len() {
                    break;
                }
                return Ok(parse_data_runs(&record_buffer[runs_start..attr_offset + attr_length]));
            }

            attr_offset += attr_length;
        }

        // 如果 $MFT 极小（理论上不可能），data runs 解析失败，回退到连续读取
        Err(io::Error::new(
            io::ErrorKind::Other,
            "无法解析 $MFT 的 $DATA data runs",
        ))
    }

    /// 读取所有 $MFT FILE 记录，解析 $FILE_NAME 属性。
    /// 使用 $MFT 自身的 $DATA data runs 处理 MFT 碎片，确保不遗漏记录。
    fn read_all_records(&self) -> io::Result<MftIndex> {
        let record_size = self.mft_record_size as usize;
        let max_records = (self.mft_valid_size as usize) / record_size;
        let mut records: MftIndex = std::iter::repeat_with(|| None).take(max_records).collect();

        // 获取 $MFT 的 data runs（碎片位置）；优先使用 open 时缓存的 runs
        let data_runs = if self.mft_data_runs.is_empty() {
            self.read_mft_data_runs()?
        } else {
            self.mft_data_runs.clone()
        };

        // 一次读 1024 条记录（1MB），减少 syscall；解析用 rayon 并行
        let batch_records = 1024usize;
        let batch_size = batch_records * record_size;
        let mut buffer: Vec<u8> = vec![0u8; batch_size];

        let mut global_record_index: u64 = 0;
        let bytes_per_cluster = self.bytes_per_cluster;

        unsafe {
            for (start_lcn, cluster_count) in data_runs {
                let fragment_start_offset = start_lcn * bytes_per_cluster;
                let fragment_size = cluster_count * bytes_per_cluster;
                let records_in_fragment = (fragment_size / record_size as u64) as usize;

                for fragment_batch_start in (0..records_in_fragment).step_by(batch_records) {
                    if crate::cancel::is_requested_for(self.cancel_id) {
                        return Err(io::Error::new(
                            io::ErrorKind::Interrupted,
                            "scan cancelled",
                        ));
                    }

                    let records_in_batch =
                        (records_in_fragment - fragment_batch_start).min(batch_records);
                    let read_size = records_in_batch * record_size;

                    let seek_offset =
                        fragment_start_offset + (fragment_batch_start * record_size) as u64;
                    let mut distance_to_move: i64 = seek_offset as i64;
                    let result = SetFilePointerEx(
                        self.volume_handle,
                        seek_offset as i64,
                        &mut distance_to_move,
                        0, // FILE_BEGIN
                    );

                    if result == 0 {
                        global_record_index += records_in_batch as u64;
                        continue;
                    }

                    let mut bytes_read: u32 = 0;
                    let result = ReadFile(
                        self.volume_handle,
                        buffer.as_mut_ptr() as *mut _,
                        read_size as u32,
                        &mut bytes_read,
                        std::ptr::null_mut(),
                    );

                    // 记录编号必须与 $MFT 中的位置严格对应（FRN 低 48 位即记录号），
                    // 因此无论读取成功与否都要按批次条数推进编号。
                    let base = global_record_index as usize;
                    global_record_index += records_in_batch as u64;
                    if result == 0 || bytes_read == 0 {
                        continue;
                    }

                    let complete = (bytes_read as usize) / record_size;
                    let limit = records_in_batch
                        .min(complete)
                        .min(records.len().saturating_sub(base));
                    if limit == 0 {
                        continue;
                    }

                    // 并行解析本批次：每个线程只改自己那条记录对应的槽位
                    buffer[..limit * record_size]
                        .par_chunks_mut(record_size)
                        .zip(records[base..base + limit].par_iter_mut())
                        .enumerate()
                        .for_each(|(i, (record_data, slot))| {
                            apply_mft_fixup(record_data);
                            *slot = parse_mft_record(record_data, base + i);
                        });
                }
            }
        }

        Ok(records)
    }


    /// 构建完整路径（优化版：先建树，再 DFS）
    /// 产生的路径为 volume-relative，如 "Users/xxx/Documents"，不带盘符，也不带根目录前缀。
    /// 构建完整路径（父链 + 记忆化，O(n) 摊还）。
    ///
    /// 旧实现：children_map（FRN→Vec<FRN>）+ 从根 DFS，每个子节点都会 clone 父路径。
    /// 新实现：对每条记录沿父链向上走，遇到"已解析"就复用其路径；
    /// 环/断链的父链标记为不可达。产生的路径同样是 volume-relative。
    fn build_path_hierarchy(index: &MftIndex) -> Vec<MftFileInfo> {
        const ROOT_FRN: usize = 5;
        let n = index.len();
        // 0=未处理 1=处理中（可检测环） 2=已解析 3=不可达
        let mut state = vec![0u8; n];
        let mut paths: Vec<Option<String>> = std::iter::repeat_with(|| None).take(n).collect();

        for start in 0..n {
            if start == ROOT_FRN || index[start].is_none() || state[start] != 0 {
                continue;
            }

            // 沿父链向上，直到根 / 已解析节点 / 环 / 断链
            let mut chain: Vec<usize> = Vec::new();
            let mut cur = start;
            let mut base_path: Option<String> = None;
            let mut broken = false;
            loop {
                match state[cur] {
                    2 => {
                        base_path = paths[cur].clone();
                        break;
                    }
                    1 | 3 => {
                        broken = true;
                        break;
                    }
                    _ => {}
                }
                state[cur] = 1;
                chain.push(cur);

                let entry = match index[cur].as_ref() {
                    Some(entry) => entry,
                    None => {
                        broken = true;
                        break;
                    }
                };
                let parent = entry.parent_frn as usize;
                if parent == ROOT_FRN {
                    base_path = Some(String::new());
                    break;
                }
                if parent >= n || index[parent].is_none() {
                    broken = true;
                    break;
                }
                cur = parent;
            }

            if broken {
                for &node in &chain {
                    state[node] = 3;
                }
                continue;
            }

            // 自顶向下拼接（chain 是自底向上的，反转即可）
            let mut acc = base_path.unwrap_or_default();
            for &node in chain.iter().rev() {
                let name = index[node]
                    .as_ref()
                    .map(|entry| entry.name.as_str())
                    .unwrap_or("");
                let full = if acc.is_empty() {
                    name.to_string()
                } else {
                    format!("{}/{}", acc, name)
                };
                paths[node] = Some(full.clone());
                state[node] = 2;
                acc = full;
            }
        }

        // 取出结果（路径 String 直接 move，不额外 clone）
        let mut files = Vec::with_capacity(n);
        for (i, entry) in index.iter().enumerate() {
            if i == ROOT_FRN {
                continue;
            }
            if let Some(entry) = entry {
                if let Some(path) = paths[i].take() {
                    files.push(MftFileInfo {
                        path,
                        name: entry.name.clone(),
                        size: entry.real_size,
                        is_dir: entry.is_dir,
                        mtime: entry.mtime,
                        atime: entry.atime,
                    });
                }
            }
        }
        files
    }
}


impl Drop for MftScanner {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.volume_handle);
        }
    }
}

// ─── 单条 MFT 记录读取 & FRN 路径解析 ──────────────────────

impl MftScanner {
    /// 根据 MFT 记录号计算物理偏移。
    ///
    /// 优先使用缓存的 $MFT data runs 定位，避免 $MFT 碎片化时直接按连续偏移读取。
    /// data runs 为空时回退到传统连续读取。
    fn record_offset_for(&self, record_number: u64) -> Option<u64> {
        if self.mft_data_runs.is_empty() {
            return Some(self.mft_start_offset + record_number * self.mft_record_size as u64);
        }

        let record_size = self.mft_record_size as u64;
        let mut records_before = 0u64;
        for (start_lcn, cluster_count) in &self.mft_data_runs {
            let fragment_records =
                (cluster_count * self.bytes_per_cluster) / record_size;
            if record_number < records_before + fragment_records {
                let offset_in_fragment =
                    (record_number - records_before) * record_size;
                return Some((start_lcn * self.bytes_per_cluster) + offset_in_fragment);
            }
            records_before += fragment_records;
        }
        None
    }

    /// 读取单条 MFT 记录（按 FRN 索引）
    /// FRN 的低 48 位是 MFT 记录号，直接用作 $MFT 中的偏移
    pub fn read_single_record(&self, frn: u64) -> io::Result<Option<MftRecordInfo>> {
        // FRN 结构: [48-bit record number][16-bit sequence number]
        let record_number = frn & 0x0000FFFFFFFFFFFF;
        let record_size = self.mft_record_size as usize;
        let offset = match self.record_offset_for(record_number) {
            Some(offset) => offset,
            None => return Ok(None),
        };

        let mut buffer = vec![0u8; record_size];

        unsafe {
            // Seek 到记录位置
            let result = SetFilePointerEx(
                self.volume_handle,
                offset as i64,
                std::ptr::null_mut(),
                0, // FILE_BEGIN
            );

            if result == 0 {
                return Ok(None);
            }

            // 读取一条记录
            let mut bytes_read: u32 = 0;
            let result = ReadFile(
                self.volume_handle,
                buffer.as_mut_ptr() as *mut _,
                record_size as u32,
                &mut bytes_read,
                std::ptr::null_mut(),
            );

            if result == 0 || bytes_read == 0 {
                return Ok(None);
            }
        }

        // 应用 fixup 后解析记录
        apply_mft_fixup(&mut buffer);
        Ok(parse_mft_record_info(&buffer, frn))
    }

    /// 从 FRN 出发向上遍历 MFT 父链，解析到根的完整路径
    /// 返回相对于卷根的路径（如 "Users/xxx/Documents"），使用 "/" 分隔
    pub fn resolve_frn_path(&self, frn: u64) -> io::Result<Option<String>> {
        // FRN=5 始终是 NTFS 根目录
        const ROOT_FRN: u64 = 5;

        if frn == ROOT_FRN {
            return Ok(Some(String::new()));
        }

        let mut components: Vec<String> = Vec::new();
        let mut current_frn = frn;
        let mut depth = 0;
        const MAX_DEPTH: u32 = 64; // 防止死循环（损坏的 MFT）

        loop {
            if depth > MAX_DEPTH {
                return Ok(None);
            }
            depth += 1;

            if let Some(record) = self.read_single_record(current_frn)? {
                components.push(record.name);
                current_frn = record.parent_frn;

                if current_frn == ROOT_FRN {
                    break;
                }
            } else {
                // 记录读不到，可能是 FRN 序列号不匹配或记录已被释放
                return Ok(None);
            }
        }

        // 反转组件并拼接路径
        components.reverse();
        let path = components.join("/");
        Ok(Some(path))
    }
}

/// 解析单条 MFT 记录缓冲区，提取 $FILE_NAME 属性（简化版，供 FRN 解析使用）
fn parse_mft_record_info(data: &[u8], frn: u64) -> Option<MftRecordInfo> {
    let record_index = (frn & 0x0000FFFFFFFFFFFF) as usize;

    if data.len() < 48 {
        return None;
    }

    let signature = u32_from_le(&data[0..4]);
    if signature != MFT_FILE_SIGNATURE {
        return None;
    }

    let flags = u16_from_le(&data[0x16..0x18]);
    if flags & MFT_RECORD_IN_USE == 0 {
        return None;
    }

    let is_dir = flags & MFT_RECORD_IS_DIR != 0;

    let first_attr_offset = u16_from_le(&data[0x14..0x16]) as usize;
    if first_attr_offset >= data.len() || first_attr_offset < 48 {
        return None;
    }

    let mut attr_offset = first_attr_offset;
    let mut best_info: Option<MftRecordInfo> = None;
    let mut best_is_win32 = false;
    // 未命名 $DATA 的真实大小：$FILE_NAME.RealSize 对常驻/小文件经常是 0
    let mut data_size: u64 = 0;
    loop {
        if attr_offset + 16 > data.len() {
            break;
        }

        let attr_type = u32_from_le(&data[attr_offset..attr_offset + 4]);
        if attr_type == ATTR_END {
            break;
        }

        let attr_length = u32_from_le(&data[attr_offset + 4..attr_offset + 8]) as usize;
        if attr_length < 16 || attr_length == 0 {
            break;
        }

        let non_resident = data[attr_offset + 8];
        let attr_name_len = data[attr_offset + 9] as usize;

        if attr_type == ATTR_FILE_NAME && non_resident == 0 {
            let content_size =
                u32_from_le(&data[attr_offset + 0x10..attr_offset + 0x14]) as usize;
            let content_offset =
                u16_from_le(&data[attr_offset + 0x14..attr_offset + 0x16]) as usize;

            let fn_start = attr_offset + content_offset;
            let fn_end = fn_start + content_size;

            if fn_end > data.len() || content_size < 0x42 {
                attr_offset += attr_length;
                if attr_offset >= data.len() {
                    break;
                }
                continue;
            }

            let fn_data = &data[fn_start..fn_end];

            let parent_frn = u64_from_le(&fn_data[FN_PARENT_FRN..FN_PARENT_FRN + 8]) & 0x0000FFFFFFFFFFFF;
            let real_size = u64_from_le(&fn_data[FN_REAL_SIZE..FN_REAL_SIZE + 8]);
            let name_len = fn_data[FN_NAME_LENGTH] as usize;
            let name_start = FN_NAME_START;
            let name_end = name_start + name_len * 2;

            if name_end > content_size {
                attr_offset += attr_length;
                if attr_offset >= data.len() {
                    break;
                }
                continue;
            }

            let name = read_utf16le(&fn_data[name_start..name_end]);
            let name_type = fn_data[FN_NAME_NAMESPACE];
            let mtime = filetime_to_unix(u64_from_le(&fn_data[FN_MODIFY_TIME..FN_MODIFY_TIME + 8]));
            let atime = filetime_to_unix(u64_from_le(&fn_data[FN_ACCESS_TIME..FN_ACCESS_TIME + 8]));

            let info = MftRecordInfo {
                name,
                parent_frn,
                is_dir,
                real_size,
                mtime,
                atime,
            };

            // 1 = Win32, 3 = Win32+DOS：优先长名，且第一个长名优先
            let is_win32 = name_type == 1 || name_type == 3;
            let should_replace = match best_info {
                None => true,
                Some(_) => is_win32 && !best_is_win32,
            };
            if should_replace {
                best_info = Some(info);
                best_is_win32 = is_win32;
            }
        } else if attr_type == ATTR_DATA && attr_name_len == 0 {
            // 未命名 $DATA：resident 时 content length 即文件大小；
            // non-resident 时 DataSize 位于属性头 0x30 处
            let size = if non_resident == 0 {
                if attr_offset + 0x14 <= data.len() {
                    u32_from_le(&data[attr_offset + 0x10..attr_offset + 0x14]) as u64
                } else {
                    0
                }
            } else if attr_offset + 0x38 <= data.len() {
                u64_from_le(&data[attr_offset + 0x30..attr_offset + 0x38])
            } else {
                0
            };
            if size > data_size {
                data_size = size;
            }
        }

        attr_offset += attr_length;
        if attr_offset >= data.len() {
            break;
        }
    }

    if let Some(mut info) = best_info {
        // $FILE_NAME.RealSize 为 0 时用未命名 $DATA 的真实大小修正（目录不适用）
        if !info.is_dir && data_size > info.real_size {
            info.real_size = data_size;
        }
        return Some(info);
    }

    // 有记录但未找到 $FILE_NAME —— 返回一个占位名称
    if flags & MFT_RECORD_IN_USE != 0 {
        return Some(MftRecordInfo {
            name: format!("<record_{}>", record_index),
            parent_frn: 5,
            is_dir,
            real_size: data_size,
            mtime: 0,
            atime: 0,
        });
    }

    None
}

/// 解析单条 MFT 记录，提取 $FILE_NAME 属性
fn parse_mft_record(data: &[u8], record_index: usize) -> Option<MftEntry> {
    if data.len() < 48 {
        return None;
    }

    // 验证 "FILE" 签名
    let signature = u32_from_le(&data[0..4]);
    if signature != MFT_FILE_SIGNATURE {
        return None;
    }

    // 检查是否在使用中
    let flags = u16_from_le(&data[0x16..0x18]);
    if flags & MFT_RECORD_IN_USE == 0 {
        return None;
    }

    let is_dir = flags & MFT_RECORD_IS_DIR != 0;

    // 第一个属性的偏移
    let first_attr_offset = u16_from_le(&data[0x14..0x16]) as usize;
    if first_attr_offset >= data.len() || first_attr_offset < 48 {
        return None;
    }

    // 遍历属性，提取 $FILE_NAME（优先 Win32 长名）和未命名 $DATA 大小。
    // 某些文件（如 Chrome Code Cache）的 $FILE_NAME.RealSize 为 0，需要从 $DATA 读取真实大小。
    let mut attr_offset = first_attr_offset;
    let mut best_entry: Option<MftEntry> = None;
    let mut data_size: u64 = 0;

    loop {
        if attr_offset + 16 > data.len() {
            break;
        }

        let attr_type = u32_from_le(&data[attr_offset..attr_offset + 4]);
        if attr_type == ATTR_END {
            break;
        }

        let attr_length = u32_from_le(&data[attr_offset + 4..attr_offset + 8]) as usize;
        if attr_length < 16 || attr_length == 0 {
            break;
        }

        let non_resident = data[attr_offset + 8];
        let attr_name_len = data[attr_offset + 9] as usize;

        if attr_type == ATTR_FILE_NAME && non_resident == 0 {
            // 解析 resident $FILE_NAME 属性
            let content_size = u32_from_le(&data[attr_offset + 0x10..attr_offset + 0x14]) as usize;
            let content_offset = u16_from_le(&data[attr_offset + 0x14..attr_offset + 0x16]) as usize;

            let fn_start = attr_offset + content_offset;
            let fn_end = fn_start + content_size;

            if fn_end <= data.len() && content_size >= 0x42 {
                let fn_data = &data[fn_start..fn_end];

                // 提取父 FRN（只取低 48 位记录号，去掉高 16 位序列号）
                let parent_frn = u64_from_le(&fn_data[FN_PARENT_FRN..FN_PARENT_FRN + 8]) & 0x0000FFFFFFFFFFFF;

                // 提取实际文件大小
                let real_size = u64_from_le(&fn_data[FN_REAL_SIZE..FN_REAL_SIZE + 8]);

                // 提取文件属性标志
                let file_attrs = u32_from_le(&fn_data[FN_FLAGS..FN_FLAGS + 4]);
                let is_reparse = (file_attrs & FILE_ATTRIBUTE_REPARSE_POINT) != 0;

                // 提取修改时间 / 访问时间（FILETIME → Unix 秒）
                let mtime = filetime_to_unix(u64_from_le(&fn_data[FN_MODIFY_TIME..FN_MODIFY_TIME + 8]));
                let atime = filetime_to_unix(u64_from_le(&fn_data[FN_ACCESS_TIME..FN_ACCESS_TIME + 8]));

                // 提取文件名
                let name_len = fn_data[FN_NAME_LENGTH] as usize;
                let name_start = FN_NAME_START;
                let name_end = name_start + name_len * 2; // UTF-16LE

                if name_end <= content_size {
                    let name = read_utf16le(&fn_data[name_start..name_end]);
                    let name_type = fn_data[FN_NAME_NAMESPACE];

                    let entry = MftEntry {
                        name,
                        parent_frn,
                        real_size,
                        is_dir,
                        is_reparse,
                        mtime,
                        atime,
                    };

                    // 1 = Win32, 3 = Win32 + DOS；这两个都是长名，优先使用
                    if name_type == 1 || name_type == 3 {
                        best_entry = Some(entry);
                    } else if best_entry.is_none() {
                        best_entry = Some(entry);
                    }
                }
            }
        } else if attr_type == ATTR_DATA && attr_name_len == 0 {
            // 未命名 $DATA 属性：从中读取文件真实大小
            let size = if non_resident == 0 {
                // resident $DATA：content length 就是文件大小
                u32_from_le(&data[attr_offset + 0x10..attr_offset + 0x14]) as u64
            } else {
                // non-resident $DATA：DataSize 在属性头 0x30 偏移处
                u64_from_le(&data[attr_offset + 0x30..attr_offset + 0x38])
            };
            if size > data_size {
                data_size = size;
            }
        }

        // 移动到下一个属性
        attr_offset += attr_length;
        if attr_offset >= data.len() {
            break;
        }
    }

    if let Some(mut entry) = best_entry {
        if data_size > entry.real_size {
            entry.real_size = data_size;
        }
        return Some(entry);
    }

    // $FILE_NAME 未找到但有记录（可能是系统文件/元数据文件），返回最小信息
    if flags & MFT_RECORD_IN_USE != 0 {
        return Some(MftEntry {
            name: format!("<record_{}>", record_index),
            parent_frn: 5, // 挂到根目录
            real_size: data_size,
            is_dir,
            is_reparse: false,
            mtime: 0,
            atime: 0,
        });
    }

    None
}

// ─── 辅助函数 ──────────────────────────────────────────────

#[inline]
fn u16_from_le(data: &[u8]) -> u16 {
    u16::from_le_bytes([data[0], data[1]])
}

#[inline]
fn u32_from_le(data: &[u8]) -> u32 {
    u32::from_le_bytes([data[0], data[1], data[2], data[3]])
}

#[inline]
fn u64_from_le(data: &[u8]) -> u64 {
    u64::from_le_bytes([
        data[0], data[1], data[2], data[3],
        data[4], data[5], data[6], data[7],
    ])
}

/// 解析 NTFS data run 列表。
/// data 指向 $DATA 属性的 mapping pairs 区域，以 0x00 结束。
/// 返回 [(start_lcn, cluster_count)]，start_lcn 已转换为绝对 LCN。
fn parse_data_runs(data: &[u8]) -> Vec<(u64, u64)> {
    let mut runs = Vec::new();
    let mut pos = 0usize;
    let mut last_lcn: i64 = 0;

    loop {
        if pos >= data.len() {
            break;
        }
        let header = data[pos];
        if header == 0 {
            break;
        }

        let count_len = (header & 0x0F) as usize;
        let lcn_len = ((header >> 4) & 0x0F) as usize;
        pos += 1;

        if count_len == 0 || pos + count_len + lcn_len > data.len() {
            break;
        }

        let count = read_signed_or_unsigned(&data[pos..pos + count_len], false) as u64;
        pos += count_len;

        let lcn_delta = if lcn_len > 0 {
            read_signed_or_unsigned(&data[pos..pos + lcn_len], true) as i64
        } else {
            0i64
        };
        pos += lcn_len;

        last_lcn += lcn_delta;
        runs.push((last_lcn as u64, count));
    }

    runs
}

/// 读取小端整数，可选有符号。
fn read_signed_or_unsigned(data: &[u8], signed: bool) -> i64 {
    let mut buf = [0u8; 8];
    buf[..data.len()].copy_from_slice(data);
    let val = u64::from_le_bytes(buf);

    if !signed {
        return val as i64;
    }

    // 符号扩展
    let shift = (8 - data.len()) * 8;
    ((val << shift) as i64) >> shift
}

/// 从 UTF-16LE 字节读取字符串
fn read_utf16le(data: &[u8]) -> String {
    let u16_slice: Vec<u16> = data
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();
    String::from_utf16_lossy(&u16_slice)
}

/// 应用 MFT 记录 fixup（update sequence）。
/// NTFS 将每个 512 字节扇区最后 2 字节替换为 fixup signature；
/// 本函数从记录头读取 fixup array 并恢复原始值。返回 false 表示校验失败。
fn apply_mft_fixup(data: &mut [u8]) -> bool {
    if data.len() < 0x34 {
        return false;
    }

    let update_sequence_offset = u16_from_le(&data[0x04..0x06]) as usize;
    let update_sequence_count = u16_from_le(&data[0x06..0x08]) as usize;

    if update_sequence_offset < 0x30
        || update_sequence_offset + update_sequence_count * 2 > data.len()
        || update_sequence_count == 0
    {
        return false;
    }

    let signature = u16_from_le(&data[update_sequence_offset..update_sequence_offset + 2]);
    let sectors = update_sequence_count.saturating_sub(1);
    if sectors == 0 {
        return true;
    }

    let sector_size = 512usize;
    for i in 0..sectors {
        let pos = (i + 1) * sector_size - 2;
        if pos + 2 > data.len() {
            return false;
        }
        let current = u16_from_le(&data[pos..pos + 2]);
        if current != signature {
            // fixup signature 不匹配，记录可能损坏；但仍尝试恢复
            return false;
        }
        let original_offset = update_sequence_offset + 2 + i * 2;
        let original = u16_from_le(&data[original_offset..original_offset + 2]);
        data[pos..pos + 2].copy_from_slice(&original.to_le_bytes());
    }

    true
}

// ─── 公共接口 ──────────────────────────────────────────────

/// 检测当前进程是否以管理员/提升权限运行（Windows）
#[cfg(target_os = "windows")]
pub fn is_admin() -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Security::{GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY};
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    unsafe {
        let mut token: isize = INVALID_HANDLE_VALUE;
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return false;
        }

        let mut elevation: TOKEN_ELEVATION = std::mem::zeroed();
        let mut return_length: u32 = 0;
        let buffer_size = std::mem::size_of::<TOKEN_ELEVATION>() as u32;

        let ok = GetTokenInformation(
            token,
            TokenElevation,
            &mut elevation as *mut _ as *mut _,
            buffer_size,
            &mut return_length,
        );

        CloseHandle(token);

        if ok == 0 {
            return false;
        }

        elevation.TokenIsElevated != 0
    }
}

/// 非 Windows 平台始终返回 false
#[cfg(not(target_os = "windows"))]
pub fn is_admin() -> bool {
    false
}

/// 快速检测 MFT 扫描是否可用（仅尝试打开卷，不读取数据）
pub fn check_mft_available(path: &str) -> bool {
    let drive_letter = match extract_drive_letter(path) {
        Some(d) => d,
        None => return false,
    };

    let volume_path = format!(r"\\.\{}:", drive_letter);
    let wide_path: Vec<u16> = volume_path
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();

    unsafe {
        let handle = CreateFileW(
            wide_path.as_ptr(),
            GENERIC_READ,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            std::ptr::null_mut(),
            OPEN_EXISTING,
            0,
            0,
        );

        if handle == INVALID_HANDLE_VALUE {
            return false;
        }
        CloseHandle(handle);
    }

    true
}

/// 以管理员权限重启当前应用（Windows ShellExecute runas）
pub fn restart_as_admin() -> bool {
    let exe_path = match std::env::current_exe() {
        Ok(p) => p,
        Err(_) => return false,
    };

    let exe_str = match exe_path.to_str() {
        Some(s) => s,
        None => return false,
    };

    let wide_exe: Vec<u16> = exe_str
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let wide_verb: Vec<u16> = "runas"
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();

    unsafe {
        use windows_sys::Win32::UI::Shell::ShellExecuteW;
        use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOW;

        let result = ShellExecuteW(
            0,                              // hwnd
            wide_verb.as_ptr(),            // lpOperation = "runas"
            wide_exe.as_ptr(),             // lpFile
            std::ptr::null(),              // lpParameters
            std::ptr::null(),              // lpDirectory
            SW_SHOW,                        // nShowCmd
        );

        // ShellExecuteW returns >32 on success
        result > 32
    }
}

/// 尝试使用 MFT 直接扫描（Windows 管理员权限下）
/// 失败时返回 None，调用者应回退到目录遍历方式
pub fn try_mft_scan(root_path: &str) -> Option<MftScanResult> {
    let scan_id = crate::cancel::begin();
    try_mft_scan_with_cancel(root_path, scan_id)
}

/// 与 `try_mft_scan` 相同，但复用调用方已有的取消代号：
/// 避免为子步骤新开一代，从而不会清掉/覆盖在飞扫描的取消状态。
pub fn try_mft_scan_with_cancel(root_path: &str, cancel_id: u64) -> Option<MftScanResult> {
    // 提取盘符
    let drive_letter = extract_drive_letter(root_path)?;

    let mut scanner = match MftScanner::open(drive_letter) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("[MFT] 无法打开卷 {}: {}", drive_letter, e);
            return None;
        }
    };
    scanner.set_cancel_id(cancel_id);

    match scanner.scan() {
        Ok(result) => {
            eprintln!(
                "[MFT] 扫描完成: {} 文件, {} 目录, {:.1}MB MFT 数据 (读取+解析 {:.2}s, 建路径 {:.2}s)",
                result.file_count,
                result.dir_count,
                result.data_read as f64 / 1024.0 / 1024.0,
                result.read_secs,
                result.path_secs
            );
            Some(result)
        }
        Err(e) => {
            eprintln!("[MFT] 扫描失败: {}", e);
            None
        }
    }
}

/// 从路径中提取盘符，如 "C:\Users" → 'C'
/// 从路径中提取盘符，如 "C:\\Users" -> 'C'，支持 canonicalize 产生的 \\?\ 前缀。
pub(crate) fn extract_drive_letter(path: &str) -> Option<char> {
    let path = path.trim();
    let normalized = path.replace('\\', "/");
    // 处理 canonicalize 产生的 \\?\C:\ 前缀（标准化后为 //?/C:/）
    let trimmed = if normalized.starts_with("//?/") {
        &normalized[4..]
    } else {
        normalized.as_str()
    };

    if trimmed.len() >= 2 {
        let bytes = trimmed.as_bytes();
        if bytes[1] == b':' && bytes[0].is_ascii_alphabetic() {
            return Some(bytes[0] as char);
        }
    }
    // 尝试解析为绝对路径
    let pb = PathBuf::from(trimmed);
    if let Some(ancestor) = pb.ancestors().last() {
        if let Some(s) = ancestor.to_str() {
            let bytes = s.as_bytes();
            if s.len() >= 2 && bytes[1] == b':' && bytes[0].is_ascii_alphabetic() {
                return Some(bytes[0] as char);
            }
        }
    }
    None
}

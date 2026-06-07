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

use std::collections::HashMap;
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
}

/// FRN → MftEntry 的索引（FRN 去掉序列号的高位作为 key）
/// FRN 结构: [48-bit record number][16-bit sequence number]
type MftIndex = HashMap<u64, MftEntry>;

/// MFT 扫描器 —— 打开 NTFS 卷并顺序读取 $MFT
pub struct MftScanner {
    volume_handle: isize,
    bytes_per_cluster: u64,
    mft_start_offset: u64,
    mft_valid_size: u64,
    mft_record_size: u32,
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
}

/// 单个文件的 MFT 信息
#[derive(Debug, Clone)]
pub struct MftFileInfo {
    pub path: String,
    pub name: String,
    pub size: u64,
    pub is_dir: bool,
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

            Ok(Self {
                volume_handle: handle,
                bytes_per_cluster: vol_data.bytes_per_cluster as u64,
                mft_start_offset,
                mft_valid_size,
                mft_record_size: vol_data.bytes_per_file_record_segment,
            })
        }
    }

    /// 执行 MFT 扫描，返回所有文件的元数据
    pub fn scan(&self) -> io::Result<MftScanResult> {
        // 第一步：读取所有 MFT 记录，建立 FRN → MftEntry 索引
        let index = self.read_all_records()?;

        // 计数
        let mut file_count = 0usize;
        let mut dir_count = 0usize;
        for entry in index.values() {
            if entry.is_dir {
                dir_count += 1;
            } else {
                file_count += 1;
            }
        }

        // 第二步：构建父子关系树，DFS 解析完整路径
        let files = Self::build_path_hierarchy(&index);

        Ok(MftScanResult {
            files,
            file_count,
            dir_count,
            data_read: self.mft_valid_size,
        })
    }

    /// 顺序读取所有 $MFT FILE 记录，解析 $FILE_NAME 属性
    fn read_all_records(&self) -> io::Result<MftIndex> {
        let record_size = self.mft_record_size as usize;
        // MFT 中有一些预留区域（前 16-24 条是系统记录，后面的才是用户文件）
        // 实际记录数 = mft_valid_size / record_size
        let max_records = (self.mft_valid_size as usize) / record_size;
        let mut index: MftIndex = HashMap::with_capacity(max_records);

        // 分配读取缓冲区：一次读 256 条记录（256KB）
        let batch_records = 256usize;
        let batch_size = batch_records * record_size;
        let mut buffer: Vec<u8> = vec![0u8; batch_size];

        let mut total_bytes_read = 0u64;
        let chunk_start = self.mft_start_offset;

        unsafe {
            for batch_start in (0..max_records).step_by(batch_records) {
                let records_in_batch = (max_records - batch_start).min(batch_records);
                let read_size = records_in_batch * record_size;

                // Seek 到对应 MFT 位置
                let seek_offset = chunk_start + (batch_start * record_size) as u64;
                let mut distance_to_move: i64 = seek_offset as i64;
                let result = SetFilePointerEx(
                    self.volume_handle,
                    seek_offset as i64,
                    &mut distance_to_move,
                    0, // FILE_BEGIN
                );

                if result == 0 {
                    // Seek 失败，跳过这一批
                    continue;
                }

                // 读取一批 MFT 记录
                let mut bytes_read: u32 = 0;
                let result = ReadFile(
                    self.volume_handle,
                    buffer.as_mut_ptr() as *mut _,
                    read_size as u32,
                    &mut bytes_read,
                    std::ptr::null_mut(),
                );

                if result == 0 || bytes_read == 0 {
                    // 读取失败或 EOF
                    if total_bytes_read >= self.mft_valid_size {
                        break;
                    }
                    continue;
                }

                total_bytes_read += bytes_read as u64;

                // 解析这一批中的每一条记录
                for i in 0..records_in_batch {
                    let record_offset = i * record_size;
                    if record_offset + record_size > bytes_read as usize {
                        break;
                    }
                    let record_data = &buffer[record_offset..record_offset + record_size];

                    if let Some(entry) = parse_mft_record(record_data, batch_start + i) {
                        let frn = (batch_start + i) as u64; // 简化：用记录号作为 FRN
                        index.insert(frn, entry);
                    }
                }
            }
        }

        Ok(index)
    }

    /// 构建完整路径（优化版：先建树，再 DFS）
    pub(crate) fn build_path_hierarchy(index: &MftIndex) -> Vec<MftFileInfo> {
        // 第一步：构建 parent_frn → children 的映射
        let mut children_map: HashMap<u64, Vec<u64>> = HashMap::new();
        for (&frn, entry) in index.iter() {
            children_map
                .entry(entry.parent_frn)
                .or_default()
                .push(frn);
        }

        // 第二步：从根目录 (FRN=5) 开始 DFS
        let mut files = Vec::with_capacity(index.len());
        let root_frn = 5u64;
        Self::dfs_resolve(root_frn, String::new(), index, &children_map, &mut files);
        files
    }

    fn dfs_resolve(
        frn: u64,
        current_path: String,
        index: &MftIndex,
        children_map: &HashMap<u64, Vec<u64>>,
        files: &mut Vec<MftFileInfo>,
    ) {
        if let Some(entry) = index.get(&frn) {
            let full_path = if current_path.is_empty() {
                entry.name.clone()
            } else {
                format!("{}/{}", current_path, entry.name)
            };

            // 文件和目录都加入输出（目录的 size=0，后续聚合计算）
            files.push(MftFileInfo {
                path: full_path.clone(),
                name: entry.name.clone(),
                size: entry.real_size,
                is_dir: entry.is_dir,
            });

            // 递归处理子节点
            if let Some(children) = children_map.get(&frn) {
                for &child_frn in children {
                    Self::dfs_resolve(
                        child_frn,
                        if entry.is_dir {
                            full_path.clone()
                        } else {
                            current_path.clone()
                        },
                        index,
                        children_map,
                        files,
                    );
                }
            }
        }
    }
}

impl Drop for MftScanner {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.volume_handle);
        }
    }
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

    // 遍历属性，找到 $FILE_NAME (0x30)
    let mut attr_offset = first_attr_offset;
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

        if attr_type == ATTR_FILE_NAME && non_resident == 0 {
            // 解析 resident $FILE_NAME 属性
            let content_size = u32_from_le(&data[attr_offset + 0x10..attr_offset + 0x14]) as usize;
            let content_offset = u16_from_le(&data[attr_offset + 0x14..attr_offset + 0x16]) as usize;

            let fn_start = attr_offset + content_offset;
            let fn_end = fn_start + content_size;

            if fn_end > data.len() || content_size < 0x42 {
                break;
            }

            let fn_data = &data[fn_start..fn_end];

            // 提取父 FRN
            let parent_frn = u64_from_le(&fn_data[FN_PARENT_FRN..FN_PARENT_FRN + 8]);

            // 提取实际文件大小
            let real_size = u64_from_le(&fn_data[FN_REAL_SIZE..FN_REAL_SIZE + 8]);

            // 提取文件属性标志
            let file_attrs = u32_from_le(&fn_data[FN_FLAGS..FN_FLAGS + 4]);
            let is_reparse = (file_attrs & FILE_ATTRIBUTE_REPARSE_POINT) != 0;

            // 提取文件名
            let name_len = fn_data[FN_NAME_LENGTH] as usize;
            let name_start = FN_NAME_START;
            let name_end = name_start + name_len * 2; // UTF-16LE

            if name_end > content_size {
                break;
            }

            let name = read_utf16le(&fn_data[name_start..name_end]);

            return Some(MftEntry {
                name,
                parent_frn,
                real_size,
                is_dir,
                is_reparse,
            });
        }

        // 移动到下一个属性
        attr_offset += attr_length;
        if attr_offset >= data.len() {
            break;
        }
    }

    // $FILE_NAME 未找到但有记录（可能是系统文件/元数据文件），返回最小信息
    if flags & MFT_RECORD_IN_USE != 0 {
        return Some(MftEntry {
            name: format!("<record_{}>", record_index),
            parent_frn: 5, // 挂到根目录
            real_size: 0,
            is_dir,
            is_reparse: false,
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

/// 从 UTF-16LE 字节读取字符串
fn read_utf16le(data: &[u8]) -> String {
    let u16_slice: Vec<u16> = data
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();
    String::from_utf16_lossy(&u16_slice)
}

// ─── 公共接口 ──────────────────────────────────────────────

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
    // 提取盘符
    let drive_letter = extract_drive_letter(root_path)?;

    let scanner = match MftScanner::open(drive_letter) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("[MFT] 无法打开卷 {}: {}", drive_letter, e);
            return None;
        }
    };

    match scanner.scan() {
        Ok(result) => {
            eprintln!(
                "[MFT] 扫描完成: {} 文件, {} 目录, {:.1}MB MFT 数据",
                result.file_count,
                result.dir_count,
                result.data_read as f64 / 1024.0 / 1024.0
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
pub(crate) fn extract_drive_letter(path: &str) -> Option<char> {
    let path = path.trim();
    if path.len() >= 2 {
        let bytes = path.as_bytes();
        if bytes[1] == b':' && bytes[0].is_ascii_alphabetic() {
            return Some(bytes[0] as char);
        }
    }
    // 尝试解析为绝对路径
    let pb = PathBuf::from(path);
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

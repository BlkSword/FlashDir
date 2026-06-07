// USN Journal (Update Sequence Number Journal) 增量更新
//
// NTFS 的 USN Journal 记录了所有文件变更操作（创建/删除/重命名/修改），
// 每条记录包含文件引用号、父目录引用号、文件名、变更原因码、USN 号。
//
// 工作原理：
// 1. 首次全量扫描（MFT）后，记录此时的 max USN 作为 checkpoint
// 2. 下次扫描时，只读取 USN Journal 中 checkpoint 之后的增量记录
// 3. 根据变更原因码（CREATE/DELETE/RENAME/DATA_CHANGE）更新缓存
// 4. 配合 disk_cache 实现近乎即时的「重新扫描」
//
// 这就是 Everything 能在文件变更后秒级刷新索引的核心技术。

use std::io;
use std::mem;

use serde::{Deserialize, Serialize};
use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, GENERIC_READ, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows_sys::Win32::System::IO::DeviceIoControl;

// ─── FSCTL 常量 ────────────────────────────────────────────

/// FSCTL_QUERY_USN_JOURNAL
const FSCTL_QUERY_USN_JOURNAL: u32 = 0x000900F4;

/// FSCTL_READ_USN_JOURNAL (METHOD_NEITHER)
const FSCTL_READ_USN_JOURNAL: u32 = 0x000900B8;

// ─── USN 原因码 ─────────────────────────────────────────────

/// 文件数据被覆盖写入
pub const USN_REASON_DATA_OVERWRITE: u32 = 0x00000001;
/// 文件数据被扩展
pub const USN_REASON_DATA_EXTEND: u32 = 0x00000002;
/// 文件数据被截断
pub const USN_REASON_DATA_TRUNCATION: u32 = 0x00000004;
/// 文件被创建
pub const USN_REASON_FILE_CREATE: u32 = 0x00000100;
/// 文件被删除
pub const USN_REASON_FILE_DELETE: u32 = 0x00000200;
/// 文件被重命名（旧名称）
pub const USN_REASON_RENAME_OLD_NAME: u32 = 0x00001000;
/// 文件被重命名（新名称）
pub const USN_REASON_RENAME_NEW_NAME: u32 = 0x00002000;
/// 文件基本属性变更
pub const USN_REASON_BASIC_INFO_CHANGE: u32 = 0x00008000;
/// 关闭句柄（通常与上述原因组合使用）
pub const USN_REASON_CLOSE: u32 = 0x80000000;

// ─── USN 数据结构 ───────────────────────────────────────────

/// USN_JOURNAL_DATA — 查询 USN Journal 状态
#[repr(C)]
struct UsnJournalData {
    usn_journal_id: u64,
    first_usn: i64,
    next_usn: i64,
    lowest_valid_usn: i64,
    max_usn: i64,
    maximum_size: u64,
    allocation_delta: u64,
}

/// READ_USN_JOURNAL_DATA — 读取 USN Journal
#[repr(C)]
struct ReadUsnJournalData {
    start_usn: i64,
    reason_mask: u32,
    return_only_on_close: u32,
    timeout: u64,
    bytes_to_wait_for: u64,
    usn_journal_id: u64,
}

/// USN_RECORD 头部（可变长度，以 FileName 结尾）
#[repr(C)]
struct UsnRecordHeader {
    record_length: u32,
    major_version: u16,
    minor_version: u16,
    file_reference_number: u64,
    parent_file_reference_number: u64,
    usn: i64,
    timestamp: i64,
    reason: u32,
    source_info: u32,
    security_id: u32,
    file_attributes: u32,
    file_name_length: u16,
    file_name_offset: u16,
    // file_name: [u16; file_name_length] follows
}

/// 解析后的 USN 变更记录
#[derive(Debug, Clone)]
pub struct UsnChangeRecord {
    /// 文件引用号
    pub file_ref: u64,
    /// 父目录引用号
    pub parent_ref: u64,
    /// 文件名
    pub name: String,
    /// 变更原因码
    pub reason: u32,
    /// USN 编号
    pub usn: i64,
    /// 时间戳 (Windows FILETIME)
    pub timestamp: i64,
    /// 文件属性
    pub attributes: u32,
}

// ─── USN Checkpoint ─────────────────────────────────────────

/// USN 检查点 —— 保存在磁盘上，用于增量更新
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsnCheckpoint {
    /// NTFS 卷序列号
    pub volume_serial: u64,
    /// USN Journal ID（检测 Journal 是否被重置）
    pub journal_id: u64,
    /// 上次扫描时的 max USN
    pub max_usn: i64,
    /// 检查点创建时间
    pub created_at: i64,
}

/// USN Journal 操作句柄
pub struct UsnJournal {
    volume_handle: isize,
    drive_letter: char,
}

impl UsnJournal {
    /// 打开卷的 USN Journal
    pub fn open(drive_letter: char) -> io::Result<Self> {
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
                return Err(io::Error::from_raw_os_error(GetLastError() as i32));
            }

            Ok(Self {
                volume_handle: handle,
                drive_letter,
            })
        }
    }

    /// 查询 USN Journal 当前状态
    pub fn query_journal(&self) -> io::Result<UsnJournalData> {
        unsafe {
            let mut journal_data: UsnJournalData = mem::zeroed();
            let mut bytes_returned: u32 = 0;

            let result = DeviceIoControl(
                self.volume_handle,
                FSCTL_QUERY_USN_JOURNAL,
                std::ptr::null_mut(),
                0,
                &mut journal_data as *mut _ as *mut _,
                mem::size_of::<UsnJournalData>() as u32,
                &mut bytes_returned,
                std::ptr::null_mut(),
            );

            if result == 0 {
                return Err(io::Error::from_raw_os_error(GetLastError() as i32));
            }

            Ok(journal_data)
        }
    }

    /// 读取 USN Journal 中指定 USN 之后的所有变更记录
    pub fn read_changes_since(
        &self,
        start_usn: i64,
        journal_id: u64,
        max_records: usize,
    ) -> io::Result<Vec<UsnChangeRecord>> {
        let mut read_data = ReadUsnJournalData {
            start_usn,
            reason_mask: 0xFFFFFFFF, // 所有变更类型
            return_only_on_close: 0,  // 返回所有记录，不仅仅是关闭的
            timeout: 0,               // 不等待
            bytes_to_wait_for: 0,
            usn_journal_id: journal_id,
        };

        // 为 USN 记录分配缓冲区（每条记录最大约 512 字节）
        let buffer_size = (max_records * 512).min(4 * 1024 * 1024); // 最多 4MB
        let mut buffer: Vec<u8> = vec![0u8; buffer_size];

        unsafe {
            let mut bytes_returned: u32 = 0;

            let result = DeviceIoControl(
                self.volume_handle,
                FSCTL_READ_USN_JOURNAL,
                &mut read_data as *mut _ as *mut _,
                mem::size_of::<ReadUsnJournalData>() as u32,
                buffer.as_mut_ptr() as *mut _,
                buffer_size as u32,
                &mut bytes_returned,
                std::ptr::null_mut(),
            );

            if result == 0 {
                let err = GetLastError();
                // ERROR_HANDLE_EOF (38) = 没有更多记录，这是正常的
                if err == 38 {
                    return Ok(Vec::new());
                }
                return Err(io::Error::from_raw_os_error(err as i32));
            }

            // 解析返回的 USN 记录
            let mut records = Vec::new();
            let mut offset = 0usize;

            while offset + mem::size_of::<UsnRecordHeader>() <= bytes_returned as usize {
                let header = &*(buffer.as_ptr().add(offset) as *const UsnRecordHeader);

                if header.record_length == 0 || header.record_length as usize > buffer_size - offset {
                    break;
                }

                // 提取文件名
                let name_offset = header.file_name_offset as usize;
                let name_len = header.file_name_length as usize;
                let name_bytes = &buffer[offset + name_offset..offset + name_offset + name_len];

                let u16_slice: Vec<u16> = name_bytes
                    .chunks_exact(2)
                    .map(|c| u16::from_le_bytes([c[0], c[1]]))
                    .collect();

                let name = String::from_utf16_lossy(&u16_slice);

                records.push(UsnChangeRecord {
                    file_ref: header.file_reference_number,
                    parent_ref: header.parent_file_reference_number,
                    name,
                    reason: header.reason,
                    usn: header.usn,
                    timestamp: header.timestamp,
                    attributes: header.file_attributes,
                });

                offset += header.record_length as usize;
            }

            Ok(records)
        }
    }

    /// 创建检查点（基于当前 USN Journal 状态）
    pub fn create_checkpoint(&self, volume_serial: u64) -> io::Result<UsnCheckpoint> {
        let journal = self.query_journal()?;

        Ok(UsnCheckpoint {
            volume_serial,
            journal_id: journal.usn_journal_id,
            max_usn: journal.max_usn,
            created_at: chrono::Utc::now().timestamp(),
        })
    }
}

impl Drop for UsnJournal {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.volume_handle);
        }
    }
}

// ─── 高级接口 ──────────────────────────────────────────────

/// 获取盘符对应卷的 USN 检查点
pub fn get_checkpoint(drive_letter: char) -> Option<UsnCheckpoint> {
    let journal = UsnJournal::open(drive_letter).ok()?;
    let vol_serial = get_volume_serial(drive_letter)?;
    journal.create_checkpoint(vol_serial).ok()
}

/// 从检查点读取增量变更
/// 返回 (变更记录列表, 新的检查点)
pub fn read_incremental_changes(
    drive_letter: char,
    checkpoint: &UsnCheckpoint,
) -> io::Result<(Vec<UsnChangeRecord>, UsnCheckpoint)> {
    let journal = UsnJournal::open(drive_letter)?;

    // 验证 Journal ID 未变（Journal 未被重置）
    let current = journal.query_journal()?;
    if current.usn_journal_id != checkpoint.journal_id {
        // Journal 被重置了，需要全量扫描
        return Err(io::Error::new(
            io::ErrorKind::Other,
            "USN Journal was reset, full scan required",
        ));
    }

    // 读取增量变更
    let changes = journal.read_changes_since(
        checkpoint.max_usn,
        checkpoint.journal_id,
        100_000, // 最多处理 10 万条变更
    )?;

    // 创建新检查点
    let vol_serial = get_volume_serial(drive_letter)
        .ok_or_else(|| io::Error::new(io::ErrorKind::Other, "Cannot get volume serial"))?;

    let new_checkpoint = journal.create_checkpoint(vol_serial)?;

    Ok((changes, new_checkpoint))
}

/// 获取 NTFS 卷序列号（Volume Serial Number）
fn get_volume_serial(drive_letter: char) -> Option<u64> {
    let root = format!("{}:\\", drive_letter);

    use windows_sys::Win32::Storage::FileSystem::{
        GetVolumeInformationW,
    };

    let wide_root: Vec<u16> = root
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();

    unsafe {
        let mut serial: u32 = 0;
        let result = GetVolumeInformationW(
            wide_root.as_ptr(),
            std::ptr::null_mut(),
            0,
            &mut serial,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            0,
        );

        if result == 0 {
            return None;
        }

        Some(serial as u64)
    }
}

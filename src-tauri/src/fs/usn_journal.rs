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
///
/// CTL_CODE(FILE_DEVICE_FILE_SYSTEM=0x0009, 46, METHOD_NEITHER=3, FILE_ANY_ACCESS=0)
/// = 0x00090000 | (46 << 2) | 3 = 0x000900BB。
/// 早期写成 0x000900B8（漏掉 METHOD_NEITHER 位），驱动会直接返回
/// ERROR_INVALID_FUNCTION，USN 增量整条链路都不可用。
const FSCTL_READ_USN_JOURNAL: u32 = 0x000900BB;

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
///
/// 注意：`max_usn` 是"该 Journal 能容纳的 USN 上限常量"（NTFS 上恒为
/// 2^63 - 2^16），**不是当前写入位置**。当前可用的位置是 `next_usn`
/// （下一个将被分配的 USN）。早期实现把 `max_usn` 当位置保存到检查点，
/// 结果 READ_USN_JOURNAL 直接返回 ERROR_INVALID_FUNCTION。
#[repr(C)]
#[allow(dead_code)]
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

/// USN_RECORD_V2 固定头部长度（FileName 之前的字节数 = sizeof(USN_RECORD_V2) - 2）
const USN_V2_FIXED_SIZE: usize = 60;

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
    /// 检查点位置：下一个将被分配的 USN（`USN_JOURNAL_DATA.NextUsn`）。
    /// 兼容旧版本 JSON 里的 `max_usn` 字段名（旧值语义错误，会被范围校验判为失效）。
    #[serde(alias = "max_usn")]
    pub next_usn: i64,
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

    /// 读取 USN Journal 中指定 USN 之后的变更记录（单批，最多 max_records 条）。
    ///
    /// 关键点：`FSCTL_READ_USN_JOURNAL` 的输出缓冲区以 8 字节 `USN`（下次读取起点）
    /// 开头，其后才是 `USN_RECORD` 数组 —— 见 winioctl.h 中的宏注释
    /// `FSCTL_READ_USN_JOURNAL ... // READ_USN_JOURNAL_DATA, USN`。
    /// 早期实现从 offset 0 开始解析，把 next-USN 的低 32 位当成 RecordLength，
    /// 导致增量更新被静默跳过，或按错位字段解析出垃圾记录。
    ///
    /// 返回 `(变更记录, 下次读取起点)`。
    pub fn read_changes_since(
        &self,
        start_usn: i64,
        journal_id: u64,
        max_records: usize,
    ) -> io::Result<(Vec<UsnChangeRecord>, i64)> {
        let mut read_data = ReadUsnJournalData {
            start_usn,
            reason_mask: 0xFFFFFFFF, // 所有变更类型
            return_only_on_close: 0,  // 返回所有记录，不仅仅是关闭的
            timeout: 0,               // 不等待
            bytes_to_wait_for: 0,
            usn_journal_id: journal_id,
        };

        // 为 USN 记录分配缓冲区（每条记录最大约 512 字节）
        let max_records = max_records.max(1);
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
                    return Ok((Vec::new(), start_usn));
                }
                return Err(io::Error::from_raw_os_error(err as i32));
            }

            let returned = bytes_returned as usize;
            // 前 8 字节为下次读取起点（可能只有这 8 字节，表示无新记录）
            if returned < 8 {
                return Ok((Vec::new(), start_usn));
            }
            let next_usn = i64::from_le_bytes([
                buffer[0], buffer[1], buffer[2], buffer[3],
                buffer[4], buffer[5], buffer[6], buffer[7],
            ]);

            // 解析返回的 USN 记录（从 offset 8 开始）
            let mut records = Vec::new();
            let mut offset = 8usize;

            while offset + USN_V2_FIXED_SIZE <= returned {
                let header = &*(buffer.as_ptr().add(offset) as *const UsnRecordHeader);

                let record_len = header.record_length as usize;
                if record_len < USN_V2_FIXED_SIZE || offset + record_len > returned {
                    break;
                }
                // 只支持 USN_RECORD_V2（输入结构体为 V0，驱动固定返回 V2 记录）
                if header.major_version != 2 {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("不支持的 USN 记录版本: {}", header.major_version),
                    ));
                }

                // 提取文件名（严格边界校验，避免脏日志越界 panic）
                let name_offset = header.file_name_offset as usize;
                let name_len = header.file_name_length as usize;
                if name_offset < USN_V2_FIXED_SIZE
                    || name_len % 2 != 0
                    || name_offset + name_len > record_len
                {
                    offset += record_len;
                    continue;
                }
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

                offset += record_len;
            }

            Ok((records, next_usn))
        }
    }

    /// 创建检查点（基于当前 USN Journal 状态）
    /// Journal 的可读窗口与当前位置：`(lowest_valid_usn, next_usn)`。
    /// 索引增量同步用它判断"积压多少条变更、还值不值得追赶"。
    pub fn window(&self) -> io::Result<(i64, i64)> {
        let data = self.query_journal()?;
        Ok((data.lowest_valid_usn, data.next_usn))
    }

    pub fn create_checkpoint(&self, volume_serial: u64) -> io::Result<UsnCheckpoint> {
        let journal = self.query_journal()?;

        Ok(UsnCheckpoint {
            volume_serial,
            journal_id: journal.usn_journal_id,
            next_usn: journal.next_usn,
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

/// 单次增量检查允许累积的最大变更数。
///
/// 实测（C 盘、NVMe）：增量应用约 **1.67ms/条**（每条变更要随机读一次 MFT 记录
/// 解析 FRN→路径，外加最终整块重写 blob 缓存），而全量 MFT 扫描只要 2.4–3.5s。
/// 因此约 2000 条变更时两者持平；这里取 1200 留出余量，超过就直接全量扫描，
/// 既更快也更稳（避免大量随机读把磁盘打满）。
pub const MAX_USN_CHANGES: usize = 1200;

/// 一次 USN 增量读取的结果
#[derive(Debug, Clone)]
pub struct UsnDelta {
    /// 变更记录（最多 MAX_USN_CHANGES + 1 条）
    pub changes: Vec<UsnChangeRecord>,
    /// 已消费到的位置：下次从此 USN 继续读取
    pub next_usn: i64,
    /// Journal 中仍可读取的最早 USN；低于它说明增量窗口已失效
    pub lowest_valid_usn: i64,
}

/// USN 增量读取失败原因
#[derive(Debug)]
pub enum UsnReadError {
    /// Journal 被重建：增量不可用，必须全量扫描
    JournalReset,
    /// 盘符指向了另一块磁盘：增量不可用，必须全量扫描
    VolumeChanged,
    /// 校验点落在 Journal 可读范围之外（回滚、或历史版本写入的错误位置）
    WindowExpired,
    /// 其它 I/O 错误（可稍后重试）
    Io(io::Error),
}

impl std::fmt::Display for UsnReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            UsnReadError::JournalReset => f.write_str("USN Journal 已被重置，需要全量扫描"),
            UsnReadError::VolumeChanged => f.write_str("卷序列号变化，需要全量扫描"),
            UsnReadError::WindowExpired => {
                f.write_str("校验点已超出 Journal 可读范围，需要全量扫描")
            }
            UsnReadError::Io(e) => {
                f.write_str("读取 USN Journal 失败: ")?;
                std::fmt::Display::fmt(e, f)
            }
        }
    }
}

impl std::error::Error for UsnReadError {}

/// 从 `start_usn` 开始读取增量变更，循环拉取直到追平 Journal 当前状态，
/// 或变更数超过 `MAX_USN_CHANGES`（此时调用方应回退全量扫描）。
///
/// 注意：读取起点是调用方为"某个目录缓存"记录的已校验 USN（per-path），
/// 而不是全局 checkpoint 的 max_usn —— 只有这样，"该目录已校验到 U" 的
/// 判断才是严谨的：如果缓存只是"某个更早时刻的快照"，从全局 checkpoint
/// 之后读增量会漏掉两者之间的变更。
pub fn read_incremental_changes(
    drive_letter: char,
    checkpoint: &UsnCheckpoint,
    start_usn: i64,
    max_records: usize,
) -> Result<UsnDelta, UsnReadError> {
    let journal = UsnJournal::open(drive_letter).map_err(UsnReadError::Io)?;

    // 验证 Journal ID 未变（Journal 未被重置）
    let current = journal.query_journal().map_err(UsnReadError::Io)?;
    if current.usn_journal_id != checkpoint.journal_id {
        return Err(UsnReadError::JournalReset);
    }

    // 验证卷序列号未变（防止盘符指向了另一块磁盘）
    let vol_serial = get_volume_serial(drive_letter).ok_or(UsnReadError::VolumeChanged)?;
    if vol_serial != checkpoint.volume_serial {
        return Err(UsnReadError::VolumeChanged);
    }

    // 起点必须落在 Journal 仍可读取的区间内：
    // - 低于 lowest_valid_usn：中间变更已被 Journal 回收，增量会漏数据；
    // - 高于 next_usn：位置非法（例如历史版本把 MaxUsn 常量当位置存了下来）。
    if start_usn < current.lowest_valid_usn || start_usn > current.next_usn {
        return Err(UsnReadError::WindowExpired);
    }

    let mut changes: Vec<UsnChangeRecord> = Vec::new();
    let mut start = start_usn;
    let mut next_usn = start_usn;
    // 批量大小与总量上限都跟随调用方：全局索引增量同步用小批量（每条变更要随机读
    // MFT 解析 FRN→路径，跨目录时约 5-8ms/条），目录缓存沿用 MAX_USN_CHANGES。
    let batch_records = max_records.clamp(64, 1024);
    let total_cap = max_records.max(1);

    loop {
        let (mut records, resume) = journal
            .read_changes_since(start, current.usn_journal_id, batch_records)
            .map_err(UsnReadError::Io)?;

        if resume <= start {
            // 没有进展：已到 Journal 末尾
            break;
        }

        next_usn = resume;
        start = resume;
        changes.append(&mut records);

        if changes.len() >= total_cap {
            break;
        }
        if start >= current.next_usn {
            break;
        }
    }

    Ok(UsnDelta {
        changes,
        next_usn,
        lowest_valid_usn: current.lowest_valid_usn,
    })
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

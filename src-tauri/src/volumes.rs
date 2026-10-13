//! 卷（驱动器）容量与基本信息：供标题栏容量条、扫描目标选择使用。
//!
//! 只用 Win32 查询（GetDiskFreeSpaceEx / GetVolumeInformation / GetDriveType），
//! 不触碰文件系统遍历，调用开销可忽略。

/// 单个卷的信息
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VolumeInfo {
    /// 盘符字母，如 "C"
    pub letter: String,
    /// 卷标（可能为空）
    pub label: String,
    /// 文件系统名，如 "NTFS"
    pub fs: String,
    /// 总容量（字节）。
    ///
    /// 注意：未就绪的卷（光驱无盘等）这里保持 0，并由 `ready = false` 表达"容量未知"，
    /// 而不是谎报一个非零容量——前端应先用 ready 判断再展示容量条。
    pub total_bytes: u64,
    /// 可用容量（字节）；未就绪时同样为 0，语义见 `total_bytes`。
    pub free_bytes: u64,
    /// 驱动器类型：2=可移动 3=固定 4=网络 5=光驱 6=RAM
    pub drive_type: u32,
    /// **MFT / USN 是否可用**（决定能否走 MFT / USN 快路径）。
    ///
    /// 旧实现的语义是"文件系统自报为 NTFS"，于是网络映射盘只要自报 NTFS 就被
    /// 上层 `global_search::list_ntfs_drives` 当成可扫描盘，最终落到 failed_drives。
    /// 现在要求"文件系统是 NTFS **且** 不是网络盘（DRIVE_REMOTE）"。
    /// 字段名与类型不变，仅收紧判定语义，消费方无需改动。
    pub is_ntfs: bool,
    /// 文件系统是否自报为 NTFS（只描述文件系统本身，不含"能否走 MFT"的判断）。
    /// 追加字段，向后兼容；`is_ntfs == fs_is_ntfs && !is_remote`。
    pub fs_is_ntfs: bool,
    /// 是否为网络映射盘（GetDriveTypeW 返回 DRIVE_REMOTE）。
    /// 追加字段，向后兼容。
    pub is_remote: bool,
    /// 是否就绪（光驱无盘等为 false）
    pub ready: bool,
}

/// GetDriveTypeW 的 DRIVE_REMOTE 取值。
///
/// 这里用自备常量而不是 `windows_sys::Win32::System::WindowsProgramming::DRIVE_REMOTE`：
/// 该常量位于需要额外 Cargo feature 的模块里，本文件不想为此扩大 windows-sys 的
/// feature 集合（也不需要）。
pub const DRIVE_REMOTE_TYPE: u32 = 4;

/// 推导"MFT / USN 是否可用"：文件系统必须是 NTFS，且不能是网络盘。
///
/// 抽成纯函数是为了能在任意平台上做回归测试（网络映射盘自报 NTFS 的场景无法在
/// CI 上真实构造）。非 Windows 平台没有 list_volumes 调用它，故只在那种配置下
/// 允许 dead_code，避免非 Windows 构建产生告警。
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
fn mft_usable(fs: &str, drive_type: u32) -> bool {
    fs.eq_ignore_ascii_case("NTFS") && drive_type != DRIVE_REMOTE_TYPE
}

/// 枚举所有存在的卷并返回容量信息
#[cfg(target_os = "windows")]
pub fn list_volumes() -> Vec<VolumeInfo> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        GetDiskFreeSpaceExW, GetDriveTypeW, GetLogicalDrives, GetVolumeInformationW,
    };

    fn wide(s: &str) -> Vec<u16> {
        std::ffi::OsStr::new(s)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    }

    let mask = unsafe { GetLogicalDrives() };
    let mut out = Vec::new();

    for i in 0..26u32 {
        if mask & (1 << i) == 0 {
            continue;
        }
        let letter = (b'A' + i as u8) as char;
        let root = format!("{}:\\", letter);
        let root_w = wide(&root);

        let drive_type = unsafe { GetDriveTypeW(root_w.as_ptr()) };

        let mut free_to_caller: u64 = 0;
        let mut total: u64 = 0;
        let mut total_free: u64 = 0;
        let ok = unsafe {
            GetDiskFreeSpaceExW(
                root_w.as_ptr(),
                &mut free_to_caller,
                &mut total,
                &mut total_free,
            )
        };
        let ready = ok != 0;

        let mut label_buf = [0u16; 256];
        let mut fs_buf = [0u16; 64];
        let mut serial: u32 = 0;
        let mut max_component: u32 = 0;
        let mut fs_flags: u32 = 0;
        let ok_info = unsafe {
            GetVolumeInformationW(
                root_w.as_ptr(),
                label_buf.as_mut_ptr(),
                label_buf.len() as u32,
                &mut serial,
                &mut max_component,
                &mut fs_flags,
                fs_buf.as_mut_ptr(),
                fs_buf.len() as u32,
            )
        };

        let to_string = |buf: &[u16]| -> String {
            let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
            String::from_utf16_lossy(&buf[..end])
        };

        let (label, fs) = if ok_info != 0 {
            (to_string(&label_buf), to_string(&fs_buf))
        } else {
            (String::new(), String::new())
        };

        let fs_is_ntfs = fs.eq_ignore_ascii_case("NTFS");
        let is_remote = drive_type == DRIVE_REMOTE_TYPE;

        out.push(VolumeInfo {
            letter: letter.to_string(),
            label,
            // is_ntfs 表示"MFT/USN 可用"：网络盘即使自报 NTFS 也不可用（MFT 打不开）
            is_ntfs: mft_usable(&fs, drive_type),
            fs_is_ntfs,
            is_remote,
            fs,
            total_bytes: total,
            free_bytes: total_free,
            drive_type,
            ready,
        });
    }

    out
}

#[cfg(not(target_os = "windows"))]
pub fn list_volumes() -> Vec<VolumeInfo> {
    Vec::new()
}

#[cfg(test)]
mod unit_tests {
    use super::*;

    /// 回归：网络映射盘自报 NTFS 时，旧实现 is_ntfs = true，
    /// 上层据此把该盘列入可扫描盘，最终落到 failed_drives。
    #[test]
    fn remote_ntfs_is_not_mft_usable() {
        assert!(mft_usable("NTFS", 3), "本地固定盘 NTFS 应可用");
        assert!(!mft_usable("NTFS", DRIVE_REMOTE_TYPE), "网络盘即使自报 NTFS 也不可用 MFT");
        // 可移动 NTFS 卷（U 盘）本身支持 MFT/USN，本次只收紧网络盘，行为保持不变
        assert!(mft_usable("NTFS", 2), "可移动 NTFS 卷不因本次收紧而被排除");
        assert!(!mft_usable("exFAT", 3), "非 NTFS 文件系统不可用");
        assert!(!mft_usable("FAT32", 2), "非 NTFS 文件系统不可用");
        assert!(!mft_usable("", 3), "文件系统未知时不可用");
        assert!(!mft_usable("", DRIVE_REMOTE_TYPE), "网络盘且文件系统未知更不可用");
        assert!(mft_usable("ntfs", 3), "文件系统名大小写不敏感");
    }

    #[test]
    fn drive_type_constant_matches_win32() {
        // GetDriveTypeW: DRIVE_REMOTE == 4。自备常量必须与 Win32 取值一致。
        assert_eq!(DRIVE_REMOTE_TYPE, 4);
    }

    /// 契约字段名不能变（前端 / MCP 依赖 camelCase 字段）。
    #[test]
    fn volume_info_serialization_keeps_backward_compatible_fields() {
        let v = VolumeInfo {
            letter: "C".to_string(),
            label: "sys".to_string(),
            fs: "NTFS".to_string(),
            total_bytes: 1000,
            free_bytes: 400,
            drive_type: 3,
            is_ntfs: true,
            fs_is_ntfs: true,
            is_remote: false,
            ready: true,
        };
        let json = serde_json::to_value(&v).expect("序列化失败");
        for key in [
            "letter",
            "label",
            "fs",
            "totalBytes",
            "freeBytes",
            "driveType",
            "isNtfs",
            "ready",
            // 追加字段
            "fsIsNtfs",
            "isRemote",
        ] {
            assert!(json.get(key).is_some(), "缺少契约字段 {key}");
        }
        assert_eq!(json["isNtfs"], serde_json::json!(true));
    }
}

#[cfg(all(test, target_os = "windows"))]
mod tests {
    #[test]
    fn list_volumes_returns_sane_data() {
        let vols = super::list_volumes();
        assert!(!vols.is_empty(), "至少应枚举到一个卷");

        // 契约：前端 v-for 直接遍历，必须序列化为数组
        let json = serde_json::to_value(&vols).expect("序列化失败");
        assert!(json.is_array(), "卷列表必须序列化为数组");
        for v in &vols {
            assert!(!v.letter.is_empty());
            if v.ready {
                assert!(v.total_bytes > 0, "{} 卷总容量应大于 0", v.letter);
                assert!(v.free_bytes <= v.total_bytes, "{} 可用容量不应超过总容量", v.letter);
            } else {
                // 未就绪盘不谎报容量：容量字段保持 0，用 ready=false 表达
                assert_eq!(v.total_bytes, 0, "{} 未就绪盘不应谎报总容量", v.letter);
                assert_eq!(v.free_bytes, 0, "{} 未就绪盘不应谎报可用容量", v.letter);
            }
            // 语义一致性：is_ntfs 必须等价于"文件系统是 NTFS 且非网络盘"
            assert_eq!(
                v.is_ntfs,
                v.fs_is_ntfs && !v.is_remote,
                "{} 卷 is_ntfs 语义应为 MFT/USN 可用",
                v.letter
            );
            if v.is_remote {
                assert!(!v.is_ntfs, "{} 网络盘不得被判为可走 MFT", v.letter);
            }
        }
        let summary: Vec<String> = vols
            .iter()
            .map(|v| {
                format!(
                    "{}: {} {} {}GB/{}GB type={}",
                    v.letter,
                    v.label,
                    v.fs,
                    v.total_bytes / 1024 / 1024 / 1024,
                    v.free_bytes / 1024 / 1024 / 1024,
                    v.drive_type
                )
            })
            .collect();
        eprintln!("[test] 枚举到卷:\n  {}", summary.join("\n  "));
    }
}

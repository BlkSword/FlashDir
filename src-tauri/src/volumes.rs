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
    /// 总容量（字节）
    pub total_bytes: u64,
    /// 可用容量（字节）
    pub free_bytes: u64,
    /// 驱动器类型：2=可移动 3=固定 4=网络 5=光驱 6=RAM
    pub drive_type: u32,
    /// 是否 NTFS（决定能否走 MFT / USN 快路径）
    pub is_ntfs: bool,
    /// 是否就绪（光驱无盘等为 false）
    pub ready: bool,
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

        out.push(VolumeInfo {
            letter: letter.to_string(),
            label,
            is_ntfs: fs.eq_ignore_ascii_case("NTFS"),
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

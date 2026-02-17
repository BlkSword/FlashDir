use std::ffi::OsString;
use std::os::windows::ffi::OsStringExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use tokio::sync::mpsc;
use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FindClose, FindFirstFileExW, FindNextFileW, GetFileSizeEx, FILE_ATTRIBUTE_DIRECTORY,
    FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OVERLAPPED, FILE_SHARE_READ, FILE_SHARE_WRITE,
    FIND_FIRST_EX_CASE_SENSITIVE, FIND_FIRST_EX_LARGE_FETCH, FINDEX_INFO_LEVELS,
    FINDEX_SEARCH_OPS, WIN32_FIND_DATAW,
};
use windows_sys::Win32::System::IO::{CreateIoCompletionPort, GetQueuedCompletionStatus, PostQueuedCompletionStatus};

use crate::FileInfo;

const IOCP_BUFFER_SIZE: usize = 64 * 1024;
const MAX_CONCURRENT_OPS: usize = 64;

#[repr(C)]
struct IoContext {
    overlapped: windows_sys::Win32::System::IO::OVERLAPPED,
    buffer: [u8; IOCP_BUFFER_SIZE],
    path: PathBuf,
    operation_type: OperationType,
}

#[derive(Clone, Copy, Debug)]
enum OperationType {
    DirectoryScan,
    FileStat,
}

pub struct IocpScanner {
    iocp_handle: HANDLE,
    stats: Arc<ScanStats>,
}

pub struct ScanStats {
    files_scanned: AtomicU64,
    dirs_scanned: AtomicU64,
    bytes_read: AtomicU64,
}

impl ScanStats {
    pub fn new() -> Self {
        Self {
            files_scanned: AtomicU64::new(0),
            dirs_scanned: AtomicU64::new(0),
            bytes_read: AtomicU64::new(0),
        }
    }

    pub fn record_file(&self, size: u64) {
        self.files_scanned.fetch_add(1, Ordering::Relaxed);
        self.bytes_read.fetch_add(size, Ordering::Relaxed);
    }

    pub fn record_dir(&self) {
        self.dirs_scanned.fetch_add(1, Ordering::Relaxed);
    }

    pub fn snapshot(&self) -> StatsSnapshot {
        StatsSnapshot {
            files: self.files_scanned.load(Ordering::Relaxed),
            dirs: self.dirs_scanned.load(Ordering::Relaxed),
            bytes: self.bytes_read.load(Ordering::Relaxed),
        }
    }
}

pub struct StatsSnapshot {
    pub files: u64,
    pub dirs: u64,
    pub bytes: u64,
}

impl IocpScanner {
    pub fn new() -> std::io::Result<Self> {
        let iocp_handle = unsafe {
            CreateIoCompletionPort(INVALID_HANDLE_VALUE, std::ptr::null_mut(), 0, 0)
        };

        if iocp_handle.is_null() || iocp_handle == INVALID_HANDLE_VALUE {
            return Err(std::io::Error::last_os_error());
        }

        Ok(Self {
            iocp_handle,
            stats: Arc::new(ScanStats::new()),
        })
    }

    pub async fn scan_directory(&self, root: PathBuf) -> std::io::Result<Vec<FileInfo>> {
        let start = Instant::now();
        let (tx, mut rx) = mpsc::channel::<FileInfo>(10000);
        let results = Arc::new(std::sync::Mutex::new(Vec::with_capacity(10000)));
        let results_clone = results.clone();

        let collector = tokio::spawn(async move {
            while let Some(info) = rx.recv().await {
                results_clone.lock().unwrap().push(info);
            }
        });

        self.scan_with_iocp(root, tx).await?;

        drop(collector);
        let _ = tokio::time::timeout(tokio::time::Duration::from_secs(5), collector).await;

        let files = Arc::try_unwrap(results)
            .unwrap()
            .into_inner()
            .unwrap();

        let elapsed = start.elapsed();
        let stats = self.stats.snapshot();
        log::info!(
            "IOCP scan completed: {} files, {} dirs in {:?} ({:.0} files/sec)",
            stats.files,
            stats.dirs,
            elapsed,
            stats.files as f64 / elapsed.as_secs_f64()
        );

        Ok(files)
    }

    async fn scan_with_iocp(
        &self,
        root: PathBuf,
        tx: mpsc::Sender<FileInfo>,
    ) -> std::io::Result<()> {
        let mut pending_dirs = vec![root];
        let mut active_ops = 0usize;

        while !pending_dirs.is_empty() || active_ops > 0 {
            while active_ops < MAX_CONCURRENT_OPS && !pending_dirs.is_empty() {
                let dir = pending_dirs.pop().unwrap();
                self.submit_directory_scan(dir, &tx)?;
                active_ops += 1;
            }

            if active_ops > 0 {
                match self.wait_for_completion().await {
                    Ok((completed_dir, subdirs, files)) => {
                        active_ops -= 1;
                        pending_dirs.extend(subdirs);
                        for file in files {
                            let _ = tx.send(file).await;
                        }
                        self.stats.record_dir();
                    }
                    Err(e) => {
                        log::warn!("IOCP completion error: {}", e);
                        active_ops -= 1;
                    }
                }
            }
        }

        Ok(())
    }

    fn submit_directory_scan(
        &self,
        path: PathBuf,
        _tx: &mpsc::Sender<FileInfo>,
    ) -> std::io::Result<()> {
        let wide_path: Vec<u16> = path
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();

        let search_pattern: Vec<u16> = path
            .join("*")
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();

        unsafe {
            let mut find_data: WIN32_FIND_DATAW = std::mem::zeroed();
            let handle = FindFirstFileExW(
                search_pattern.as_ptr(),
                FINDEX_INFO_LEVELS::FindExInfoBasic,
                &mut find_data as *mut _ as *mut _,
                FINDEX_SEARCH_OPS::FindExSearchNameMatch,
                std::ptr::null(),
                FIND_FIRST_EX_LARGE_FETCH | FIND_FIRST_EX_CASE_SENSITIVE,
            );

            if handle == INVALID_HANDLE_VALUE {
                let err = GetLastError();
                if err == 2 || err == 3 {
                    return Ok(());
                }
                return Err(std::io::Error::from_raw_os_error(err as i32));
            }

            let _ = self.process_find_data(handle, &find_data, &path);
            FindClose(handle);
        }

        Ok(())
    }

    unsafe fn process_find_data(
        &self,
        handle: HANDLE,
        find_data: &WIN32_FIND_DATAW,
        base_path: &PathBuf,
    ) -> std::io::Result<(Vec<PathBuf>, Vec<FileInfo>)> {
        let mut subdirs = Vec::new();
        let mut files = Vec::new();
        let mut find_data = *find_data;

        loop {
            let name_len = find_data.cFileName.iter().position(|&c| c == 0).unwrap_or(260);
            let name = OsString::from_wide(&find_data.cFileName[..name_len]);

            if name != "." && name != ".." {
                let full_path = base_path.join(&name);
                let is_directory = (find_data.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY) != 0;

                if is_directory {
                    subdirs.push(full_path);
                } else {
                    let file_info = self.create_file_info(&find_data, &full_path)?;
                    self.stats.record_file(file_info.size);
                    files.push(file_info);
                }
            }

            if FindNextFileW(handle, &mut find_data) == 0 {
                break;
            }
        }

        Ok((subdirs, files))
    }

    unsafe fn create_file_info(
        &self,
        find_data: &WIN32_FIND_DATAW,
        path: &PathBuf,
    ) -> std::io::Result<FileInfo> {
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_string();

        let size = ((find_data.nFileSizeHigh as u64) << 32) | (find_data.nFileSizeLow as u64);

        let modified = Self::file_time_to_timestamp(&find_data.ftLastWriteTime);
        let created = Self::file_time_to_timestamp(&find_data.ftCreationTime);

        let extension = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase();

        Ok(FileInfo {
            name,
            path: path.to_string_lossy().to_string(),
            size,
            is_directory: false,
            modified,
            created,
            extension,
        })
    }

    unsafe fn file_time_to_timestamp(ft: &windows_sys::Win32::Foundation::FILETIME) -> u64 {
        let ticks = ((ft.dwHighDateTime as u64) << 32) | (ft.dwLowDateTime as u64);
        ticks / 10000000 - 11644473600
    }

    async fn wait_for_completion(&self) -> std::io::Result<(PathBuf, Vec<PathBuf>, Vec<FileInfo>)> {
        tokio::task::yield_now().await;
        Ok((PathBuf::new(), Vec::new(), Vec::new()))
    }
}

impl Drop for IocpScanner {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.iocp_handle);
        }
    }
}

pub fn create_iocp_scanner() -> std::io::Result<IocpScanner> {
    IocpScanner::new()
}

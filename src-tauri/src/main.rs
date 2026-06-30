#![cfg_attr(
    all(not(debug_assertions), target_os = "windows"),
    windows_subsystem = "windows"
)]

#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

use std::collections::VecDeque;
use parking_lot::Mutex;

mod commands;

use flashdir::scan;
use flashdir::global_search;
use tauri::Emitter;

struct AppState {
    history: Mutex<VecDeque<scan::HistoryItem>>,
}

#[tokio::main]
async fn main() {
    let _ = flashdir::disk_cache::DiskCache::instance();

    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_fs::init())
        .manage(AppState {
            history: Mutex::new(commands::load_history_from_file_sync()),
        })
        .setup(|app| {
            let app_handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                let idx = global_search::instance();

                // 若已从磁盘持久化缓存恢复，则不再重复构建
                if matches!(idx.state(), global_search::IndexState::Ready(..)) {
                    let _ = app_handle.emit(
                        "global-search-progress",
                        serde_json::json!({ "drive": "", "scanned": 0, "phase": "done" }),
                    );
                    return;
                }

                idx.set_loading();
                let drives = global_search::list_ntfs_drives();
                if drives.is_empty() {
                    idx.set_failed("未检测到可扫描的 NTFS 卷".to_string());
                    let _ = app_handle.emit(
                        "global-search-progress",
                        serde_json::json!({ "drive": "", "scanned": 0, "phase": "done" }),
                    );
                    return;
                }

                let mut ok_drives: Vec<char> = Vec::new();
                for &drive in &drives {
                    let _ = app_handle.emit(
                        "global-search-progress",
                        serde_json::json!({ "drive": drive.to_string(), "scanned": 0, "phase": "scanning" }),
                    );

                    let root = format!("{}:\\", drive);
                    let app_h = app_handle.clone();
                    let result = tokio::task::spawn_blocking(move || {
                        scan::scan_lite(&root).map(|items| {
                            let count = items.len();
                            global_search::instance().append_scan(drive, &items);
                            (drive, count)
                        })
                    })
                    .await;

                    match result {
                        Ok(Some((drive, count))) => {
                            ok_drives.push(drive);
                            let _ = app_h.emit(
                                "global-search-progress",
                                serde_json::json!({ "drive": drive.to_string(), "scanned": count, "phase": "ok (lite)" }),
                            );
                        }
                        _ => {
                            let _ = app_h.emit(
                                "global-search-progress",
                                serde_json::json!({ "drive": drive.to_string(), "scanned": 0, "phase": "skipped" }),
                            );
                        }
                    }
                }

                if ok_drives.is_empty() {
                    idx.set_failed("需要管理员权限才能读取 NTFS MFT".to_string());
                } else {
                    idx.finish_building(&ok_drives);
                }
                let _ = app_handle.emit(
                    "global-search-progress",
                    serde_json::json!({ "drive": "", "scanned": 0, "phase": "done" }),
                );
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::scan_directory,
            commands::scan_directory_binary,
            commands::scan_directories_batch,
            commands::get_history_summary,
            commands::get_history,
            commands::clear_history,
            commands::get_performance_metrics,
            commands::get_performance_history,
            commands::clear_performance_history,
            commands::get_performance_summary,
            commands::get_disk_cache_stats,
            commands::clear_disk_cache,
            commands::get_memory_cache_stats,
            commands::get_system_info,
            commands::is_admin,
            commands::check_mft_available,
            commands::get_scan_status,
            commands::open_path,
            commands::is_directory,
            commands::restart_as_admin,
            commands::analyze_dev_disk,
            commands::save_snapshot,
            commands::list_snapshots,
            commands::compare_snapshots,
            commands::delete_snapshot,
            commands::compare_with_latest_snapshot,
            commands::global_search_status,
            commands::global_search_ensure_index,
            commands::global_search,
            commands::global_search_refresh,
            commands::global_search_add_scan,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

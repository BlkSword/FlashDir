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
use tauri::{Emitter, Manager};

struct AppState {
    history: Mutex<VecDeque<scan::HistoryItem>>,
    tray: Mutex<Option<tauri::tray::TrayIcon>>,
}


fn toggle_main_window(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        if window.is_visible().unwrap_or(false) {
            let _ = window.hide();
        } else {
            let _ = window.show();
            let _ = window.unminimize();
            let _ = window.set_focus();
        }
    }
}

fn setup_tray(app: &tauri::AppHandle) -> tauri::Result<()> {
    use tauri::menu::{Menu, MenuItem};
    use tauri::tray::TrayIconBuilder;

    let show_item = MenuItem::with_id(app, "show", "显示/隐藏主窗口", true, None::<&str>)?;
    let quit_item = MenuItem::with_id(app, "quit", "退出 FlashDir", true, None::<&str>)?;

    let menu = Menu::with_items(app, &[&show_item, &quit_item])?;

    let tray = TrayIconBuilder::with_id("flashdir-tray")
        .icon(app.default_window_icon().cloned().expect("default window icon"))
        .tooltip("FlashDir")
        .menu(&menu)
        .show_menu_on_left_click(true)
        .on_menu_event(|app, event| match event.id().as_ref() {
            "show" => toggle_main_window(app),
            "quit" => app.exit(0),
            _ => {}
        })
        .build(app)?;

    let state = app.state::<AppState>();
    *state.tray.lock() = Some(tray);
    Ok(())
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
            tray: Mutex::new(None),
        })
        .setup(|app| {
            let app_handle = app.handle().clone();

            // 关闭主窗口 = 最小化到系统托盘
            if let Some(main_window) = app_handle.get_webview_window("main") {
                let main_window_for_event = main_window.clone();
                main_window.on_window_event(move |event| {
                    if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                        api.prevent_close();
                        let _ = main_window_for_event.hide();
                    }
                });
            }

            // 创建系统托盘
            if let Err(e) = setup_tray(&app_handle) {
                eprintln!("[Tray] 创建托盘失败: {}", e);
            }

            tauri::async_runtime::spawn(async move {
                let idx = global_search::instance();

                // 1. 后台加载持久化索引，避免阻塞启动路径
                let _ = app_handle.emit(
                    "global-search-progress",
                    serde_json::json!({ "drive": "", "scanned": 0, "phase": "loading-persisted" }),
                );
                let load_result = tokio::task::spawn_blocking(move || {
                    global_search::instance().load_persisted();
                    global_search::instance().state()
                })
                .await;

                if let Ok(global_search::IndexState::Ready(..)) = load_result {
                    let _ = app_handle.emit(
                        "global-search-progress",
                        serde_json::json!({ "drive": "", "scanned": 0, "phase": "done" }),
                    );
                    return;
                }

                // 2. 没有可用持久化缓存，继续走轻量扫描构建索引
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
                let mut failed_drives: Vec<char> = Vec::new();
                for &drive in &drives {
                    let root = format!("{}:\\", drive);
                    let app_h = app_handle.clone();
                    let result = tokio::task::spawn_blocking(move || {
                        flashdir::fs::try_mft_scan(&root).map(|mft_result| {
                            let count = mft_result.files.len();
                            global_search::instance().append_mft_files(drive, &mft_result.files);
                            (drive, count)
                        })
                    })
                    .await;

                    match result {
                        Ok(Some((drive, count))) => {
                            ok_drives.push(drive);
                            let total = global_search::instance().entries_len();
                            let _ = app_h.emit(
                                "global-search-progress",
                                serde_json::json!({ "drive": drive.to_string(), "scanned": total, "phase": "ok (lite)", "count": count }),
                            );
                        }
                        _ => {
                            failed_drives.push(drive);
                            let _ = app_h.emit(
                                "global-search-progress",
                                serde_json::json!({ "drive": drive.to_string(), "scanned": global_search::instance().entries_len(), "phase": "skipped" }),
                            );
                        }
                    }
                }

                if ok_drives.is_empty() {
                    idx.set_failed("需要管理员权限才能读取 NTFS MFT".to_string());
                } else {
                    idx.finish_building(&ok_drives, &failed_drives);
                }
                let _ = app_handle.emit(
                    "global-search-progress",
                    serde_json::json!({ "drive": "", "scanned": global_search::instance().entries_len(), "phase": "done" }),
                );
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::scan_directory_paged,
            commands::get_dir_children,
            commands::get_history_summary,
            commands::clear_history,
            commands::get_memory_cache_stats,
            commands::get_diagnostics,
            commands::is_admin,
            commands::cancel_scan,
            commands::open_path,
            commands::is_directory,
            commands::restart_as_admin,
            commands::analyze_dev_disk,
            commands::find_duplicates,
            commands::save_snapshot_from_cache,
            commands::list_snapshots,
            commands::compare_snapshots,
            commands::compare_with_latest_snapshot_from_cache,
            commands::delete_snapshot,
            commands::global_search_status,
            commands::global_search_ensure_index,
            commands::global_search,
            commands::global_search_refresh,
            commands::global_search_add_scan_from_cache,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

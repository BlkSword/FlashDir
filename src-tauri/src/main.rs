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

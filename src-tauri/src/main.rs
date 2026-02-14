#![cfg_attr(
    all(not(debug_assertions), target_os = "windows"),
    windows_subsystem = "windows"
)]

#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

use std::collections::VecDeque;
use parking_lot::Mutex;

mod commands;
mod scan;
mod perf;
mod disk_cache;
mod binary_protocol;

struct AppState {
    history: Mutex<VecDeque<scan::HistoryItem>>,
}

#[tokio::main]
async fn main() {
    let _ = disk_cache::DiskCache::instance();

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
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

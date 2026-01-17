#![cfg_attr(
    all(not(debug_assertions), target_os = "windows"),
    windows_subsystem = "windows"
)]

use std::sync::Mutex;

mod commands;
mod scan;

struct AppState {
    history: Mutex<Vec<scan::HistoryItem>>,
}

#[tokio::main]
async fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_fs::init())
        .manage(AppState {
            history: Mutex::new(Vec::new()),
        })
        .setup(|_app| Ok(()))
        .invoke_handler(tauri::generate_handler![
            commands::scan_directory,
            commands::get_history,
            commands::get_history_item,
            commands::clear_history,
            commands::open_in_explorer,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

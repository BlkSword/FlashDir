use crate::scan::{self, HistoryItem, ScanResult};
use crate::AppState;
use chrono::Utc;
use tauri::{command, State};
use std::fs::{self, File};
use std::io::Write;
use std::path::PathBuf;
use tokio::task;

fn get_history_file_path() -> Result<PathBuf, String> {
    let home_dir = std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .map_err(|_| "无法获取用户目录")?;

    let mut path = PathBuf::from(home_dir);
    path.push(".flashdir");
    path.push("history.json");
    Ok(path)
}

pub fn load_history_from_file() -> Vec<HistoryItem> {
    match get_history_file_path() {
        Ok(path) => {
            if path.exists() {
                match fs::read_to_string(&path) {
                    Ok(content) => {
                        serde_json::from_str(&content).unwrap_or_default()
                    }
                    Err(_) => Vec::new()
                }
            } else {
                if let Some(parent) = path.parent() {
                    let _ = fs::create_dir_all(parent);
                }
                Vec::new()
            }
        }
        Err(_) => Vec::new()
    }
}

fn save_history_to_file(history: &[HistoryItem]) -> Result<(), String> {
    let path = get_history_file_path()?;

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("创建目录失败: {}", e))?;
    }

    let json = serde_json::to_string_pretty(history)
        .map_err(|e| format!("序列化失败: {}", e))?;

    let mut file = File::create(&path)
        .map_err(|e| format!("创建文件失败: {}", e))?;

    file.write_all(json.as_bytes())
        .map_err(|e| format!("写入文件失败: {}", e))?;

    Ok(())
}

#[command]
pub async fn scan_directory(
    path: String,
    force_refresh: bool,
    state: State<'_, AppState>,
) -> Result<ScanResult, String> {
    let path = path.trim();

    if path.is_empty() {
        return Err("请提供有效的目录路径".to_string());
    }

    match scan::scan_directory(path, force_refresh).await {
        Ok(mut result) => {
            let history_item = HistoryItem {
                path: path.to_string(),
                scan_time: Utc::now(),
                total_size: result.total_size,
                size_format: result.total_size_formatted.clone(),
                items: result.items.clone(),
            };

            let mut history = state.history.lock().unwrap();
            history.push(history_item.clone());

            if history.len() > 20 {
                history.remove(0);
            }

            let history_slice: Vec<_> = history.iter().cloned().collect();
            drop(history);

            task::spawn_blocking(move || {
                let _ = save_history_to_file(&history_slice);
            });

            result.path = path.to_string();

            Ok(result)
        }
        Err(e) => Err(e.to_string()),
    }
}

#[command]
pub fn get_history(state: State<'_, AppState>) -> Vec<HistoryItem> {
    let history = state.history.lock().unwrap();
    history.iter().rev().cloned().collect()
}

#[command]
pub fn clear_history(state: State<'_, AppState>) -> Result<(), String> {
    let mut history = state.history.lock().unwrap();
    history.clear();
    drop(history);
    save_history_to_file(&[])
}

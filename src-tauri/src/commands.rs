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
    let path = path.trim().to_string();

    if path.is_empty() {
        return Err("请提供有效的目录路径".to_string());
    }

    match scan::scan_directory(&path, force_refresh).await {
        Ok(result) => {
            // 优化：避免克隆整个 items 列表
            // 直接使用 result 的数据构造 HistoryItem
            let history_item = HistoryItem {
                path: path.clone(),
                scan_time: Utc::now(),
                total_size: result.total_size,
                size_format: result.total_size_formatted.clone(),
                items: result.items.clone(), // 保留这个克隆，因为历史记录需要独立副本
            };

            let mut history = state.history.lock().unwrap();
            history.push(history_item);

            if history.len() > 20 {
                history.remove(0);
            }

            // 优化：使用 into_iter 避免克隆，但在闭包中需要获取所有权
            // 由于 spawn_blocking 需要 'static，我们仍然需要克隆
            // 但可以优化为只克隆必要的数据
            let history_for_save: Vec<HistoryItem> = history.iter().cloned().collect();
            drop(history);

            // 异步保存历史，不阻塞响应
            task::spawn_blocking(move || {
                if let Err(e) = save_history_to_file(&history_for_save) {
                    eprintln!("保存历史记录失败: {}", e);
                }
            });

            Ok(result)
        }
        Err(e) => Err(e.to_string()),
    }
}

#[command]
pub fn get_history(state: State<'_, AppState>) -> Vec<HistoryItem> {
    let history = state.history.lock().unwrap();
    // 优化：使用 reverse in-place 而不是创建新向量
    let mut result: Vec<_> = history.iter().cloned().collect();
    result.reverse();
    result
}

#[command]
pub fn clear_history(state: State<'_, AppState>) -> Result<(), String> {
    let mut history = state.history.lock().unwrap();
    history.clear();
    drop(history);
    save_history_to_file(&[])
}

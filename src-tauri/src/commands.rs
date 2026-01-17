use crate::scan::{self, HistoryItem, ScanResult};
use crate::AppState;
use chrono::Utc;
use tauri::{command, State};

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
            // 添加到历史记录（包括缓存命中）
            let history_item = HistoryItem {
                path: path.to_string(),
                scan_time: Utc::now(),
                total_size: result.total_size,
                size_format: result.total_size_formatted.clone(),
                items: result.items.clone(),
            };

            // 保存到历史记录
            let mut history = state.history.lock().unwrap();
            history.push(history_item);

            // 保持历史记录在合理范围内（最多保存20条，减少内存占用）
            if history.len() > 20 {
                history.remove(0);
            }

            // 更新结果中的路径为规范路径
            result.path = path.to_string();

            Ok(result)
        }
        Err(e) => Err(e.to_string()),
    }
}

#[command]
pub fn get_history(state: State<'_, AppState>) -> Vec<HistoryItem> {
    let history = state.history.lock().unwrap();
    // 返回逆序（最新的在前）
    history.iter().rev().cloned().collect()
}

#[command]
pub fn get_history_item(path: String, state: State<'_, AppState>) -> Option<ScanResult> {
    let history = state.history.lock().unwrap();

    // 查找最新的匹配历史记录
    for item in history.iter().rev() {
        if item.path == path {
            return Some(ScanResult {
                items: item.items.clone(),
                total_size: item.total_size,
                total_size_formatted: item.size_format.clone(),
                scan_time: 0.0,
                path: item.path.clone(),
            });
        }
    }

    None
}

#[command]
pub fn clear_history(state: State<'_, AppState>) -> Result<(), String> {
    let mut history = state.history.lock().unwrap();
    history.clear();
    Ok(())
}

#[command]
pub fn open_in_explorer(path: String) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        use std::process::Command;
        Command::new("explorer")
            .args(["/select,", &path])
            .spawn()
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    #[cfg(target_os = "macos")]
    {
        use std::process::Command;
        Command::new("open")
            .args(["-R", &path])
            .spawn()
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    #[cfg(target_os = "linux")]
    {
        use std::process::Command;
        Command::new("xdg-open")
            .arg(&path)
            .spawn()
            .map_err(|e| e.to_string())?;
        Ok(())
    }
}

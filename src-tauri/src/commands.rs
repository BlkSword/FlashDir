use crate::scan::{self, HistoryItem, HistoryItemSummary, ScanResult};
use crate::AppState;
use chrono::Utc;
use std::collections::VecDeque;
use tauri::{command, State};
use std::path::PathBuf;
use tokio::{fs, io::AsyncWriteExt};

fn get_history_file_path() -> Result<PathBuf, String> {
    let home_dir = std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .map_err(|_| "无法获取用户目录")?;

    let mut path = PathBuf::from(home_dir);
    path.push(".flashdir");
    path.push("history.json");
    Ok(path)
}

pub fn load_history_from_file_sync() -> VecDeque<HistoryItem> {
    match get_history_file_path() {
        Ok(path) => {
            if path.exists() {
                match std::fs::read_to_string(&path) {
                    Ok(content) => {
                        match serde_json::from_str::<VecDeque<HistoryItem>>(&content) {
                            Ok(history) => history,
                            Err(_) => {
                                #[derive(serde::Deserialize)]
                                struct OldHistoryItem {
                                    path: String,
                                    #[serde(with = "chrono::serde::ts_seconds")]
                                    scan_time: chrono::DateTime<chrono::Utc>,
                                    total_size: i64,
                                    size_format: String,
                                    items: Vec<scan::Item>,
                                }

                                let old_history: Vec<OldHistoryItem> =
                                    serde_json::from_str(&content).unwrap_or_default();

                                old_history.into_iter().map(|old| HistoryItem {
                                    path: smartstring::SmartString::from(old.path),
                                    scan_time: old.scan_time,
                                    total_size: old.total_size,
                                    size_format: smartstring::SmartString::from(old.size_format),
                                    item_count: old.items.len(),
                                }).collect()
                            }
                        }
                    }
                    Err(_) => VecDeque::new()
                }
            } else {
                if let Some(parent) = path.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                VecDeque::new()
            }
        }
        Err(_) => VecDeque::new()
    }
}

async fn save_history_to_file_async(history: &VecDeque<HistoryItem>) -> Result<(), String> {
    let path = get_history_file_path()?;

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .await
            .map_err(|e| format!("创建目录失败: {}", e))?;
    }

    let json = serde_json::to_string(history)
        .map_err(|e| format!("序列化失败: {}", e))?;

    let mut file = fs::File::create(&path)
        .await
        .map_err(|e| format!("创建文件失败: {}", e))?;

    file.write_all(json.as_bytes())
        .await
        .map_err(|e| format!("写入文件失败: {}", e))?;

    file.sync_all()
        .await
        .map_err(|e| format!("同步文件失败: {}", e))?;

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
            let history_item = HistoryItem {
                path: smartstring::SmartString::from(path.clone()),
                scan_time: Utc::now(),
                total_size: result.total_size,
                size_format: smartstring::SmartString::from(result.total_size_formatted.as_str()),
                item_count: result.items.len(),
            };

            let mut history = state.history.lock();
            history.push_back(history_item);

            while history.len() > 20 {
                history.pop_front();
            }

            let history_for_save: VecDeque<HistoryItem> = history.clone();
            drop(history);

            tokio::spawn(async move {
                if let Err(e) = save_history_to_file_async(&history_for_save).await {
                    eprintln!("保存历史记录失败: {}", e);
                }
            });

            Ok(result)
        }
        Err(e) => Err(e.to_string()),
    }
}

#[command]
pub fn get_history_summary(state: State<'_, AppState>) -> Vec<HistoryItemSummary> {
    let history = state.history.lock();
    let summaries: Vec<HistoryItemSummary> = history.iter().map(|item| item.into()).collect();
    summaries.into_iter().rev().collect()
}

#[command]
pub fn get_history(state: State<'_, AppState>) -> Vec<HistoryItem> {
    let history = state.history.lock();
    let mut result: Vec<_> = history.iter().cloned().collect();
    result.reverse();
    result
}

#[command]
pub async fn clear_history(state: State<'_, AppState>) -> Result<(), String> {
    {
        let mut history = state.history.lock();
        history.clear();
    }

    save_history_to_file_async(&VecDeque::new()).await
}

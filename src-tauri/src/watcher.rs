// 轻量目录变更监听
//
// 原理：定期调用 scan_directory（内部优先走 USN 增量），
// 与上一次结果做 diff，并把变更摘要通过 `dir-changes` 事件推送给前端。
// 监听频率默认 5 秒一次，适合作为“近实时变更”能力。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;
use std::time::Duration;

use parking_lot::Mutex;
use tauri::Emitter;

use crate::scan::Item;

static WATCH_PATH: OnceLock<Mutex<Option<String>>> = OnceLock::new();
static WATCH_STOP: AtomicBool = AtomicBool::new(false);

fn watch_path() -> &'static Mutex<Option<String>> {
    WATCH_PATH.get_or_init(|| Mutex::new(None))
}

/// 启动监听；如果已有监听会先停止旧监听
pub fn start(app: tauri::AppHandle, path: String) {
    stop();
    *watch_path().lock() = Some(path.clone());
    WATCH_STOP.store(false, Ordering::Relaxed);

    tauri::async_runtime::spawn(async move {
        let mut last_items: Option<std::sync::Arc<Vec<Item>>> = None;

        loop {
            if WATCH_STOP.load(Ordering::Relaxed) {
                break;
            }

            let current_path = watch_path().lock().clone();
            let Some(current_path) = current_path else {
                break;
            };

            let perf = crate::perf::PerformanceMonitor::instance();
            match crate::scan::scan_directory(&current_path, false, perf, None).await {
                Ok(result) => {
                    if let Some(prev) = &last_items {
                        let old_total: i64 = prev
                            .iter()
                            .filter(|i| !i.is_dir)
                            .map(|i| i.size)
                            .sum();
                        let diff = crate::diff_engine::diff(prev, &result.items, old_total);
                        let changed = diff.added.len() + diff.removed.len() + diff.modified.len();
                        if changed > 0 {
                            let _ = app.emit(
                                "dir-changes",
                                serde_json::json!({
                                    "path": current_path,
                                    "changed": changed,
                                    "added": diff.added.len(),
                                    "removed": diff.removed.len(),
                                    "modified": diff.modified.len(),
                                    "netChange": diff.net_change,
                                    "timestamp": chrono::Utc::now().timestamp(),
                                }),
                            );
                        }
                    }
                    last_items = Some(std::sync::Arc::new(result.items));
                }
                Err(_) => {
                    // 扫描失败时保留上次结果，下一轮继续尝试
                }
            }

            tokio::time::sleep(Duration::from_secs(5)).await;
        }

        *watch_path().lock() = None;
    });
}

/// 停止监听
pub fn stop() {
    WATCH_STOP.store(true, Ordering::Relaxed);
    *watch_path().lock() = None;
}

/// 当前是否正在监听
pub fn is_active() -> bool {
    WATCH_STOP.load(Ordering::Relaxed) == false && watch_path().lock().is_some()
}

// 快照差异引擎
//
// 比较两次扫描结果，识别新增、删除、修改的文件。
// 用于磁盘空间变化追踪和增长趋势分析。

use serde::Serialize;
use std::collections::HashMap;
use crate::scan::{Item, format_size};

/// 差异结果
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotDiff {
    /// 新增文件/目录
    pub added: Vec<DiffItem>,
    /// 已删除的文件/目录
    pub removed: Vec<DiffItem>,
    /// 大小发生变化的文件/目录
    pub modified: Vec<DiffModifyItem>,
    /// 新增总字节数
    pub added_total_size: i64,
    /// 删除总字节数
    pub removed_total_size: i64,
    /// 修改导致的净变化 (new - old)
    pub modified_delta: i64,
    /// 净变化 (added - removed + modified_delta)
    pub net_change: i64,
    /// 未变更的文件数
    pub unchanged_count: usize,
    /// 变更统计摘要
    pub summary: DiffSummary,
}

/// 差异项
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiffItem {
    pub path: String,
    pub name: String,
    pub size: i64,
    pub size_formatted: String,
    pub is_dir: bool,
}

/// 修改项
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiffModifyItem {
    pub path: String,
    pub name: String,
    pub old_size: i64,
    pub new_size: i64,
    pub delta: i64,
    pub old_size_formatted: String,
    pub new_size_formatted: String,
    pub delta_formatted: String,
    pub is_dir: bool,
}

/// 差异摘要
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiffSummary {
    pub added_count: usize,
    pub removed_count: usize,
    pub modified_count: usize,
    pub total_changes: usize,
    pub old_total_size: i64,
    pub old_total_size_formatted: String,
    pub old_item_count: usize,
    pub new_total_size: i64,
    pub new_total_size_formatted: String,
    pub new_item_count: usize,
    pub growth_percent: f64, // 正数=增长，负数=缩减
}

/// 计算两次扫描结果之间的差异
/// `old_items` 是较旧的扫描结果，`new_items` 是较新的
pub fn diff(old_items: &[Item], new_items: &[Item], old_total_size: i64) -> SnapshotDiff {
    // 建立路径 → Item 的索引
    let mut old_map: HashMap<&str, &Item> = HashMap::with_capacity(old_items.len());
    for item in old_items {
        old_map.insert(item.path.as_str(), item);
    }

    let mut added: Vec<DiffItem> = Vec::new();
    let mut removed: Vec<DiffItem> = Vec::new();
    let mut modified: Vec<DiffModifyItem> = Vec::new();
    let mut unchanged_count = 0usize;

    let mut new_map: HashMap<&str, &Item> = HashMap::with_capacity(new_items.len());

    // 遍历新结果，找出新增和修改的
    for new_item in new_items {
        new_map.insert(new_item.path.as_str(), new_item);

        if let Some(old_item) = old_map.get(new_item.path.as_str()) {
            // 文件存在于两边
            if old_item.size != new_item.size {
                // 大小变了
                let delta = new_item.size - old_item.size;
                modified.push(DiffModifyItem {
                    path: new_item.path.to_string(),
                    name: new_item.name.to_string(),
                    old_size: old_item.size,
                    new_size: new_item.size,
                    delta,
                    old_size_formatted: format_size(old_item.size).to_string(),
                    new_size_formatted: format_size(new_item.size).to_string(),
                    delta_formatted: format_delta(delta),
                    is_dir: new_item.is_dir,
                });
            } else {
                unchanged_count += 1;
            }
        } else {
            // 只在新结果中存在 → 新增
            added.push(DiffItem {
                path: new_item.path.to_string(),
                name: new_item.name.to_string(),
                size: new_item.size,
                size_formatted: format_size(new_item.size).to_string(),
                is_dir: new_item.is_dir,
            });
        }
    }

    // 遍历旧结果，找出删除的
    for old_item in old_items {
        if !new_map.contains_key(old_item.path.as_str()) {
            // 只在旧结果中存在 → 已删除
            removed.push(DiffItem {
                path: old_item.path.to_string(),
                name: old_item.name.to_string(),
                size: old_item.size,
                size_formatted: format_size(old_item.size).to_string(),
                is_dir: old_item.is_dir,
            });
        }
    }

    // 聚合统计
    let added_total_size: i64 = added.iter().map(|i| i.size).sum();
    let removed_total_size: i64 = removed.iter().map(|i| i.size).sum();
    let modified_delta: i64 = modified.iter().map(|m| m.delta).sum();
    let net_change = added_total_size - removed_total_size + modified_delta;

    let new_total_size: i64 = new_items.iter().map(|i| if !i.is_dir { i.size } else { 0 }).sum();

    let growth_percent = if old_total_size > 0 {
        (net_change as f64 / old_total_size as f64) * 100.0
    } else {
        0.0
    };

    // 按 size 降序排序
    added.sort_unstable_by(|a, b| b.size.cmp(&a.size));
    removed.sort_unstable_by(|a, b| b.size.cmp(&a.size));
    modified.sort_unstable_by(|a, b| b.delta.abs().cmp(&a.delta.abs()));

    SnapshotDiff {
        added_total_size,
        removed_total_size,
        modified_delta,
        net_change,
        unchanged_count,
        summary: DiffSummary {
            added_count: added.len(),
            removed_count: removed.len(),
            modified_count: modified.len(),
            total_changes: added.len() + removed.len() + modified.len(),
            old_total_size,
            old_total_size_formatted: format_size(old_total_size).to_string(),
            old_item_count: old_items.len(),
            new_total_size,
            new_total_size_formatted: format_size(new_total_size).to_string(),
            new_item_count: new_items.len(),
            growth_percent,
        },
        added,
        removed,
        modified,
    }
}

/// 格式化差值（带正负号）
fn format_delta(delta: i64) -> String {
    let abs = delta.abs();
    let formatted = format_size(abs);
    if delta >= 0 {
        format!("+{}", formatted)
    } else {
        format!("-{}", formatted)
    }
}

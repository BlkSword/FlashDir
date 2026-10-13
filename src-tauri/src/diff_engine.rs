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
    /// 大小或 mtime 发生变化的文件/目录（旧实现只看 size，等长覆盖写会漏报）
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
    /// 追加字段（向后兼容，老消费方忽略即可）：
    /// 本次修改是否由 mtime 变化引起（含"仅 mtime 变化"的情形，此时 delta == 0）。
    /// 追加而非替换，前端仍按原有字段渲染。
    pub mtime_changed: bool,
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
            // 文件存在于两边。
            //
            // 旧实现只用 size 判定"修改"：等长覆盖写（size 不变、内容已变）和
            // 仅 mtime 变化的文件都会被误判成"未变更"，增长/变更趋势因此漏报。
            // 现在 size 或 mtime 任一不同都算 modified。
            //
            // mtime 的坑：老快照可能没有 mtime（Item.mtime == 0 表示"未知"），
            // 若把"一方为 0"也当作不同，则与新快照做 diff 时所有历史文件都会被
            // 误报为 modified。因此仅当两侧 mtime 都非 0 且不相等时才按 mtime 判定。
            let size_changed = old_item.size != new_item.size;
            let mtime_changed = old_item.mtime != 0
                && new_item.mtime != 0
                && old_item.mtime != new_item.mtime;

            if size_changed || mtime_changed {
                // 仅 mtime 变化时 delta = 0：modified_delta / net_change / growth_percent
                // 的口径不变（字节数确实没变），只有 modified 明细与计数会多出这一条。
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
                    mtime_changed,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scan::CompactString;

    fn item(path: &str, size: i64, mtime: i64) -> Item {
        Item {
            path: CompactString::from(path),
            name: CompactString::from(path.rsplit('/').next().unwrap_or(path)),
            size,
            size_formatted: CompactString::new(),
            is_dir: false,
            mtime,
            atime: 0,
        }
    }

    /// 汇总口径自洽：net_change == added - removed + modified_delta，
    /// total_changes == added + removed + modified，且 unchanged 与各项不重叠。
    fn assert_consistent(d: &SnapshotDiff, old_len: usize, new_len: usize) {
        assert_eq!(
            d.net_change,
            d.added_total_size - d.removed_total_size + d.modified_delta,
            "net_change 必须等于 added - removed + modified_delta"
        );
        assert_eq!(d.summary.added_count, d.added.len());
        assert_eq!(d.summary.removed_count, d.removed.len());
        assert_eq!(d.summary.modified_count, d.modified.len());
        assert_eq!(
            d.summary.total_changes,
            d.added.len() + d.removed.len() + d.modified.len()
        );
        assert_eq!(d.summary.old_item_count, old_len);
        assert_eq!(d.summary.new_item_count, new_len);
        // 每个新项要么 added 要么 modified 要么未变更，三类互斥且合计等于新项数。
        assert_eq!(
            d.added.len() + d.modified.len() + d.unchanged_count,
            new_len,
            "added/modified/unchanged 必须刚好覆盖全部新项"
        );
    }

    #[test]
    fn size_change_is_modified() {
        let old = vec![item("C:/a.bin", 100, 1000)];
        let new = vec![item("C:/a.bin", 150, 2000)];
        let d = diff(&old, &new, 100);

        assert_eq!(d.modified.len(), 1, "size 变化必须判为 modified（回归基线）");
        assert_eq!(d.modified[0].delta, 50);
        assert_eq!(d.modified[0].old_size, 100);
        assert_eq!(d.modified[0].new_size, 150);
        assert!(d.modified[0].mtime_changed);
        assert_eq!(d.modified_delta, 50);
        assert_eq!(d.added.len(), 0);
        assert_eq!(d.removed.len(), 0);
        assert_eq!(d.unchanged_count, 0);
        assert_eq!(d.net_change, 50);
        assert!((d.summary.growth_percent - 50.0).abs() < 1e-9);
        assert_consistent(&d, 1, 1);
    }

    /// 核心缺陷回归：等长覆盖写（size 不变）以前被判为"未变更"。
    #[test]
    fn mtime_only_change_is_modified_with_zero_delta() {
        let old = vec![item("C:/a.txt", 4096, 1_700_000_000)];
        let new = vec![item("C:/a.txt", 4096, 1_700_000_600)];
        let d = diff(&old, &new, 4096);

        assert_eq!(
            d.modified.len(),
            1,
            "size 相同但 mtime 不同（等长覆盖写）必须判为 modified"
        );
        assert_eq!(d.modified[0].old_size, 4096);
        assert_eq!(d.modified[0].new_size, 4096);
        assert_eq!(d.modified[0].delta, 0, "仅 mtime 变化不产生字节差");
        assert!(d.modified[0].mtime_changed);
        assert_eq!(d.unchanged_count, 0);
        // 汇总口径不能被"零字节差异的 modified"破坏
        assert_eq!(d.modified_delta, 0);
        assert_eq!(d.net_change, 0);
        assert_eq!(d.summary.modified_count, 1);
        assert_eq!(d.summary.total_changes, 1);
        assert!((d.summary.growth_percent - 0.0).abs() < 1e-9);
        assert_consistent(&d, 1, 1);
    }

    /// 老快照 mtime=0（未知）不得被误判为修改。
    #[test]
    fn zero_mtime_is_not_treated_as_change() {
        // 旧快照无 mtime，新快照有
        let d1 = diff(
            &[item("C:/a.txt", 10, 0)],
            &[item("C:/a.txt", 10, 1_700_000_000)],
            10,
        );
        assert_eq!(d1.modified.len(), 0, "一方 mtime=0（未知）不能算修改");
        assert_eq!(d1.unchanged_count, 1);

        // 新快照无 mtime，旧快照有（反向同样不能算）
        let d2 = diff(
            &[item("C:/a.txt", 10, 1_700_000_000)],
            &[item("C:/a.txt", 10, 0)],
            10,
        );
        assert_eq!(d2.modified.len(), 0, "一方 mtime=0（未知）不能算修改");
        assert_eq!(d2.unchanged_count, 1);

        // 两侧都为 0：视为未变更
        let d3 = diff(&[item("C:/a.txt", 10, 0)], &[item("C:/a.txt", 10, 0)], 10);
        assert_eq!(d3.modified.len(), 0);
        assert_eq!(d3.unchanged_count, 1);

        // size 不同 + 一方 mtime 未知：仍必须按 size 判为 modified
        let d4 = diff(&[item("C:/a.txt", 10, 0)], &[item("C:/a.txt", 20, 0)], 10);
        assert_eq!(d4.modified.len(), 1);
        assert_eq!(d4.modified[0].delta, 10);
        assert!(!d4.modified[0].mtime_changed, "mtime 未参与判定");
    }

    #[test]
    fn added_and_removed_are_detected() {
        let old = vec![item("C:/a.txt", 100, 1), item("C:/gone.txt", 50, 1)];
        let new = vec![item("C:/a.txt", 100, 1), item("C:/new.txt", 200, 1)];
        let d = diff(&old, &new, 150);

        assert_eq!(d.added.len(), 1);
        assert_eq!(d.added[0].path, "C:/new.txt");
        assert_eq!(d.removed.len(), 1);
        assert_eq!(d.removed[0].path, "C:/gone.txt");
        assert_eq!(d.modified.len(), 0);
        assert_eq!(d.unchanged_count, 1);
        assert_eq!(d.added_total_size, 200);
        assert_eq!(d.removed_total_size, 50);
        assert_eq!(d.net_change, 150);
        assert_eq!(d.summary.new_total_size, 300);
        assert_consistent(&d, 2, 2);
    }

    #[test]
    fn net_change_and_growth_percent_are_self_consistent() {
        let old = vec![item("C:/a.bin", 600, 1), item("C:/b.bin", 400, 1)];
        let new = vec![
            item("C:/a.bin", 900, 2),  // modified +300
            item("C:/d.bin", 200, 1),  // added +200
        ];
        let d = diff(&old, &new, 1000);

        assert_eq!(d.added_total_size, 200);
        assert_eq!(d.removed_total_size, 400); // b.bin 消失
        assert_eq!(d.modified_delta, 300);
        assert_eq!(d.net_change, 100);
        assert_eq!(d.summary.new_total_size, 1100);
        assert!((d.summary.growth_percent - 10.0).abs() < 1e-9);
        assert_consistent(&d, 2, 2);

        // old_total_size == 0（旧快照为空/未知）不能除零
        let empty_old: Vec<Item> = Vec::new();
        let d2 = diff(&empty_old, &[item("C:/a.bin", 10, 1)], 0);
        assert_eq!(d2.summary.growth_percent, 0.0);
        assert_eq!(d2.net_change, 10);
        assert_consistent(&d2, 0, 1);
    }

    /// 现状说明：diff 不做路径级去重，重复 path 按出现次数各计一次。
    ///
    /// 真实扫描结果（scan / MFT / USN）里 path 唯一，因此线上口径不受影响；
    /// 这里把现状固化下来，避免"看起来修好了"的误解。若要改变该行为，
    /// 需要连同 summary.new_item_count 的口径一起调整。
    #[test]
    fn duplicate_paths_are_counted_per_occurrence_documented() {
        let old = vec![item("C:/a.txt", 100, 1)];
        let new = vec![item("C:/a.txt", 150, 1), item("C:/a.txt", 150, 1)];
        let d = diff(&old, &new, 100);

        // 现状：同一 path 出现两次 → modified 里两条（不是一条）
        assert_eq!(d.modified.len(), 2, "现状：重复 path 不去重，按出现次数计数");
        assert_eq!(d.modified[0].path, d.modified[1].path);
        // 现状：new_item_count 取的是新结果条数（含重复），与明细条数口径一致
        assert_eq!(d.summary.new_item_count, 2);
        assert_consistent(&d, 1, 2);

        // 只在新结果里重复、旧结果没有 → added 同样是两条
        let d2 = diff(&[], &[item("C:/x.txt", 10, 1), item("C:/x.txt", 10, 1)], 0);
        assert_eq!(d2.added.len(), 2);
        assert_eq!(d2.added_total_size, 20);
        assert_consistent(&d2, 0, 2);
    }

    #[test]
    fn sort_orders_are_stable_contracts() {
        let old = vec![item("C:/a.bin", 10, 1)];
        let new = vec![
            item("C:/a.bin", 30, 1), // modified delta +20
            item("C:/big.bin", 500, 1),
            item("C:/small.bin", 5, 1),
        ];
        let d = diff(&old, &new, 10);
        assert_eq!(d.added[0].path, "C:/big.bin", "added 按 size 降序");
        let old2 = vec![item("C:/g1.bin", 7, 1), item("C:/g2.bin", 70, 1)];
        let d2 = diff(&old2, &[], 77);
        assert_eq!(d2.removed[0].path, "C:/g2.bin", "removed 按 size 降序");
        assert_eq!(d.modified[0].path, "C:/a.bin");
    }
}

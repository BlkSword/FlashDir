//! 缺陷修复回归测试集
//!
//! 每个测试对应一个已确认缺陷；测试注释里写清"旧实现错在哪"，这样以后重构时
//! 若把行为改回去，会有一条具名测试直接失败，而不是靠人记住。
//!
//! 运行：`cargo test --lib tests_regression`
//!
//! 注意：模块声明处（lib.rs）已有 `#[cfg(test)]`，这里不再重复写内层
//! `#![cfg(test)]`（重复的 cfg 属性会触发 clippy 的 duplicated attribute 警告）。

use crate::global_search::{
    empty_instance_for_test, item_matches_filters_at, parse_search_filter, IndexEntry,
};

fn entry(path: &str, size: i64) -> IndexEntry {
    let name_lower = path
        .rsplit('/')
        .next()
        .unwrap_or(path)
        .to_lowercase();
    IndexEntry {
        path: path.to_string(),
        name_lower,
        size,
        is_dir: false,
        mtime: 0,
    }
}

/// 缺陷：`remove_path_internal` 用 `retain` 摘桶槽位，与 `swap_remove` 的搬移相互干扰。
///
/// 旧实现顺序是「先改写末位条目的槽位 last → idx，再对 removed 桶 retain 掉所有 == idx」。
/// 当被删条目与末位条目首字符同桶时，改写后两个槽位的值都等于 idx，`retain` 把两条一起
/// 摘掉：末位条目本身仍在 `entries` / `by_path` 中（全量扫描搜得到），却永久失去
/// `name_index` 桶槽位 —— 所有走首字符分桶快路径的 `前缀*` 查询都搜不到它，直到该路径
/// 被重新 upsert 或整盘重建索引。USN 增量删除走的正是这条路径，因此是真实可达数据缺陷。
#[test]
fn remove_keeps_moved_bucket_slot() {
    let idx = empty_instance_for_test();
    idx.upsert(entry("C:/z1.txt", 10));
    idx.upsert(entry("C:/a1.txt", 20));
    idx.upsert(entry("C:/a2.txt", 30));

    // 删除中间的 a1 → 末位条目 a2（首字符同桶 'a'）被 swap_remove 搬到 a1 的位置
    idx.remove_by_path("C:/a1.txt");

    // 首字符分桶快路径（存在正向前缀条件时启用）必须仍能搜到 a2
    let fast = idx.search_with_filter("a*", 10);
    assert_eq!(
        fast.len(),
        1,
        "同桶 swap_remove 后 moved 条目丢失桶槽位，前缀快路径漏结果: {:?}",
        fast.iter().map(|e| e.path.clone()).collect::<Vec<_>>()
    );
    assert!(fast[0].path.ends_with("a2.txt"));

    // 全量过滤路径（无前缀条件）不受分桶影响，应给出一致结果
    let full = idx.search_with_filter("a", 10);
    assert_eq!(full.len(), 1);
    assert!(full[0].path.ends_with("a2.txt"));

    // 再删一个同桶条目，确认反复搬移后桶槽位依然一致
    idx.upsert(entry("C:/a3.txt", 40));
    idx.remove_by_path("C:/a3.txt");
    assert_eq!(idx.search_with_filter("a*", 10).len(), 1);
}

/// 缺陷：多个裸词被 `join(" ")` 拼成**单个**短语，于是 `report 2024` 要求文件名里
/// 出现字面量 "report 2024"，几乎必然 0 结果 —— 与「组合：多条件同时满足」不符。
/// 现在每个裸词各自成为一个 AND 条件；需要短语匹配时用引号（分词阶段引号内不切分）。
#[test]
fn bare_words_are_independent_conditions_but_quotes_keep_phrases() {
    let and = parse_search_filter("report 2024");
    assert_eq!(and.len(), 2, "两个裸词应当是两个独立条件");
    assert!(item_matches_filters_at(
        "report_2024.pdf",
        "C:/d/report_2024.pdf",
        10,
        false,
        0,
        &and,
        0
    ));
    assert!(!item_matches_filters_at(
        "report.pdf",
        "C:/d/report.pdf",
        10,
        false,
        0,
        &and,
        0
    ));

    let phrase = parse_search_filter("\"report 2024\"");
    assert_eq!(phrase.len(), 1, "引号内应保持为一个完整短语");
    assert!(item_matches_filters_at(
        "report 2024.pdf",
        "C:/d/report 2024.pdf",
        10,
        false,
        0,
        &phrase,
        0
    ));
    assert!(!item_matches_filters_at(
        "report_2024.pdf",
        "C:/d/report_2024.pdf",
        10,
        false,
        0,
        &phrase,
        0
    ));
}

/// 缺陷：未识别的 `type:` 值被静默当成"只看文件"（`is_dir = false`），
/// 用户以为条件生效，实际拿到的是被偷偷过滤过的结果。现在未识别值不产生任何条件。
#[test]
fn unknown_type_value_is_ignored_not_treated_as_file() {
    let unknown = parse_search_filter("type:xyz");
    assert!(unknown.is_empty(), "未识别的 type 值不应产生任何条件");
    assert!(item_matches_filters_at("a.txt", "C:/d/a.txt", 1, false, 0, &unknown, 0));
    assert!(
        item_matches_filters_at("d", "C:/d", 0, true, 0, &unknown, 0),
        "目录也不应被排除"
    );

    let dir = parse_search_filter("type:dir");
    assert!(item_matches_filters_at("d", "C:/d", 0, true, 0, &dir, 0));
    assert!(!item_matches_filters_at("a.txt", "C:/d/a.txt", 1, false, 0, &dir, 0));

    let file = parse_search_filter("type:file");
    assert!(item_matches_filters_at("a.txt", "C:/d/a.txt", 1, false, 0, &file, 0));
    assert!(!item_matches_filters_at("d", "C:/d", 0, true, 0, &file, 0));
}

/// 缺陷：Mtime 条件在**每个条目**都调一次 `Utc::now()`（百万级索引 = 上百万次时钟读取）。
/// 现在"现在"由调用方传入；这里用显式 now 验证判定确实基于传入值而非墙上时钟。
#[test]
fn mtime_filter_uses_supplied_now() {
    let f = parse_search_filter("mtime:>1d");
    let now = 1_000_000i64;
    // 2 天前修改 → 大于 1 天 → 命中
    assert!(item_matches_filters_at("a", "C:/a", 1, false, now - 172_800, &f, now));
    // 1 小时前修改 → 不命中
    assert!(!item_matches_filters_at("a", "C:/a", 1, false, now - 3_600, &f, now));
}

/// 启动提速：`upsert_batch` 的并行批量构建（批量 ≥ PARALLEL_BUILD_MIN）必须与串行
/// 逐条插入语义**完全等价**。两条路径都要覆盖：
/// 1) 全部路径唯一 → 走并行构建：条目数、`name_index`（前缀快路径）、
///    `by_path`（按路径删除）都必须正确；
/// 2) 批内存在重复路径 → 并行构建检测到哈希不唯一后必须回退串行"替换"语义，
///    不能产生重复条目。
#[test]
fn bulk_build_matches_sequential_semantics() {
    const N: usize = 12_000;

    let idx = empty_instance_for_test();
    let batch: Vec<IndexEntry> = (0..N)
        .map(|i| entry(&format!("C:/bulk/dir_{:05}/file_{:05}.txt", i % 97, i), 100 + i as i64))
        .collect();
    idx.upsert_batch(batch);
    assert_eq!(idx.entries_len(), N, "唯一路径批量载入后条目数必须等于批量大小");

    // name_index（并行构建）与全量扫描必须给出同样多的命中
    let fast = idx.search_with_filter("file_00*", 10_000);
    let full = idx.search_with_filter("file_00", 10_000);
    assert!(!fast.is_empty());
    assert_eq!(
        fast.len(),
        full.len(),
        "并行构建的 name_index 与全量扫描结果不一致"
    );

    // by_path（并行构建）必须可用：按路径删除要真的删掉
    idx.remove_by_path("C:/bulk/dir_00000/file_00000.txt");
    assert_eq!(idx.entries_len(), N - 1);
    assert!(
        !idx.search_with_filter("file_00000", 10)
            .iter()
            .any(|e| e.path.ends_with("file_00000.txt")),
        "并行构建的 by_path 不可用，删除未生效"
    );

    // 批内重复路径：必须回退串行替换语义
    let dup = empty_instance_for_test();
    let mut batch: Vec<IndexEntry> = (0..N)
        .map(|i| entry(&format!("C:/dup/f_{:05}.txt", i), 1))
        .collect();
    batch[N - 1] = entry("C:/dup/f_00000.txt", 999);
    dup.upsert_batch(batch);
    assert_eq!(dup.entries_len(), N - 1, "重复路径必须只保留一个条目");
    let hit = dup.search_with_filter("f_00000", 10);
    assert_eq!(hit.len(), 1);
    assert_eq!(hit[0].size, 999, "重复路径应保留最后一次写入的数据");
}

/// offset 分页结果缓存（P1-1）的正确性：
/// 缓存只能"少扫几次"，绝不能改变结果本身。这里覆盖三种最容易出错的场景 ——
/// 分页拼接与一次性取数是否逐条一致、缓存窗口小于请求量时是否会少给结果、
/// 索引增删后是否立刻失效。
#[test]
fn paged_search_cache_keeps_results_identical_and_invalidates() {
    let idx = empty_instance_for_test();
    for i in 0..25 {
        idx.upsert(entry(&format!("C:/docs/report_{i:02}.pdf"), 100 + i));
    }

    // 一次性取全量作为基准
    let full: Vec<String> = idx
        .search_with_filter("report", 100)
        .into_iter()
        .map(|e| e.path)
        .collect();
    assert_eq!(full.len(), 25);

    // 分页拼接必须与一次性结果完全一致（顺序、条数、内容都不能变）
    let mut paged: Vec<String> = Vec::new();
    for p in 0..9usize {
        let (page, total) = idx.search_with_filter_paged("report", 3, p * 3);
        assert_eq!(total, 25, "命中总数必须与 limit/offset 无关");
        paged.extend(page.into_iter().map(|e| e.path));
    }
    assert_eq!(paged, full, "分页拼接必须与一次性取数逐条一致（缓存不得漏项或乱序）");

    // 缓存窗口小于请求量时必须重取，而不是少给结果
    // （真实场景：命令面板 limit=60 → 结果视图 limit=1000）
    let idx2 = empty_instance_for_test();
    for i in 0..30 {
        idx2.upsert(entry(&format!("C:/logs/log_{i:02}.txt"), 10 + i));
    }
    let (first, total1) = idx2.search_with_filter_paged("log", 10, 0);
    assert_eq!(first.len(), 10);
    assert_eq!(total1, 30);
    let (bigger, total2) = idx2.search_with_filter_paged("log", 25, 0);
    assert_eq!(bigger.len(), 25, "缓存窗口不足时必须重取，不能返回不足一页");
    assert_eq!(total2, 30);

    // 索引发生增删必须让缓存立刻失效
    idx.remove_by_path("C:/docs/report_00.pdf");
    let (after, total_after) = idx.search_with_filter_paged("report", 30, 0);
    assert_eq!(total_after, 24, "删除后命中总数必须更新");
    assert!(
        !after.iter().any(|e| e.path.ends_with("report_00.pdf")),
        "缓存未随索引变化失效，仍返回已删除条目"
    );

    // 越界翻页：缓存已含全部命中时应回空页且 total 正确
    let (tail, total_tail) = idx.search_with_filter_paged("report", 10, 1000);
    assert!(tail.is_empty());
    assert_eq!(total_tail, 24);
}

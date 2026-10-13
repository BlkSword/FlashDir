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
    empty_instance_for_test, item_matches_filters_at, parse_search_filter, FilterOp, GlobalIndex,
    IndexEntry, SearchFilter, SearchFilterKind,
};

fn entry(path: &str, size: i64) -> IndexEntry {
    let name_lower = path
        .rsplit('/')
        .next()
        .unwrap_or(path)
        .to_lowercase()
        .into();
    IndexEntry::new(path.to_string(), name_lower, size, false, 0)
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

/// 回归：`IndexEntry::path_ascii` 标记（`dir:` / 粘贴完整路径这两条"整条路径包含"的
/// 热路径用它免掉逐条 `is_ascii()` 全串扫描）不得改变匹配语义。
///
/// 带标记的快路径在 `path_ascii == true` 时，字节窗口扫描未命中就直接返回 false，
/// 不再对路径做一次 `is_ascii()` 扫描（310 万条真实索引上，串行扫描实测 562ms → 506ms，
/// 其中约 56ms 正是 294 万条 ASCII 路径的 `is_ascii()`）。
/// 风险点在于"ASCII 字节窗口比较是否真的覆盖了全部折叠可能"：非 ASCII 大小写
/// （西里尔/希腊/İ）必须仍然走 Unicode 折叠回退，否则会静默漏结果。
/// 这里对每个条目、每个查询词用参考实现 `path.to_lowercase().contains(needle_lower)`
/// 逐条对拍（参考实现 = 旧语义），并验证构造函数确实按 `path` 算出了标记。
#[test]
fn path_ascii_flag_keeps_path_contains_semantics() {
    // (路径, 查询词)。每个词都会走 `dir:词`；词里含分隔符时再加一条"裸粘贴路径"
    // （parse 后是 `PathText`）——两条正是本次改动的分支。
    let cases: &[(&str, &[&str])] = &[
        // 纯 ASCII 大小写混合 → 走带标记快路径（不扫 is_ascii、不分配）
        (
            "C:/Windows/System32/x.dll",
            &["windows", "system32/x.dll", "x.DLL", "linux"],
        ),
        // CJK：折叠是恒等变换，needle 无大小写字符 → 朴素包含
        ("C:/用户/文档/a.txt", &["文档", "用户/文档", "图片"]),
        // 西里尔：非 ASCII 且真正区分大小写 → 必须仍然命中（靠折叠回退）
        (
            "C:/ПАПКА/файл.txt",
            &["папка", "ПАПКА/ФАЙЛ", "файл", "ящик"],
        ),
        // 德语 ß：`to_lowercase` 不会变成 "ss"，因此 "strasse" 不命中（与参考实现一致）
        ("C:/straße/a.txt", &["straße", "strasse", "a.txt"]),
        // İ (U+0130) 折叠为 "i" + 组合点：needle "i" 只有折叠之后才可能命中
        ("C:/İstanbul/a.txt", &["i", "İSTANBUL", "z"]),
        // 非 ASCII 段与 ASCII 段大小写混合
        ("C:/MiXeD/ÄÖÜ.txt", &["äöü", "mixed/äöü", "xyz"]),
    ];

    let idx = empty_instance_for_test();
    for &(path, _) in cases {
        let name_lower = path.rsplit('/').next().unwrap_or(path).to_lowercase().into();
        idx.upsert(IndexEntry::new(path.to_string(), name_lower, 1, false, 0));
    }

    let mut ascii_paths = 0usize;
    let mut non_ascii_paths = 0usize;
    for &(path, needles) in cases {
        // 构造函数必须真的派生标记：漏算/写死都会让热路径判错
        let probe = IndexEntry::new(path.to_string(), String::new().into(), 1, false, 0);
        assert_eq!(
            probe.path_ascii,
            path.is_ascii(),
            "path_ascii 必须等于 path.is_ascii(): {path:?}"
        );
        if probe.path_ascii {
            ascii_paths += 1;
        } else {
            non_ascii_paths += 1;
        }

        for &needle in needles {
            // 参考实现：旧语义 = 整条路径小写化后做包含（查询值在 parse 阶段已小写化）
            let reference = path.to_lowercase().contains(&needle.to_lowercase());
            let found = |q: &str| {
                idx.search_with_filter(q, 100)
                    .iter()
                    .any(|e| e.path.as_str() == path)
            };

            assert_eq!(
                found(&format!("dir:{needle}")),
                reference,
                "dir: 查询与参考实现不一致: path={path:?} needle={needle:?}"
            );
            if needle.contains('/') {
                assert_eq!(
                    found(needle),
                    reference,
                    "粘贴路径(PathText)与参考实现不一致: path={path:?} needle={needle:?}"
                );
            }
        }
    }

    // 两类路径都必须覆盖到，否则这个测试证明不了"标记不改变语义"
    assert!(ascii_paths > 0, "必须覆盖纯 ASCII 路径");
    assert!(non_ascii_paths > 0, "必须覆盖非 ASCII 路径");

    // 防止"两边都是 false"的假对拍：西里尔/İ 这两类必须真的能搜到
    assert!(
        idx.search_with_filter("dir:ПАПКА/ФАЙЛ", 100)
            .iter()
            .any(|e| e.path == "C:/ПАПКА/файл.txt"),
        "非 ASCII 大小写折叠场景漏结果（带标记快路径误判为 ASCII 结论）"
    );
    assert!(
        idx.search_with_filter("dir:i", 100)
            .iter()
            .any(|e| e.path.starts_with("C:/İstanbul")),
        "İ → i 折叠边界漏结果"
    );
    assert!(
        idx.search_with_filter("dir:молоко", 100).is_empty(),
        "不相关查询不该命中"
    );
}

// ─────────────────────── `dir:` 加速的等价性判据 ───────────────────────
//
// `dir:S` 的语义是**整条路径**做大小写不敏感的包含匹配（`contains_ignore_case(path, S)`），
// 不是"某个目录段等于 S"，也不是"某个目录段以 S 开头"：`dir:win` 必须命中
// `C:/mywindows/x.txt`（段内子串），`dir:s/p` 必须命中跨分隔符的 `C:/projects/p.txt`。
//
// 因此任何"dir: 加速"（段索引 / 前缀索引 / 预过滤位图）都只是在 narrowing 候选集：
// 它必须对**所有**输入给出与全量扫描完全相同的命中集合与命中总数。下面的随机化测试
// 就是这个判据：合成数据 + 随机变异 + 独立模型暴力对拍，逐条比较排序后的路径列表。

/// 固定种子 xorshift64。变异序列必须完全可复现：失败时能原样重跑定位，
/// 而不是"偶发失败、下次就绿了"。
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Rng(seed | 1)
    }
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
    fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            0
        } else {
            (self.next() % n as u64) as usize
        }
    }
    fn pick<T: Copy>(&mut self, items: &[T]) -> T {
        items[self.below(items.len())]
    }
}

/// 目录段池：同名不同层（tools / Tools / TOOLS）、大小写混排、CJK、西里尔。
const DIR_POOL: &[&str] = &[
    "projects",
    "Projects",
    "tools",
    "Tools",
    "TOOLS",
    "src",
    "SRC",
    "bin",
    "Bin",
    "node_modules",
    "Temp",
    "temp",
    "Windows",
    "windows",
    "WINdows",
    "文档",
    "报告",
    "ПАПКА",
    "папка",
    "Users",
];

const FILE_POOL: &[&str] = &[
    "README.md",
    "readme.MD",
    "main.rs",
    "MAIN.RS",
    "notes.txt",
    "报告.docx",
    "файл.txt",
    "WINDOWS.LOG",
    "win_dows.txt",
    "apache.sys",
    "a.bin",
    "MixedCase.TXT",
];

/// 生成一条合成路径：1~4 层目录 + 文件名。
/// 40% 落在公共祖先 `C:/projects`（随机大小写）子树下 —— 覆盖"单个目录名是多条路径的
/// 公共祖先"（子树场景），它是 `remove_prefix_internal` 与任何段索引的主要风险点。
fn gen_path(rng: &mut Rng) -> String {
    let mut p = String::from("C:/");
    let depth = 1 + rng.below(4);
    for d in 0..depth {
        if d == 0 && rng.below(10) < 4 {
            p.push_str(if rng.below(2) == 0 { "projects" } else { "PROJECTS" });
        } else {
            p.push_str(rng.pick(DIR_POOL));
        }
        p.push('/');
    }
    p.push_str(rng.pick(FILE_POOL));
    p
}

fn dir_entry(path: &str, is_dir: bool) -> IndexEntry {
    let name_lower = path.rsplit('/').next().unwrap_or(path).to_lowercase().into();
    IndexEntry::new(path.to_string(), name_lower, path.len() as i64, is_dir, 0)
}

fn model_upsert(model: &mut Vec<String>, path: &str) {
    if !model.iter().any(|p| p == path) {
        model.push(path.to_string());
    }
}

fn model_remove(model: &mut Vec<String>, path: &str) {
    model.retain(|p| p != path);
}

/// 与 `global_search::is_same_or_child` 同语义（该函数是模块私有的，测试无法直接调用；
/// 这里刻意保持逐字一致 —— 一旦它与实现分叉，`entries_len()` 会立刻对不上而失败）。
fn is_same_or_child_mirror(base: &str, key: &str) -> bool {
    if key.eq_ignore_ascii_case(base) {
        return true;
    }
    let child_prefix = if base.ends_with('/') {
        base.to_string()
    } else {
        format!("{}/", base)
    };
    key.get(..child_prefix.len())
        .is_some_and(|head| head.eq_ignore_ascii_case(&child_prefix))
}

/// 暴力参考实现：旧语义（也是 `dir:` 的定义）= 整条路径小写化后包含查询词。
fn reference_hits(model: &[String], needle_lower: &str) -> Vec<String> {
    let mut v: Vec<String> = model
        .iter()
        .filter(|p| p.to_lowercase().contains(needle_lower))
        .cloned()
        .collect();
    v.sort();
    v
}

/// 一次完整对拍：命中集合逐条一致 + 命中总数一致 + arena 条目数与模型一致。
fn assert_dir_query_matches_model(idx: &GlobalIndex, model: &[String], needle: &str, ctx: &str) {
    assert_eq!(
        idx.entries_len(),
        model.len(),
        "{ctx}: arena 条目数与独立模型不一致（重复条目或丢失条目）"
    );
    let needle_lower = needle.to_lowercase();
    let (hits, total) = idx.search_with_filter_paged(&format!("dir:{needle}"), 100_000, 0);
    let mut got: Vec<String> = hits.into_iter().map(|e| e.path).collect();
    got.sort();
    let want = reference_hits(model, &needle_lower);
    assert_eq!(
        got, want,
        "{ctx}: dir:{needle} 命中集合与暴力参考实现不一致（加速实现漏结果/多结果）"
    );
    assert_eq!(
        total,
        want.len(),
        "{ctx}: dir:{needle} 命中总数不一致（total 由另一个分支统计）"
    );
}

/// 覆盖要求场景的固定查询词：段内子串、跨分隔符、大小写混排、CJK、西里尔、必然 0 命中。
const DIR_NEEDLES: &[&str] = &[
    "projects",
    "WiNdOwS",
    "ndow",
    "s/p",
    "projects/tools",
    "s/tools",
    "文档",
    "报告",
    "ПАПКА",
    "папка/файл",
    "ПРОЕКТЫ",
    "node_modules",
    "MixedCase",
    "n",
    "i",
];

#[test]
fn dir_query_equivalence_under_random_mutations() {
    let mut rng = Rng::new(0x5EED_2026_D17A_11C3);
    let idx = empty_instance_for_test();
    let mut model: Vec<String> = Vec::new();

    // ── 初始数据：2000 条合成路径（含重复，走"替换"分支）
    let mut batch: Vec<IndexEntry> = Vec::with_capacity(2000);
    for i in 0..2000 {
        let path = gen_path(&mut rng);
        model_upsert(&mut model, &path);
        batch.push(dir_entry(&path, i % 5 == 0));
    }
    idx.upsert_batch(batch);
    assert_eq!(idx.entries_len(), model.len(), "初始批量载入后条目数应与模型一致");

    // 数据集自检：要求覆盖的场景必须真的出现，否则这个对拍证明不了什么
    {
        let total = model.len();
        assert!(total >= 1000, "初始数据集太小: {total}");
        let cjk = model.iter().filter(|p| p.contains("文档")).count();
        let cyr = model.iter().filter(|p| p.contains("ПАПКА") || p.contains("папка")).count();
        let subtree = model
            .iter()
            .filter(|p| p.starts_with("C:/projects/") || p.starts_with("C:/PROJECTS/"))
            .count();
        let lower_case_variant = model.iter().filter(|p| p.contains("/windows/")).count();
        let upper_case_variant = model.iter().filter(|p| p.contains("/Windows/")).count();
        // 同名目录出现在不同深度：统计 tools 出现的层级集合是否 > 1
        let depths: std::collections::BTreeSet<usize> = model
            .iter()
            .filter_map(|p| p.rfind("/tools/").map(|i| p[..i].matches('/').count()))
            .collect();
        assert!(cjk >= 5, "必须覆盖 CJK 段: {cjk}");
        assert!(cyr >= 5, "必须覆盖西里尔段: {cyr}");
        assert!(subtree >= 20, "必须覆盖公共祖先子树场景: {subtree}");
        assert!(lower_case_variant > 0 && upper_case_variant > 0, "必须覆盖大小写混排");
        assert!(depths.len() > 1, "同名目录必须出现在多个层级: {depths:?}");
    }

    // ── 随机变异：每轮之后全量对拍
    let mut checks = 0usize;
    let mut nonempty_hits = 0usize;
    let mut empty_hits = 0usize;

    for round in 0..80 {
        match rng.below(7) {
            // 新增：upsert 新路径（文件 + 目录混排）
            0 | 1 => {
                for _ in 0..1 + rng.below(3) {
                    let path = gen_path(&mut rng);
                    idx.upsert(dir_entry(&path, rng.below(3) == 0));
                    model_upsert(&mut model, &path);
                }
            }
            // 替换：upsert 已存在路径（arena 走"替换已有条目"分支，不新增下标）
            2 => {
                if !model.is_empty() {
                    for _ in 0..1 + rng.below(3) {
                        let path = model[rng.below(model.len())].clone();
                        idx.upsert(dir_entry(&path, rng.below(2) == 0));
                    }
                }
            }
            // 删除：remove_by_path（含 30% 不存在的路径，走"线性兜底 + 直接返回"）
            3 => {
                let path = if !model.is_empty() && rng.below(10) < 7 {
                    model[rng.below(model.len())].clone()
                } else {
                    gen_path(&mut rng)
                };
                idx.remove_by_path(&path);
                model_remove(&mut model, &path);
            }
            // 批量删除：remove_paths_batch（每条都触发一次 swap_remove，最容易踩搬移不变式）
            4 => {
                let mut paths = Vec::new();
                for _ in 0..1 + rng.below(4) {
                    if model.is_empty() {
                        break;
                    }
                    paths.push(model[rng.below(model.len())].clone());
                }
                idx.remove_paths_batch(&paths);
                for p in &paths {
                    model_remove(&mut model, p);
                }
            }
            // 批量 upsert：批内故意混入重复路径，逼走"替换"分支
            5 => {
                let mut batch = Vec::new();
                let mut touched = Vec::new();
                for _ in 0..1 + rng.below(5) {
                    let path = if !model.is_empty() && rng.below(10) < 4 {
                        model[rng.below(model.len())].clone()
                    } else {
                        gen_path(&mut rng)
                    };
                    touched.push(path.clone());
                    batch.push(dir_entry(&path, rng.below(3) == 0));
                }
                if rng.below(2) == 0 {
                    let dup = batch[rng.below(batch.len())].clone();
                    batch.push(dup);
                }
                idx.upsert_batch(batch);
                for p in &touched {
                    model_upsert(&mut model, p);
                }
            }
            // 子树删除（remove_prefix_internal，生产上由 add_items / USN 目录改名触发）
            // 与整体清空（clear_internal）。
            _ => {
                if rng.below(6) == 0 || model.is_empty() {
                    idx.set_loading();
                    model.clear();
                } else {
                    // 前缀取自模型里真实存在的路径 → 一定是"有多条后代"的子树；
                    // 大小写与前导/末尾分隔符随机化，覆盖 is_same_or_child 的折叠比较。
                    let base = model[rng.below(model.len())].clone();
                    let mut cuts = vec![2usize.min(base.len())];
                    for (i, c) in base.char_indices() {
                        if c == '/' {
                            cuts.push(i);
                        }
                    }
                    let cut = cuts[rng.below(cuts.len())];
                    let mut prefix = base[..cut].to_string();
                    if rng.below(3) == 0 {
                        prefix = prefix.to_uppercase();
                    }
                    if rng.below(3) == 0 {
                        prefix.push('/');
                    }
                    idx.remove_prefix_for_test(&prefix);
                    model.retain(|p| !is_same_or_child_mirror(&prefix, p));
                }
            }
        }

        // 固定词 + 从模型里随机切出的子串（保证既有多命中也有 0 命中）
        let mut needles: Vec<String> = DIR_NEEDLES.iter().map(|s| s.to_string()).collect();
        for _ in 0..3 {
            if model.is_empty() {
                break;
            }
            let lower = model[rng.below(model.len())].to_lowercase();
            let start = rng.below(lower.len());
            let end = (start + 1 + rng.below(7)).min(lower.len());
            if !lower.is_char_boundary(start) || !lower.is_char_boundary(end) {
                continue;
            }
            let mut needle: String = lower[start..end]
                .chars()
                .filter(|c| !c.is_whitespace() && *c != '"' && *c != '!')
                .collect();
            while needle.ends_with('/') {
                needle.pop();
            }
            if !needle.is_empty() {
                needles.push(needle);
            }
        }

        for needle in &needles {
            assert_dir_query_matches_model(&idx, &model, needle, &format!("round {round}"));
            checks += 1;
            if reference_hits(&model, &needle.to_lowercase()).is_empty() {
                empty_hits += 1;
            } else {
                nonempty_hits += 1;
            }
        }
    }

    // 防假对拍：必须真的跑到过量、必须同时出现"命中"与"不命中"两种查询
    assert!(checks >= 1000, "对拍次数过少: {checks}");
    assert!(nonempty_hits > 0 && empty_hits > 0, "对拍必须覆盖命中与不命中: {nonempty_hits}/{empty_hits}");

    // ── 首次全量载入的并行构建分支（batch ≥ PARALLEL_BUILD_MIN 且索引为空）：
    // 该分支自己并行构建 by_path / name_index，dir: 结果必须与串行插入完全一致
    {
        let mut rng = Rng::new(0xBADC_0FFE_5EED);
        let fresh = empty_instance_for_test();
        let mut model: Vec<String> = Vec::new();
        let mut batch = Vec::with_capacity(11_000);
        for i in 0..11_000 {
            let path = gen_path(&mut rng);
            model_upsert(&mut model, &path);
            batch.push(dir_entry(&path, i % 3 == 0));
        }
        fresh.upsert_batch(batch);
        for needle in ["projects", "文档", "папка", "Tools", "n", "s/projects", "windows"] {
            assert_dir_query_matches_model(&fresh, &model, needle, "parallel-build");
        }
    }
}

// ─────────────────── 索引内存布局（目录前缀驻留）的等价性判据 ───────────────────
//
// 目标：把"每条一份完整路径字符串"（`IndexEntry.path`，占索引堆内存一半以上）改成
// 「目录表 + (目录 id, 文件名)」。存档布局改动**不允许**改变任何一条查询语义，
// 因为它同时被过滤（`dir:` / 粘贴完整路径）、排序（同分同名的 path 次序）、
// 删除（`swap_remove` 搬移末位条目）、前缀子树操作消费。
//
// 判据与 `dir_query_equivalence_under_random_mutations` 同构，但覆盖面扩大到**全部**
// 过滤种类与排序结果：建索引 → 随机变异（upsert 新增/替换、`remove_by_path`、
// `remove_paths_batch`、`upsert_batch`（批内含重复）、前缀子树删除、清空）→ 每一步
// 与**独立暴力参考模型**对拍：每类查询的（排序后命中路径列表 + 命中总数 + `entries_len`）。
//
// 参考模型只保留过滤语义真正读到的字段，匹配逻辑按"文档语义"独立重写
// （路径类条件一律 `path.to_lowercase().contains(needle)`，与旧实现同义），
// 不调用被改造模块里的任何匹配函数。

/// 参考模型里的一条：过滤语义只会读这 4 个字段（name / name_lower / ext 都能派生）。
#[derive(Clone)]
struct ModelEntry {
    path: String,
    size: i64,
    is_dir: bool,
    mtime: i64,
}

impl ModelEntry {
    /// 与写入侧同义：小写文件名（末段）。
    fn name_lower(&self) -> String {
        self.path
            .rsplit('/')
            .next()
            .unwrap_or(&self.path)
            .to_lowercase()
    }
}

fn cmp_op(a: i64, op: FilterOp, b: i64) -> bool {
    match op {
        FilterOp::Gt => a > b,
        FilterOp::Gte => a >= b,
        FilterOp::Lt => a < b,
        FilterOp::Lte => a <= b,
        FilterOp::Eq => a == b,
        FilterOp::Ne => a != b,
    }
}

/// 独立实现的过滤求值（文档语义），`now` 由调用方传入。
fn model_matches(m: &ModelEntry, filters: &[SearchFilter], now: i64) -> bool {
    let name_lower = m.name_lower();
    for f in filters {
        let matched = match &f.kind {
            SearchFilterKind::Text(t) | SearchFilterKind::Name(t) => {
                name_lower.contains(t.as_str())
            }
            // 路径类条件：整条路径做大小写不敏感包含（旧实现 = 文档语义）
            SearchFilterKind::PathText(p) | SearchFilterKind::Dir(p) => {
                m.path.to_lowercase().contains(p.as_str())
            }
            SearchFilterKind::Prefix(p) => name_lower.starts_with(p.as_str()),
            SearchFilterKind::Suffix(s) => name_lower.ends_with(s.as_str()),
            SearchFilterKind::Ext(e) => {
                !m.is_dir
                    && name_lower
                        .rsplit_once('.')
                        .map(|(_, ext)| ext == e.as_str())
                        .unwrap_or(false)
            }
            SearchFilterKind::Type { is_dir } => m.is_dir == *is_dir,
            SearchFilterKind::Size { op, bytes } => cmp_op(m.size, *op, *bytes),
            SearchFilterKind::Mtime { op, seconds } => cmp_op(now - m.mtime, *op, *seconds),
        };
        if matched == f.negate {
            return false;
        }
    }
    true
}

/// 暴力参考：排序后的命中路径列表。
fn model_hits(model: &[ModelEntry], query: &str, now: i64) -> Vec<String> {
    let filters = parse_search_filter(query);
    // 条件为空（例如只有 `AND`，或 `type:xyz` 这类被忽略的值）→ 实现返回空结果，
    // 绝不退化成"全量按体积取前 N"（见 search_internal 的注释）。
    if filters.is_empty() {
        return Vec::new();
    }
    let mut hits: Vec<String> = model
        .iter()
        .filter(|m| model_matches(m, &filters, now))
        .map(|m| m.path.clone())
        .collect();
    hits.sort();
    hits
}

fn model_upsert_mixed(model: &mut Vec<ModelEntry>, e: &ModelEntry) {
    match model.iter_mut().find(|m| m.path == e.path) {
        Some(slot) => *slot = e.clone(),
        None => model.push(e.clone()),
    }
}

/// 一次完整对拍：命中集合逐条一致 + 命中总数一致 + arena 条目数与模型一致。
fn assert_query_matches_model(
    idx: &GlobalIndex,
    model: &[ModelEntry],
    query: &str,
    now: i64,
    ctx: &str,
) {
    assert_eq!(
        idx.entries_len(),
        model.len(),
        "{ctx}: entry 数与独立模型不一致（重复条目或丢失条目）"
    );
    let (hits, total) = idx.search_with_filter_paged(query, 100_000, 0);
    let mut got: Vec<String> = hits.into_iter().map(|e| e.path).collect();
    got.sort();
    let want = model_hits(model, query, now);
    assert_eq!(
        got, want,
        "{ctx}: 查询 {query:?} 命中集合与暴力参考实现不一致（漏结果 / 多结果 / 顺序外内容不同）"
    );
    assert_eq!(
        total,
        want.len(),
        "{ctx}: 查询 {query:?} 命中总数不一致（total 由另一条分支统计）"
    );
}

/// 参考模型的相关性分数（文档语义：相关性降序 → name_lower 升序 → 完整路径升序）。
///
/// `q_lower` = 第一个**非否定**文本条件的值（没有则空串 → 分数就是体积）。
fn model_q_lower(query: &str) -> String {
    parse_search_filter(query)
        .iter()
        .find_map(|f| match &f.kind {
            SearchFilterKind::Text(t) if !f.negate => Some(t.to_lowercase()),
            _ => None,
        })
        .unwrap_or_default()
}

fn model_score(m: &ModelEntry, q_lower: &str) -> i64 {
    if q_lower.is_empty() {
        return m.size;
    }
    let name_lower = m.name_lower();
    let base = if name_lower == q_lower {
        3i64 << 60
    } else if name_lower.starts_with(q_lower) {
        2i64 << 60
    } else {
        1i64 << 60
    };
    base + m.size
}

/// 参考模型的命中按文档次序排好：(路径, 分数, name_lower)。
fn model_ranked(model: &[ModelEntry], query: &str, now: i64) -> Vec<(String, i64, String)> {
    let filters = parse_search_filter(query);
    if filters.is_empty() {
        return Vec::new();
    }
    let q_lower = model_q_lower(query);
    let mut ordered: Vec<(String, i64, String)> = model
        .iter()
        .filter(|m| model_matches(m, &filters, now))
        .map(|m| (m.path.clone(), model_score(m, &q_lower), m.name_lower()))
        .collect();
    ordered.sort_by(|(pa, sa, na), (pb, sb, nb)| {
        sb.cmp(sa).then_with(|| na.cmp(nb)).then_with(|| pa.cmp(pb))
    });
    ordered
}

/// 小 limit 对拍：**取到的候选集**与**展示次序**都必须符合文档契约。
///
/// 契约：
/// 1. **候选集**：取到的就是"按相关性 / `name_lower` 排序的前 K 名" —— `(score, name_lower)`
///    主键序列必须逐项等于参考模型的前 K 名（内存布局改造不允许改变"哪些条目进结果集"）；
/// 2. **展示次序**：每个主键相同的连续段内，路径必须升序（排序定义的第三关键字）；
/// 3. 每条命中都必须属于参考模型的命中集合。
///
/// 不断言"同分同名候选里具体是哪几条"：这些候选在文档排序下等价（相关性判据里没有路径
/// 这一维），任何实现都只需保证"主键序列 + 组内路径升序"。
fn assert_topk_matches_model(
    idx: &GlobalIndex,
    model: &[ModelEntry],
    query: &str,
    now: i64,
    k: usize,
    ctx: &str,
) {
    let ranked = model_ranked(model, query, now);
    let want: Vec<(String, i64, String)> = ranked.iter().take(k).cloned().collect();
    let (hits, _total) = idx.search_with_filter_paged(query, k, 0);
    let got: Vec<String> = hits.into_iter().map(|e| e.path).collect();
    assert_eq!(
        got.len(),
        want.len(),
        "{ctx}: 查询 {query:?} 前 {k} 名条数不一致（截断位置错了）"
    );

    let q_lower = model_q_lower(query);
    let key_of = |p: &String| -> (i64, String) {
        let e = model
            .iter()
            .find(|m| &m.path == p)
            .unwrap_or_else(|| panic!("{ctx}: 查询 {query:?} 返回了参考模型里没有的路径 {p:?}"));
        (model_score(e, &q_lower), e.name_lower())
    };
    let got_keys: Vec<(i64, String)> = got.iter().map(key_of).collect();
    let want_keys: Vec<(i64, String)> = want.iter().map(|(_, s, n)| (*s, n.clone())).collect();
    assert_eq!(
        got_keys, want_keys,
        "{ctx}: 查询 {query:?} 前 {k} 名的相关性主键序列与参考模型不一致（选错了候选或次序）"
    );

    // 同主键段内必须按完整路径升序（`into_entries` 的最终重排）
    for i in 1..got.len() {
        if got_keys[i] == got_keys[i - 1] {
            assert!(
                got[i - 1] < got[i],
                "{ctx}: 查询 {query:?} 同分同名的候选未按完整路径升序: {:?} → {:?}",
                got[i - 1],
                got[i]
            );
        }
    }
}

/// 构造一条合成条目：路径来自 `gen_path`，体积/目录标志/时间随机。
fn mixed_entry(path: &str, is_dir: bool, size: i64, mtime: i64) -> (IndexEntry, ModelEntry) {
    let name_lower = path
        .rsplit('/')
        .next()
        .unwrap_or(path)
        .to_lowercase()
        .into();
    (
        IndexEntry::new(path.to_string(), name_lower, size, is_dir, mtime),
        ModelEntry {
            path: path.to_string(),
            size,
            is_dir,
            mtime,
        },
    )
}

/// 生成一条"贴近真实文件系统"的路径：60% 复用模型里已有目录（真实索引平均每目录约 7 条，
/// 目录前缀就是这么被共享出来的），其余走 `gen_path` 造新目录树。
/// 共享率直接决定"目录前缀驻留"能省多少内存，也决定这个判据是否有区分度。
fn gen_shared_path(rng: &mut Rng, model: &[ModelEntry]) -> String {
    if !model.is_empty() && rng.below(10) < 6 {
        let base = &model[rng.below(model.len())].path;
        let dir = match base.rsplit_once('/') {
            Some((d, _)) => d,
            None => base.as_str(),
        };
        if rng.below(3) == 0 {
            format!("{}/{}/{}", dir, rng.pick(DIR_POOL), rng.pick(FILE_POOL))
        } else {
            format!("{}/{}", dir, rng.pick(FILE_POOL))
        }
    } else {
        gen_path(rng)
    }
}

#[test]
fn search_semantics_equivalence_under_random_mutations() {
    let mut rng = Rng::new(0xD12E_C70A_5EED_2026);
    let idx = empty_instance_for_test();
    let mut model: Vec<ModelEntry> = Vec::new();

    // mtime 取值必须远离查询阈值（1d / 7d）：全部相对测试开始时刻，留出 ≥23 天的余量，
    // 否则跨秒边界会让"参考模型"与"实现各取一次 now"出现假失败。
    let now0 = chrono::Utc::now().timestamp();
    const DAY: i64 = 24 * 60 * 60;
    let mtimes = [0i64, now0 - 30 * DAY, now0 - 60, now0 - 2 * DAY];
    let sizes = [0i64, 100, 4096, 1024 * 1024, 3 * 1024 * 1024 * 1024];

    let mut batch: Vec<IndexEntry> = Vec::with_capacity(2000);
    for i in 0..2000 {
        let path = gen_shared_path(&mut rng, &model);
        let (e, m) = mixed_entry(
            &path,
            i % 5 == 0,
            sizes[rng.below(sizes.len())],
            mtimes[rng.below(mtimes.len())],
        );
        model_upsert_mixed(&mut model, &m);
        batch.push(e);
    }
    idx.upsert_batch(batch);
    assert_eq!(idx.entries_len(), model.len(), "初始批量载入后条目数应与模型一致");

    // 数据集自检：必须真的存在"多条路径共享同一个目录前缀"，否则这个对拍证明不了什么
    {
        let mut per_dir: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
        let mut cross_slash = 0usize;
        for m in &model {
            let dir = m.path.rsplit_once('/').map(|(d, _)| d).unwrap_or("");
            *per_dir.entry(dir).or_insert(0) += 1;
            cross_slash += m.path.matches('/').count();
        }
        let shared = model
            .iter()
            .filter(|m| {
                per_dir[m.path.rsplit_once('/').map(|(d, _)| d).unwrap_or("")] > 1
            })
            .count();
        assert!(
            shared * 2 > model.len(),
            "数据集必须大量共享目录前缀（目录前缀驻留的收益来源）: {shared}/{}",
            model.len()
        );
        assert!(cross_slash > model.len(), "路径必须有多层目录");
        assert!(per_dir.len() < model.len(), "目录数应少于条目数: {} 个目录", per_dir.len());
        assert!(model.iter().filter(|m| !m.path.is_ascii()).count() >= 5, "必须覆盖非 ASCII 路径");
    }

    let paste_targets = |rng: &mut Rng, model: &[ModelEntry]| -> Vec<String> {
        let mut v = Vec::new();
        for _ in 0..2 {
            if !model.is_empty() {
                v.push(model[rng.below(model.len())].path.clone());
            }
        }
        v
    };

    let mut checks = 0usize;
    let mut nonempty_hits = 0usize;
    let mut empty_hits = 0usize;

    for round in 0..60 {
        match rng.below(8) {
            // 新增：upsert 新路径（文件 + 目录混排）
            0 | 1 => {
                for _ in 0..1 + rng.below(3) {
                    let path = gen_shared_path(&mut rng, &model);
                    let (e, m) = mixed_entry(
                        &path,
                        rng.below(3) == 0,
                        sizes[rng.below(sizes.len())],
                        mtimes[rng.below(mtimes.len())],
                    );
                    idx.upsert(e);
                    model_upsert_mixed(&mut model, &m);
                }
            }
            // 替换：upsert 已存在路径（换体积/换目录标志，走"替换已有条目"分支）
            2 => {
                if !model.is_empty() {
                    for _ in 0..1 + rng.below(3) {
                        let path = model[rng.below(model.len())].path.clone();
                        let (e, m) = mixed_entry(
                            &path,
                            rng.below(2) == 0,
                            sizes[rng.below(sizes.len())],
                            mtimes[rng.below(mtimes.len())],
                        );
                        idx.upsert(e);
                        model_upsert_mixed(&mut model, &m);
                    }
                }
            }
            // 删除：remove_by_path（含 30% 不存在的路径）
            3 => {
                let path = if !model.is_empty() && rng.below(10) < 7 {
                    model[rng.below(model.len())].path.clone()
                } else {
                    gen_path(&mut rng)
                };
                idx.remove_by_path(&path);
                model.retain(|m| m.path != path);
            }
            // 批量删除：每条一次 swap_remove（最容易踩"末位条目搬移"的不变式）
            4 => {
                let mut paths = Vec::new();
                for _ in 0..1 + rng.below(4) {
                    if model.is_empty() {
                        break;
                    }
                    paths.push(model[rng.below(model.len())].path.clone());
                }
                idx.remove_paths_batch(&paths);
                for p in &paths {
                    model.retain(|m| &m.path != p);
                }
            }
            // 批量 upsert：批内故意混入重复路径，逼走"替换"分支
            5 => {
                let mut batch = Vec::new();
                for _ in 0..1 + rng.below(5) {
                    let path = if !model.is_empty() && rng.below(10) < 4 {
                        model[rng.below(model.len())].path.clone()
                    } else {
                        gen_shared_path(&mut rng, &model)
                    };
                    let (e, m) = mixed_entry(
                        &path,
                        rng.below(3) == 0,
                        sizes[rng.below(sizes.len())],
                        mtimes[rng.below(mtimes.len())],
                    );
                    batch.push(e);
                    model_upsert_mixed(&mut model, &m);
                }
                if !batch.is_empty() && rng.below(2) == 0 {
                    let dup = batch[rng.below(batch.len())].clone();
                    batch.push(dup);
                }
                idx.upsert_batch(batch);
            }
            // 前缀子树删除 / 整体清空
            6 => {
                if rng.below(8) == 0 || model.is_empty() {
                    idx.set_loading();
                    model.clear();
                } else {
                    let base = model[rng.below(model.len())].path.clone();
                    let mut cuts = vec![2usize.min(base.len())];
                    for (i, c) in base.char_indices() {
                        if c == '/' {
                            cuts.push(i);
                        }
                    }
                    let cut = cuts[rng.below(cuts.len())];
                    let mut prefix = base[..cut].to_string();
                    if rng.below(3) == 0 {
                        prefix = prefix.to_uppercase();
                    }
                    if rng.below(3) == 0 {
                        prefix.push('/');
                    }
                    idx.remove_prefix_for_test(&prefix);
                    model.retain(|m| !is_same_or_child_mirror(&prefix, &m.path));
                }
            }
            // 整批重新载入（覆盖并行构建分支）
            _ => {
                let reload: Vec<IndexEntry> = model
                    .iter()
                    .map(|m| mixed_entry(&m.path, m.is_dir, m.size, m.mtime).0)
                    .collect();
                idx.set_loading();
                idx.upsert_batch(reload);
            }
        }

        // ── 查询集：固定覆盖面 + 从模型里切出的随机词（保证既有多命中也有 0 命中）
        let mut queries: Vec<String> = [
            "dir:projects",
            "dir:Windows",
            "dir:n",
            "dir:s/p",
            "dir:文档",
            "dir:ПАПКА",
            "dir:node_modules",
            "ext:md",
            "ext:txt",
            "*.pdf",
            ".rs",
            "size:>1MB",
            "size:<100",
            "mtime:>7d",
            "mtime:<1d",
            "*",
            "report*",
            "*report*",
            "NOT *.tmp",
            "!report",
            "NOT dir:windows",
            "report",
            "a",
            "MAIN.RS",
            "type:dir size:>0",
            "type:xyz",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        // 粘贴完整路径（最吃"整条路径包含"的一条）
        queries.extend(paste_targets(&mut rng, &model));
        // 横跨 '/' 的窗口：直接打"目录段末尾 + '/' + 文件名开头"这个拼接点
        for _ in 0..3 {
            if model.is_empty() {
                break;
            }
            let lower = model[rng.below(model.len())].path.to_lowercase();
            let slashes: Vec<usize> = lower.match_indices('/').map(|(i, _)| i).collect();
            if slashes.is_empty() {
                continue;
            }
            let i = slashes[rng.below(slashes.len())];
            let start = i.saturating_sub(rng.below(4));
            let end = (i + 1 + rng.below(5)).min(lower.len());
            if start >= i || end <= i + 1 || !lower.is_char_boundary(start) || !lower.is_char_boundary(end)
            {
                continue;
            }
            let needle: String = lower[start..end]
                .chars()
                .filter(|c| !c.is_whitespace() && *c != '"' && *c != '!')
                .collect();
            if !needle.is_empty() {
                queries.push(needle.clone());
                queries.push(format!("dir:{needle}"));
            }
        }
        // 纯文本随机子串（含 CJK / 大小写混排）
        for _ in 0..3 {
            if model.is_empty() {
                break;
            }
            let lower = model[rng.below(model.len())].path.to_lowercase();
            let start = rng.below(lower.len());
            let end = (start + 1 + rng.below(7)).min(lower.len());
            if !lower.is_char_boundary(start) || !lower.is_char_boundary(end) {
                continue;
            }
            let mut needle: String = lower[start..end]
                .chars()
                .filter(|c| !c.is_whitespace() && *c != '"' && *c != '!')
                .collect();
            while needle.ends_with('/') {
                needle.pop();
            }
            if !needle.is_empty() {
                queries.push(needle);
            }
        }

        for (qi, q) in queries.iter().enumerate() {
            assert_query_matches_model(&idx, &model, q, now0, &format!("round {round}"));
            // 小 limit 对拍（候选集与次序）：抽样即可，模型侧排序是 O(n log n)
            if qi % 5 == 0 {
                assert_topk_matches_model(&idx, &model, q, now0, 7, &format!("round {round} topk"));
            }
            checks += 1;
            if model_hits(&model, q, now0).is_empty() {
                empty_hits += 1;
            } else {
                nonempty_hits += 1;
            }
        }
    }

    // 防假对拍：必须真的跑到过量、必须同时出现"命中"与"不命中"
    assert!(checks >= 1500, "对拍次数过少: {checks}");
    assert!(
        nonempty_hits > 100 && empty_hits > 100,
        "对拍必须同时覆盖命中与不命中: {nonempty_hits}/{empty_hits}"
    );
}

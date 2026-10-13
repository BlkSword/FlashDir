// 开发者磁盘分析模块
//
// 识别常见的开发工具目录和缓存，按类别聚合空间占用。
// 帮助开发者快速定位 node_modules、Rust target、Docker 数据等空间大户。
//
// 设计原则：
// - 只分类不修改，纯只读分析
// - 路径匹配基于**完整路径段**（不依赖绝对路径前缀格式，/ 与 \ 都支持）
// - 每个文件/目录（以及每一份字节）只归入一个类别
//
// 匹配规则的两次修正（旧实现的问题）：
// · 大小写：旧实现用 path.contains(fragment)，"/Target/"、"/Node_Modules/" 匹配不到；
//   现在按 ASCII 大小写不敏感比较。
// · 段边界：旧实现是纯子串匹配，于是 "Docker.raw" 能命中 "Docker.raw.bak"，
//   而目录名兜底把任意名为 bin/obj 的目录都当成 .NET 构建产物；
//   现在要求整段相等（"bin/Debug" 表示相邻的 bin、Debug 两个完整段）。

use serde::Serialize;
use rayon::prelude::*;
use std::collections::HashSet;
use crate::scan::Item;

// ─── 开发类别定义 ──────────────────────────────────────────

/// 已知开发工具的路径模式
struct KnownPattern {
    /// 类别标识符
    category: &'static str,
    /// 显示名称
    label: &'static str,
    /// 图标 (emoji)
    icon: &'static str,
    /// 描述说明
    description: &'static str,
    /// 路径规则：以 `/` 分隔的**连续路径段**序列。
    ///
    /// - "node_modules"  → 路径中任意一段等于 node_modules
    /// - "bin/Debug"     → 路径中必须出现相邻的 bin、Debug 两个完整段
    ///
    /// 匹配为 ASCII 大小写不敏感、段边界对齐（不接受任意子串命中）。
    /// 优先级按定义顺序（先匹配的优先）。
    path_rules: &'static [&'static str],
}

/// 所有已知模式的注册表
static KNOWN_PATTERNS: &[KnownPattern] = &[
    KnownPattern {
        category: "node",
        label: "Node.js 依赖",
        icon: "📦",
        description: "node_modules 目录",
        path_rules: &["node_modules"],
    },
    KnownPattern {
        category: "rust",
        label: "Rust 构建产物",
        icon: "🦀",
        description: "target/ — Rust 编译输出",
        path_rules: &["target"],
    },
    KnownPattern {
        category: "rust_cache",
        label: "Rust 工具链缓存",
        icon: "⚙️",
        description: ".cargo/registry、.rustup — Cargo 注册表和工具链",
        path_rules: &[
            ".cargo/registry",
            "cargo/registry",
            ".cargo/git",
            "cargo/git",
            ".rustup",
        ],
    },
    KnownPattern {
        category: "python_venv",
        label: "Python 虚拟环境",
        icon: "🐍",
        description: ".venv、venv、virtualenv — Python 隔离环境",
        path_rules: &[".venv", "venv", "virtualenv"],
    },
    KnownPattern {
        category: "python_cache",
        label: "Python 缓存",
        icon: "🗂️",
        description: "__pycache__ 和 .pyc 文件",
        path_rules: &["__pycache__"],
    },
    KnownPattern {
        category: "java_gradle",
        label: "Gradle 缓存",
        icon: "🐘",
        description: ".gradle — Gradle 构建缓存和 wrapper",
        path_rules: &[".gradle", "build/classes", "build/generated"],
    },
    KnownPattern {
        category: "java_maven",
        label: "Maven 仓库",
        icon: "📚",
        description: ".m2/repository — Maven 本地仓库",
        path_rules: &[".m2/repository"],
    },
    KnownPattern {
        category: "git",
        label: "Git 仓库数据",
        icon: "🔀",
        description: ".git — 版本控制历史和对象",
        path_rules: &[".git", ".git/objects", ".git/logs", "git/objects", "git/logs"],
    },
    KnownPattern {
        category: "dotnet",
        label: ".NET 构建产物",
        icon: "🔷",
        // 收紧：不再把任意名为 bin/obj 的目录（如 C:/tools/bin、node_modules/.bin）
        // 当成 .NET 构建产物——只认构建输出语义的 bin/Debug、obj/Release 等。
        description: "bin/Debug、bin/Release、obj/Debug、obj/Release — .NET/MSBuild 编译输出",
        path_rules: &["bin/Debug", "bin/Release", "obj/Debug", "obj/Release"],
    },
    KnownPattern {
        category: "dotnet_cache",
        label: "NuGet 缓存",
        icon: "📥",
        description: ".nuget/packages — NuGet 包缓存",
        path_rules: &[".nuget/packages"],
    },
    KnownPattern {
        category: "go",
        label: "Go 模块缓存",
        icon: "🔵",
        description: "GOPATH/pkg/mod — Go modules 下载缓存",
        path_rules: &["go/pkg/mod"],
    },
    KnownPattern {
        category: "docker",
        label: "Docker 数据",
        icon: "🐳",
        description: "Docker Desktop 磁盘镜像和数据卷",
        // Docker.raw / DockerDesktop 旧实现是纯子串，会命中 Docker.raw.bak 之类的名字；
        // 现在要求整段相等（大小写不敏感，故无需再列 Docker/ 与 docker/ 两份）。
        path_rules: &[
            "Docker",
            "docker/overlay2",
            "docker/containers",
            "DockerDesktop",
            "Docker.raw",
        ],
    },
    KnownPattern {
        category: "wsl",
        label: "WSL 虚拟磁盘",
        icon: "🐧",
        description: "WSL ext4.vhdx — Linux 子系统磁盘镜像",
        path_rules: &["ext4.vhdx", "WSL"],
    },
    KnownPattern {
        category: "android",
        label: "Android 构建",
        icon: "📱",
        description: "Android SDK、Gradle 构建缓存",
        path_rules: &[".android/avd", "Android/Sdk"],
    },
    KnownPattern {
        category: "npm_cache",
        label: "npm 全局缓存",
        icon: "🗃️",
        description: "npm-cache/_cacache — npm 下载缓存",
        path_rules: &["npm-cache/_cacache", ".npm/_cacache"],
    },
    KnownPattern {
        category: "pip_cache",
        label: "pip 缓存",
        icon: "🐍",
        description: "pip/cache — Python pip 下载缓存",
        path_rules: &["pip/cache"],
    },
    KnownPattern {
        category: "electron",
        label: "Electron 缓存",
        icon: "⚡",
        description: "electron — Electron 二进制下载缓存",
        path_rules: &["electron"],
    },
    KnownPattern {
        category: "vscode",
        label: "VS Code 数据",
        icon: "💻",
        description: "Code/User/workspaceStorage — VS Code 工作区存储",
        path_rules: &["Code/User/workspaceStorage", "Code/CachedData"],
    },
];

// ─── 输出结构 ────────────────────────────────────────────

/// 单个开发类别的聚合统计
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DevCategoryStats {
    /// 类别标识符
    pub category: String,
    /// 显示名称
    pub label: String,
    /// 图标 emoji
    pub icon: String,
    /// 描述
    pub description: String,
    /// 匹配到的项目数
    pub item_count: usize,
    /// 匹配到的文件数（不含目录）
    pub file_count: usize,
    /// 匹配到的目录数
    pub dir_count: usize,
    /// 总占用字节数（文件 + 目录聚合后的大小）
    pub total_size: i64,
    /// 格式化后的总大小
    pub total_size_formatted: String,
    /// 占总大小的百分比（相对所有开发者类别的合计）
    pub percent_of_dev: f64,
    /// 占全部扫描结果的百分比
    pub percent_of_total: f64,
    /// 该类别中最大的 5 个项目
    pub top_items: Vec<DevTopItem>,
}

/// 类别内 Top 项目
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DevTopItem {
    pub name: String,
    pub size: i64,
    pub size_formatted: String,
}

/// 完整开发者分析结果
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DevAnalysisResult {
    /// 总扫描项数
    pub total_items: usize,
    /// 被归类到开发者类别的项数
    pub dev_items: usize,
    /// 被归类到开发者类别的总字节数
    pub dev_total_size: i64,
    /// 开发者类别占总空间百分比
    pub dev_percent: f64,
    /// 按总大小降序排列的类别统计
    pub categories: Vec<DevCategoryStats>,
}

// ─── 分析引擎 ────────────────────────────────────────────

/// 对扫描结果进行开发者磁盘分析
///
/// 去重原则（匹配边界顶层 + 跨类别互斥）:
/// 后端已把每个目录的 size 聚合为其所有后代文件大小之和。因此若把"目录"和"它的
/// 子项"同时累加，同一字节会被算多遍——例如 node_modules 目录聚合值 + 其下每个文件。
/// 正确做法：一个 item 计入类别 c，当且仅当它匹配 c（取优先级最高的类别）**且其任何
/// 祖先目录都没有匹配任何类别**。这样每个"匹配区域"只在其最顶层被计一次。
///
/// 旧实现只比较"直接父目录是否属于**同一**类别"，于是跨类别的嵌套会重复累加：
/// 例如 p/.git（git 类，聚合 size 已含全部后代）与 p/.git/node_modules（node 类，
/// 目录名兜底命中）会同时计入两个类别 —— dev_total_size 虚高、各类别 size 之和与
/// dev_total_size 不一致，percent_of_total 理论上可 > 100%。
///
/// 现在只要**任意**祖先匹配了**任意**类别就跳过本项；由于被计入的项之间不可能存在
/// 祖先关系，各区域互不重叠，各类别 size 之和恒等于 dev_total_size，每个字节只算一次。
pub fn analyze(items: &[Item], total_size: i64, total_items: usize) -> DevAnalysisResult {
    // 第一遍（Rayon 并行）：为每个 item 计算它匹配的类别索引（取优先级最高的第一个匹配）
    let matches: Vec<Option<usize>> = items
        .par_iter()
        .map(|item| KNOWN_PATTERNS.iter().position(|p| matches_pattern(item, p)))
        .collect();

    // 所有"匹配到任意类别"的目录路径集合（跨类别统一去重用）
    let matched_dirs: HashSet<&str> = items
        .iter()
        .zip(matches.iter())
        .filter_map(|(item, m)| {
            if item.is_dir && m.is_some() {
                Some(item.path.as_str())
            } else {
                None
            }
        })
        .collect();

    // 为每个已知模式初始化累加器
    let mut accumulators: Vec<CategoryAccumulator> = KNOWN_PATTERNS
        .iter()
        .map(CategoryAccumulator::new)
        .collect();

    // 第二遍：只累加"匹配边界顶层"项（任何祖先都不匹配任何类别）
    for (i, item) in items.iter().enumerate() {
        let Some(idx) = matches[i] else { continue };
        if has_matched_ancestor(item.path.as_str(), &matched_dirs) {
            continue;
        }
        accumulators[idx].add(item);
    }

    // 计算开发者类别总大小
    let dev_total_size: i64 = accumulators.iter().map(|a| a.total_size).sum();
    let dev_items: usize = accumulators.iter().map(|a| a.item_count).sum();

    // 转换为输出格式
    let mut categories: Vec<DevCategoryStats> = accumulators
        .into_iter()
        .filter(|a| a.item_count > 0) // 只保留有匹配的类别
        .map(|a| a.into_stats(dev_total_size, total_size))
        .collect();

    // 按总大小降序排序
    categories.sort_unstable_by(|a, b| b.total_size.cmp(&a.total_size));

    // 重新计算各分类占 dev 总体的百分比（排序后分配 percent_of_dev）
    for cat in &mut categories {
        if dev_total_size > 0 {
            cat.percent_of_dev = (cat.total_size as f64 / dev_total_size as f64) * 100.0;
        }
    }

    let dev_percent = if total_size > 0 {
        (dev_total_size as f64 / total_size as f64) * 100.0
    } else {
        0.0
    };

    DevAnalysisResult {
        total_items,
        dev_items,
        dev_total_size,
        dev_percent,
        categories,
    }
}

/// 判断一个 item 是否匹配某个已知模式（任意一条规则命中即可）
fn matches_pattern(item: &Item, pattern: &KnownPattern) -> bool {
    let path = item.path.as_str();
    pattern
        .path_rules
        .iter()
        .any(|rule| contains_path_rule(path, rule))
}

/// 路径分隔符（同时支持 `/` 与 `\`，扫描结果在不同来源下可能是任意一种）
#[inline]
fn is_sep(b: u8) -> bool {
    b == b'/' || b == b'\\'
}

/// 判断路径中是否出现规则描述的**连续完整路径段**序列。
///
/// - `rule` 形如 "node_modules" 或 "bin/Debug"（以 `/` 分隔各段）。
/// - 段边界必须对齐：`"C:/xbin/Debug"` 不匹配 "bin/Debug"，`"C:/a/Docker.raw.bak"`
///   也不匹配 "Docker.raw"（旧实现的子串匹配会误判）。
/// - 比较是 ASCII 大小写不敏感的，因此 "/Target/"、"/Node_Modules/" 也能命中，
///   且无需为大小写各写一份规则。
/// - 全程按字节扫描，不分配堆内存（避免对每个 item 反复 to_lowercase 整条路径）。
fn contains_path_rule(path: &str, rule: &str) -> bool {
    let path = path.as_bytes();
    let rule = rule.as_bytes();
    if rule.is_empty() {
        return false;
    }

    // 逐个"段起点"尝试比对
    let mut seg_start = 0usize;
    loop {
        let mut pi = seg_start;
        let mut ri = 0usize;
        let mut matched = true;

        while ri < rule.len() {
            // 跳过路径中的分隔符，取出一个完整的路径段
            while pi < path.len() && is_sep(path[pi]) {
                pi += 1;
            }
            let p_start = pi;
            while pi < path.len() && !is_sep(path[pi]) {
                pi += 1;
            }
            let p_seg = &path[p_start..pi];

            // 取出规则中的一段
            let r_end = rule[ri..]
                .iter()
                .position(|&c| c == b'/')
                .map(|off| ri + off)
                .unwrap_or(rule.len());
            let r_seg = &rule[ri..r_end];

            if !p_seg.eq_ignore_ascii_case(r_seg) {
                matched = false;
                break;
            }
            ri = if r_end < rule.len() { r_end + 1 } else { rule.len() };
        }

        if matched {
            return true;
        }

        // 推进到下一个路径段的起点
        match path[seg_start..].iter().position(|&c| is_sep(c)) {
            Some(off) => seg_start += off + 1,
            None => return false,
        }
    }
}

/// 沿路径向上检查是否存在"匹配了任意类别的祖先目录"。
///
/// 只做字符串前缀回溯，不要求中间每一级目录都出现在 items 里（扫描可能被截断）。
fn has_matched_ancestor(path: &str, matched_dirs: &HashSet<&str>) -> bool {
    let mut rest = path;
    while let Some(pos) = rest.rfind(['/', '\\']) {
        rest = &rest[..pos];
        if rest.is_empty() {
            break;
        }
        if matched_dirs.contains(rest) {
            return true;
        }
    }
    false
}

// ─── 内部累加器 ──────────────────────────────────────────

struct CategoryAccumulator {
    category: &'static str,
    label: &'static str,
    icon: &'static str,
    description: &'static str,
    total_size: i64,
    file_count: usize,
    dir_count: usize,
    item_count: usize,
    top_items: Vec<DevTopItem>,
}

impl CategoryAccumulator {
    fn new(pattern: &KnownPattern) -> Self {
        Self {
            category: pattern.category,
            label: pattern.label,
            icon: pattern.icon,
            description: pattern.description,
            total_size: 0,
            file_count: 0,
            dir_count: 0,
            item_count: 0,
            top_items: Vec::new(),
        }
    }

    fn add(&mut self, item: &Item) {
        self.item_count += 1;
        if item.is_dir {
            self.dir_count += 1;
        } else {
            self.file_count += 1;
        }
        self.total_size += item.size;

        // 维护 Top 5 列表（按 size 降序）
        let pos = self
            .top_items
            .binary_search_by(|i: &DevTopItem| item.size.cmp(&i.size))
            .unwrap_or_else(|e| e);
        self.top_items.insert(
            pos,
            DevTopItem {
                name: item.name.to_string(),
                size: item.size,
                size_formatted: crate::scan::format_size(item.size).to_string(),
            },
        );
        if self.top_items.len() > 5 {
            self.top_items.remove(self.top_items.len() - 1); // 移除最小的（最后）
        }
    }

    fn into_stats(self, dev_total_size: i64, total_size: i64) -> DevCategoryStats {
        let percent_of_dev = if dev_total_size > 0 {
            (self.total_size as f64 / dev_total_size as f64) * 100.0
        } else {
            0.0
        };

        let percent_of_total = if total_size > 0 {
            (self.total_size as f64 / total_size as f64) * 100.0
        } else {
            0.0
        };

        DevCategoryStats {
            category: self.category.to_string(),
            label: self.label.to_string(),
            icon: self.icon.to_string(),
            description: self.description.to_string(),
            item_count: self.item_count,
            file_count: self.file_count,
            dir_count: self.dir_count,
            total_size: self.total_size,
            total_size_formatted: crate::scan::format_size(self.total_size).to_string(),
            percent_of_dev,
            percent_of_total,
            top_items: self.top_items,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scan::CompactString;

    fn item(path: &str, name: &str, size: i64, is_dir: bool) -> Item {
        Item {
            path: CompactString::from(path),
            name: CompactString::from(name),
            size,
            size_formatted: CompactString::new(),
            is_dir,
            mtime: 0,
            atime: 0,
        }
    }

    #[test]
    fn dedup_dir_aggregation_no_double_count() {
        // node_modules 目录聚合 size=100；其下 react 目录聚合=100；react/index.js=100。
        // 旧实现三者都累加得 300（虚高 3 倍），正确值应为 100（只计顶层 node_modules）。
        let items = vec![
            item("p/node_modules", "node_modules", 100, true),
            item("p/node_modules/react", "react", 100, true),
            item("p/node_modules/react/index.js", "index.js", 100, false),
        ];
        let result = analyze(&items, 100, 3);
        let node = result
            .categories
            .iter()
            .find(|c| c.category == "node")
            .unwrap();
        assert_eq!(node.total_size, 100, "node_modules 不应重复累加子项");
        assert_eq!(node.item_count, 1, "只计顶层 node_modules");
    }

    #[test]
    fn distinct_top_dirs_counted_separately() {
        // 两个独立的 node_modules 应分别计入，总和=各自聚合
        let items = vec![
            item("a/node_modules", "node_modules", 200, true),
            item("b/node_modules", "node_modules", 300, true),
        ];
        let result = analyze(&items, 500, 2);
        let node = result
            .categories
            .iter()
            .find(|c| c.category == "node")
            .unwrap();
        assert_eq!(node.total_size, 500);
        assert_eq!(node.item_count, 2);
    }

    #[test]
    fn nested_node_modules_deduped() {
        // proj/node_modules/proj2/node_modules：内层应被外层 node_modules 包含 → 跳过。
        // 注意中间目录 proj2 也匹配（路径含 /node_modules/），需在 items 中才能正确去重。
        let items = vec![
            item("proj/node_modules", "node_modules", 400, true),
            item("proj/node_modules/proj2", "proj2", 250, true),
            item("proj/node_modules/proj2/node_modules", "node_modules", 150, true),
        ];
        let result = analyze(&items, 400, 3);
        let node = result
            .categories
            .iter()
            .find(|c| c.category == "node")
            .unwrap();
        assert_eq!(node.total_size, 400, "内层 node_modules 应被外层包含，不重复计入");
        assert_eq!(node.item_count, 1);
    }

    /// 汇总口径自洽：各类别 size 之和 == dev_total_size，且没有类别超过 100%。
    fn assert_self_consistent(result: &DevAnalysisResult) {
        let sum: i64 = result.categories.iter().map(|c| c.total_size).sum();
        assert_eq!(sum, result.dev_total_size, "各类别 size 之和必须等于 dev_total_size");
        let items: usize = result.categories.iter().map(|c| c.item_count).sum();
        assert_eq!(items, result.dev_items, "各类别 item_count 之和必须等于 dev_items");
        for c in &result.categories {
            assert!(
                c.percent_of_total <= 100.0 + 1e-9,
                "{} percent_of_total = {} 不应超过 100",
                c.category,
                c.percent_of_total
            );
            assert!(
                c.percent_of_dev <= 100.0 + 1e-9,
                "{} percent_of_dev = {} 不应超过 100",
                c.category,
                c.percent_of_dev
            );
        }
    }

    /// 回归：跨类别嵌套曾重复计入 dev_total_size。
    ///
    /// p/.git（git 类，聚合 1000 已含全部后代）与 p/.git/node_modules（node 类，400）
    /// 在旧实现里同时累加 → dev_total = 1400 > 真实占用 1000，percent_of_total = 140%。
    #[test]
    fn cross_category_nesting_is_counted_once() {
        let items = vec![
            item("p/.git", ".git", 1000, true),
            item("p/.git/node_modules", "node_modules", 400, true),
            item("p/.git/node_modules/react/index.js", "index.js", 400, false),
        ];
        let result = analyze(&items, 1000, 3);

        assert_eq!(result.dev_total_size, 1000, "跨类别嵌套不得重复计入");
        assert_self_consistent(&result);

        let git = result.categories.iter().find(|c| c.category == "git").unwrap();
        assert_eq!(git.total_size, 1000);
        assert_eq!(git.item_count, 1, "只计顶层 .git");
        assert!(
            result.categories.iter().all(|c| c.category != "node"),
            "被祖先 .git 包含的 node_modules 不应另计一类"
        );
        assert!(git.percent_of_total <= 100.0);
    }

    /// 反向的跨类别嵌套：node 类祖先包住 git 类的后代，同样只算一次。
    #[test]
    fn cross_category_nesting_inner_category_loses() {
        let items = vec![
            item("p/node_modules", "node_modules", 500, true),
            item("p/node_modules/.git", ".git", 300, true),
            item("p/node_modules/.git/objects", "objects", 300, true),
        ];
        let result = analyze(&items, 500, 3);
        assert_eq!(result.dev_total_size, 500);
        assert_self_consistent(&result);
        let node = result.categories.iter().find(|c| c.category == "node").unwrap();
        assert_eq!(node.total_size, 500);
        assert!(result.categories.iter().all(|c| c.category != "git"));
    }

    /// 回归：旧实现 path.contains(fragment) 区分大小写，/Target/、/Node_Modules/ 匹配不到。
    #[test]
    fn rule_matching_is_case_insensitive() {
        let items = vec![
            item("C:/proj/Target/debug/app.exe", "app.exe", 700, false),
            item("C:/proj/Node_Modules/react/index.js", "index.js", 300, false),
            item("C:/proj/.GIT/OBJECTS/ab/cdef", "cdef", 50, false),
            item("C:/proj/Bin/Debug/app.dll", "app.dll", 20, false),
        ];
        let result = analyze(&items, 1070, 4);
        let size_of = |cat: &str| {
            result
                .categories
                .iter()
                .find(|c| c.category == cat)
                .map(|c| c.total_size)
        };
        assert_eq!(size_of("rust"), Some(700), "/Target/ 应大小写不敏感命中 rust");
        assert_eq!(size_of("node"), Some(300), "/Node_Modules/ 应大小写不敏感命中 node");
        assert_eq!(size_of("git"), Some(50), "/.GIT/OBJECTS/ 应大小写不敏感命中 git");
        assert_eq!(size_of("dotnet"), Some(20), "/Bin/Debug/ 应大小写不敏感命中 dotnet");
        assert_self_consistent(&result);
    }

    /// 回归：旧实现纯子串匹配，任意名为 bin/obj 的目录都被算作 .NET 构建产物。
    #[test]
    fn bare_bin_obj_directories_are_not_dotnet() {
        let items = vec![
            item("C:/tools/bin/tool.exe", "tool.exe", 100, false),
            item("C:/proj/node_modules/.bin/tsc", "tsc", 10, false),
            item("C:/proj/build/obj/tmp.bin", "tmp.bin", 5, false),
        ];
        let result = analyze(&items, 115, 3);
        assert!(
            result.categories.iter().all(|c| c.category != "dotnet"),
            "裸 bin/obj 目录不应再算作 .NET 构建产物"
        );
        // node_modules/.bin 仍属于 node 区域
        let node = result.categories.iter().find(|c| c.category == "node").unwrap();
        assert_eq!(node.total_size, 10);
        assert_eq!(result.dev_total_size, 10, "C:/tools/bin 不再被归类");
        assert_self_consistent(&result);

        // 构建输出语义的 bin/Debug 仍然必须命中
        let items2 = vec![item("C:/proj/bin/Debug/app.dll", "app.dll", 20, false)];
        let r2 = analyze(&items2, 20, 1);
        let dotnet = r2.categories.iter().find(|c| c.category == "dotnet").unwrap();
        assert_eq!(dotnet.total_size, 20);
        assert_self_consistent(&r2);
    }

    /// 回归：Docker.raw / DockerDesktop 旧实现是纯子串，能命中任意名字片段。
    #[test]
    fn docker_names_require_exact_segments() {
        let items = vec![
            item("C:/backup/Docker.raw.bak", "Docker.raw.bak", 900, false),
            item("C:/backup/DockerDesktopBackup/x.dat", "x.dat", 800, false),
            item("C:/data/xDocker/y.bin", "y.bin", 700, false),
            item("C:/data/xnode_modules/y.js", "y.js", 600, false),
        ];
        let result = analyze(&items, 3000, 4);
        assert_eq!(result.dev_total_size, 0, "近似名字不应再命中开发者类别");
        assert!(result.categories.is_empty());

        // 正例：完整段仍然命中
        let items2 = vec![
            item("C:/backup/Docker.raw", "Docker.raw", 900, false),
            item(
                "C:/Users/x/AppData/Local/DockerDesktop/wsl/data.bin",
                "data.bin",
                800,
                false,
            ),
        ];
        let r2 = analyze(&items2, 1700, 2);
        let docker = r2.categories.iter().find(|c| c.category == "docker").unwrap();
        assert_eq!(docker.total_size, 1700);
        assert_self_consistent(&r2);

        // 文件名段也必须整段相等
        let r3 = analyze(
            &[item("C:/vm/ext4.vhdx.old", "ext4.vhdx.old", 10, false)],
            10,
            1,
        );
        assert_eq!(r3.dev_total_size, 0, "ext4.vhdx 必须整段匹配");
        let r4 = analyze(&[item("C:/vm/ext4.vhdx", "ext4.vhdx", 10, false)], 10, 1);
        assert_eq!(
            r4.categories.iter().find(|c| c.category == "wsl").unwrap().total_size,
            10
        );
    }

    /// 反斜杠路径（MFT 来源可能保留 \）同样能匹配，且祖先去重正常。
    #[test]
    fn backslash_paths_match_and_dedup() {
        let items = vec![
            item("C:\\proj\\node_modules", "node_modules", 300, true),
            item("C:\\proj\\node_modules\\react\\index.js", "index.js", 300, false),
        ];
        let result = analyze(&items, 300, 2);
        let node = result.categories.iter().find(|c| c.category == "node").unwrap();
        assert_eq!(node.total_size, 300, "反斜杠路径也要正确去重");
        assert_eq!(node.item_count, 1);
        assert_self_consistent(&result);
    }
}

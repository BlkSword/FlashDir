// 开发者磁盘分析模块
//
// 识别常见的开发工具目录和缓存，按类别聚合空间占用。
// 帮助开发者快速定位 node_modules、Rust target、Docker 数据等空间大户。
//
// 设计原则：
// - 只分类不修改，纯只读分析
// - 路径匹配基于最后一级或倒数第二级目录名（不依赖绝对路径格式）
// - 每个文件/目录只归入一个最匹配的类别

use serde::Serialize;
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
    /// 路径匹配规则：路径中是否包含此字符串片段
    /// 匹配逻辑为 path.contains(fragment)
    /// 优先级按定义顺序（先匹配的优先）
    path_fragments: &'static [&'static str],
}

/// 所有已知模式的注册表
static KNOWN_PATTERNS: &[KnownPattern] = &[
    KnownPattern {
        category: "node",
        label: "Node.js 依赖",
        icon: "📦",
        description: "node_modules 目录",
        path_fragments: &["/node_modules/", "\\node_modules\\"],
    },
    KnownPattern {
        category: "rust",
        label: "Rust 构建产物",
        icon: "🦀",
        description: "target/ — Rust 编译输出",
        path_fragments: &["/target/", "\\target\\"],
    },
    KnownPattern {
        category: "rust_cache",
        label: "Rust 工具链缓存",
        icon: "⚙️",
        description: ".cargo/registry、.rustup — Cargo 注册表和工具链",
        path_fragments: &[
            "/.cargo/registry/",
            "\\cargo\\registry\\",
            "/.cargo/git/",
            "\\cargo\\git\\",
            "/.rustup/",
            "\\.rustup\\",
        ],
    },
    KnownPattern {
        category: "python_venv",
        label: "Python 虚拟环境",
        icon: "🐍",
        description: ".venv、venv、virtualenv — Python 隔离环境",
        path_fragments: &[
            "/.venv/",
            "\\.venv\\",
            "/venv/",
            "\\venv\\",
            "/virtualenv/",
            "\\virtualenv\\",
        ],
    },
    KnownPattern {
        category: "python_cache",
        label: "Python 缓存",
        icon: "🗂️",
        description: "__pycache__ 和 .pyc 文件",
        path_fragments: &[
            "/__pycache__/",
            "\\__pycache__\\",
        ],
    },
    KnownPattern {
        category: "java_gradle",
        label: "Gradle 缓存",
        icon: "🐘",
        description: ".gradle — Gradle 构建缓存和 wrapper",
        path_fragments: &[
            "/.gradle/",
            "\\.gradle\\",
            "/build/classes/",
            "\\build\\classes\\",
            "/build/generated/",
            "\\build\\generated\\",
        ],
    },
    KnownPattern {
        category: "java_maven",
        label: "Maven 仓库",
        icon: "📚",
        description: ".m2/repository — Maven 本地仓库",
        path_fragments: &[
            "/.m2/repository/",
            "\\.m2\\repository\\",
        ],
    },
    KnownPattern {
        category: "git",
        label: "Git 仓库数据",
        icon: "🔀",
        description: ".git — 版本控制历史和对象",
        path_fragments: &[
            "/.git/objects/",
            "\\git\\objects\\",
            "/.git/logs/",
            "\\git\\logs\\",
        ],
    },
    KnownPattern {
        category: "dotnet",
        label: ".NET 构建产物",
        icon: "🔷",
        description: "bin/、obj/ — .NET/MSBuild 编译输出",
        path_fragments: &[
            "/bin/Debug/",
            "\\bin\\Debug\\",
            "/bin/Release/",
            "\\bin\\Release\\",
            "/obj/Debug/",
            "\\obj\\Debug\\",
            "/obj/Release/",
            "\\obj\\Release\\",
        ],
    },
    KnownPattern {
        category: "dotnet_cache",
        label: "NuGet 缓存",
        icon: "📥",
        description: ".nuget/packages — NuGet 包缓存",
        path_fragments: &[
            "/.nuget/packages/",
            "\\.nuget\\packages\\",
        ],
    },
    KnownPattern {
        category: "go",
        label: "Go 模块缓存",
        icon: "🔵",
        description: "GOPATH/pkg/mod — Go modules 下载缓存",
        path_fragments: &[
            "/go/pkg/mod/",
            "\\go\\pkg\\mod\\",
        ],
    },
    KnownPattern {
        category: "docker",
        label: "Docker 数据",
        icon: "🐳",
        description: "Docker Desktop 磁盘镜像和数据卷",
        path_fragments: &[
            "/Docker/",
            "\\Docker\\",
            "/docker/overlay2/",
            "\\docker\\overlay2\\",
            "/docker/containers/",
            "\\docker\\containers\\",
            "DockerDesktop",
            "Docker.raw",
        ],
    },
    KnownPattern {
        category: "wsl",
        label: "WSL 虚拟磁盘",
        icon: "🐧",
        description: "WSL ext4.vhdx — Linux 子系统磁盘镜像",
        path_fragments: &[
            "ext4.vhdx",
            "/WSL/",
            "\\WSL\\",
        ],
    },
    KnownPattern {
        category: "android",
        label: "Android 构建",
        icon: "📱",
        description: "Android SDK、Gradle 构建缓存",
        path_fragments: &[
            "/.android/avd/",
            "\\.android\\avd\\",
            "/Android/Sdk/",
            "\\Android\\Sdk\\",
        ],
    },
    KnownPattern {
        category: "npm_cache",
        label: "npm 全局缓存",
        icon: "🗃️",
        description: "npm-cache/_cacache — npm 下载缓存",
        path_fragments: &[
            "/npm-cache/_cacache/",
            "\\npm-cache\\_cacache\\",
            "/.npm/_cacache/",
            "\\.npm\\_cacache\\",
        ],
    },
    KnownPattern {
        category: "pip_cache",
        label: "pip 缓存",
        icon: "🐍",
        description: "pip/cache — Python pip 下载缓存",
        path_fragments: &[
            "/pip/cache/",
            "\\pip\\cache\\",
        ],
    },
    KnownPattern {
        category: "electron",
        label: "Electron 缓存",
        icon: "⚡",
        description: "electron — Electron 二进制下载缓存",
        path_fragments: &[
            "/electron/",
            "\\electron\\",
            "/Electron/",
            "\\Electron\\",
        ],
    },
    KnownPattern {
        category: "vscode",
        label: "VS Code 数据",
        icon: "💻",
        description: "Code/User/workspaceStorage — VS Code 工作区存储",
        path_fragments: &[
            "/Code/User/workspaceStorage/",
            "\\Code\\User\\workspaceStorage\\",
            "/Code/CachedData/",
            "\\Code\\CachedData\\",
        ],
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
pub fn analyze(items: &[Item], total_size: i64, total_items: usize) -> DevAnalysisResult {
    // 为每个已知模式初始化累加器
    let mut accumulators: Vec<CategoryAccumulator> = KNOWN_PATTERNS
        .iter()
        .map(|p| CategoryAccumulator::new(p))
        .collect();

    // 分类：每个 item 归入第一个匹配的类别
    for item in items {
        for (idx, pattern) in KNOWN_PATTERNS.iter().enumerate() {
            if matches_pattern(item, pattern) {
                accumulators[idx].add(item);
                break; // 一个 item 只归入一个类别
            }
        }
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

/// 检查一个 item 是否匹配某个已知模式
fn matches_pattern(item: &Item, pattern: &KnownPattern) -> bool {
    let path = item.path.as_str();
    // 规范化路径：统一使用 / 进行匹配
    for fragment in pattern.path_fragments {
        // 同时支持 / 和 \ 的路径片段
        if path.contains(fragment) {
            return true;
        }
    }
    // 对于目录类型的 item，也检查其名称是否匹配
    // 例如名为 "node_modules" 的目录，无论路径格式如何都应匹配
    if item.is_dir {
        let name = item.name.as_str();
        // 检查常见开发目录名
        match name {
            "node_modules" => {
                pattern.category == "node"
            }
            "target" => {
                // target/ 可能是 Rust 的也可能是 Maven 的
                // 这里简单地归为 Rust（更常见的大目录）
                pattern.category == "rust"
            }
            "__pycache__" => {
                pattern.category == "python_cache"
            }
            ".git" => {
                pattern.category == "git"
            }
            ".venv" | "venv" | "virtualenv" => {
                pattern.category == "python_venv"
            }
            ".gradle" => {
                pattern.category == "java_gradle"
            }
            "bin" | "obj" => {
                pattern.category == "dotnet"
            }
            _ => false,
        }
    } else {
        false
    }
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

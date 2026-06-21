// FlashDir CLI — 终端磁盘空间分析工具
//
// 用法:
//   flashdir-cli <PATH> [OPTIONS]
//
// 选项:
//   --top <N>       显示前 N 条结果 (默认 20)
//   --sort <COL>    排序: size | name (默认 size)
//   --json          以 JSON 格式输出
//   --no-cache      跳过缓存，强制重新扫描
//   --no-mft        禁用 MFT 直接读取（回退到目录遍历）
//   --help          显示帮助
//
// 示例:
//   flashdir-cli C:\Users\Downloads
//   flashdir-cli C:\Windows --top 10 --sort name
//   flashdir-cli /home/user --json --no-cache

use std::io::{self, Write};
use std::time::Instant;

use flashdir::perf::PerformanceMonitor;
use flashdir::scan;

// ─── 命令行参数解析 ────────────────────────────────────────

struct Args {
    path: String,
    top: usize,
    sort: SortBy,
    json: bool,
    no_cache: bool,
    no_mft: bool,
}

#[derive(Clone, Copy)]
enum SortBy {
    Size,
    Name,
}

fn parse_args() -> Result<Args, String> {
    let raw: Vec<String> = std::env::args().collect();

    if raw.iter().any(|a| a == "--help" || a == "-h") {
        print_help();
        std::process::exit(0);
    }

    let mut path: Option<String> = None;
    let mut top: usize = 20;
    let mut sort = SortBy::Size;
    let mut json = false;
    let mut no_cache = false;
    let mut no_mft = false;

    let mut i = 1;
    while i < raw.len() {
        match raw[i].as_str() {
            "--top" => {
                i += 1;
                top = raw
                    .get(i)
                    .ok_or("--top 需要一个数字参数")?
                    .parse()
                    .map_err(|_| "--top 参数必须是数字")?;
                if top == 0 {
                    top = usize::MAX; // 0 = show all
                }
            }
            "--sort" => {
                i += 1;
                sort = match raw.get(i).map(|s| s.as_str()) {
                    Some("size") => SortBy::Size,
                    Some("name") => SortBy::Name,
                    _ => return Err("--sort 参数必须是 size 或 name".into()),
                };
            }
            "--json" => json = true,
            "--no-cache" => no_cache = true,
            "--no-mft" => no_mft = true,
            arg if !arg.starts_with('-') && path.is_none() => {
                path = Some(arg.to_string());
            }
            arg if arg.starts_with('-') => {
                return Err(format!("未知参数: {}", arg));
            }
            _ => {
                // 忽略多余的路径参数
            }
        }
        i += 1;
    }

    let path = path.ok_or("请指定要扫描的目录路径")?;

    Ok(Args {
        path,
        top,
        sort,
        json,
        no_cache,
        no_mft,
    })
}

fn print_help() {
    eprintln!(
        r#"FlashDir CLI v{} — 终端磁盘空间分析工具

用法: flashdir-cli <PATH> [OPTIONS]

选项:
  --top <N>       显示前 N 条结果 (默认 20, 0=全部)
  --sort <COL>    排序方式: size (默认) | name
  --json          以 JSON 格式输出
  --no-cache      跳过缓存，强制重新扫描
  --no-mft        禁用 MFT 直接读取
  --help, -h      显示此帮助

示例:
  flashdir-cli C:\Users\Downloads
  flashdir-cli C:\ --top 10
  flashdir-cli /home/user/Documents --sort name --json
"#,
        env!("CARGO_PKG_VERSION")
    );
}

// ─── 格式化输出 ────────────────────────────────────────────

fn format_size(bytes: i64) -> String {
    if bytes < 1024 {
        return format!("{} B", bytes);
    }
    let mut size = bytes as f64;
    let units = ["B", "KB", "MB", "GB", "TB"];
    let mut idx = 0;
    while size >= 1024.0 && idx < units.len() - 1 {
        size /= 1024.0;
        idx += 1;
    }
    if size < 10.0 {
        format!("{:.2} {}", size, units[idx])
    } else if size < 100.0 {
        format!("{:.1} {}", size, units[idx])
    } else {
        format!("{:.0} {}", size, units[idx])
    }
}

fn print_table(items: &[scan::Item], total_size: i64, scan_time: f64, file_count: usize) {
    let stdout = io::stdout();
    let mut out = stdout.lock();

    // 表头
    writeln!(
        out,
        "{:<8} {:<10} {:<50}",
        "SIZE", "TYPE", "NAME"
    )
    .ok();
    writeln!(out, "{}", "-".repeat(68)).ok();

    for item in items {
        let type_str = if item.is_dir { "<DIR>" } else { "" };
        let name = if item.name.len() > 48 {
            format!("{}...", &item.name[..45])
        } else {
            item.name.to_string()
        };

        writeln!(
            out,
            "{:>8} {:<10} {:<50}",
            item.size_formatted.as_str(),
            type_str,
            name
        )
        .ok();
    }

    writeln!(out, "{}", "-".repeat(68)).ok();
    writeln!(
        out,
        "{} 个文件 | 总计: {} | 扫描耗时: {:.2}s",
        file_count,
        format_size(total_size),
        scan_time
    )
    .ok();
}

fn print_json(items: &[scan::Item], total_size: i64, scan_time: f64, file_count: usize) {
    #[derive(serde::Serialize)]
    struct Output {
        scan_time_sec: f64,
        total_size: i64,
        total_size_formatted: String,
        file_count: usize,
        items: Vec<ItemJson>,
    }

    #[derive(serde::Serialize)]
    struct ItemJson {
        name: String,
        path: String,
        size: i64,
        size_formatted: String,
        is_dir: bool,
    }

    let output = Output {
        scan_time_sec: scan_time,
        total_size,
        total_size_formatted: format_size(total_size),
        file_count,
        items: items
            .iter()
            .map(|i| ItemJson {
                name: i.name.to_string(),
                path: i.path.to_string(),
                size: i.size,
                size_formatted: i.size_formatted.to_string(),
                is_dir: i.is_dir,
            })
            .collect(),
    };

    println!("{}", serde_json::to_string_pretty(&output).unwrap_or_default());
}

// ─── 扫描 ──────────────────────────────────────────────────

#[tokio::main]
async fn main() {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("错误: {}", e);
            eprintln!("使用 --help 查看帮助");
            std::process::exit(1);
        }
    };

    // 进度提示
    if !args.json {
        eprint!("正在扫描 {} ... ", args.path);
        io::stderr().flush().ok();
    }

    let total_start = Instant::now();
    let perf_monitor = PerformanceMonitor::instance();

    // no_mft 强制禁用 MFT 快速路径，回退到目录遍历
    if args.no_mft {
        scan::set_disable_mft(true);
    }

    // 调用扫描引擎（不使用 app_handle = 无流式事件）
    let result = match scan::scan_directory(
        &args.path,
        args.no_cache || args.no_mft, // no_mft 同时会强制刷新缓存
        perf_monitor,
        None, // CLI 不需要流式事件
    )
    .await
    {
        Ok(r) => r,
        Err(e) => {
            if !args.json {
                eprintln!("\n扫描失败: {}", e);
            } else {
                eprintln!("{{\"error\": \"{}\"}}", e);
            }
            std::process::exit(1);
        }
    };

    let elapsed = total_start.elapsed().as_secs_f64();

    if !args.json {
        eprintln!("完成 ({:.2}s)", elapsed);
    }

    // 准备输出项：按指定列排序，取 top N
    let mut items = result.items.clone();
    match args.sort {
        SortBy::Size => items.sort_unstable_by(|a, b| b.size.cmp(&a.size)),
        SortBy::Name => items.sort_unstable_by(|a, b| {
            let a_is_dir = a.is_dir as i32;
            let b_is_dir = b.is_dir as i32;
            b_is_dir
                .cmp(&a_is_dir)
                .then_with(|| a.name.as_str().cmp(b.name.as_str()))
        }),
    }

    let total_items = items.len();
    if args.top > 0 && args.top < items.len() {
        items.truncate(args.top);
    }

    // 统计纯文件数量
    let file_count = items.iter().filter(|i| !i.is_dir).count();

    if args.json {
        print_json(&items, result.total_size, elapsed, file_count);
    } else {
        print_table(&items, result.total_size, elapsed, file_count);
        if args.top > 0 && total_items > args.top {
            println!(
                "... 还有 {} 个项目未显示（使用 --top 0 查看全部）",
                total_items - args.top
            );
        }
    }
}

//! FlashDir MCP 启动器
//!
//! 三种模式：
//! - `flashdir-mcp`（默认 / `--mcp`）：以 stdio 运行 MCP 服务器（Host 直接启动，独立进程）
//! - `flashdir-mcp --bridge`：桥接到运行中的桌面端端点（共享索引与扫描缓存，
//!   继承桌面端的管理员权限 → MFT 直读）；桌面端未运行时会自动拉起
//! - `flashdir-mcp --selftest[-endpoint|-bridge]`：协议 / 端点 / 桥接自测
//!
//! Host 配置（推荐桥接，能复用桌面端热态与管理员权限）：
//! ```json
//! { "mcpServers": { "flashdir": { "command": "C:\path\flashdir-mcp.exe", "args": ["--bridge"] } } }
//! ```

#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

fn print_help() {
    eprintln!(
        r#"FlashDir MCP 启动器

用法:
  flashdir-mcp                     以 stdio 运行 MCP 服务器（独立进程）
  flashdir-mcp --bridge            桥接到桌面端端点（推荐：共享索引/缓存与管理员权限）
  flashdir-mcp --selftest          协议与工具自测
  flashdir-mcp --selftest-endpoint 本机端点自测（含 token 校验）
  flashdir-mcp --selftest-bridge   桥接链路自测（需桌面端已运行）
  flashdir-mcp --help              显示帮助

MCP Host 配置示例:
  {{ "mcpServers": {{ "flashdir": {{ "command": "<path>\flashdir-mcp.exe", "args": ["--bridge"] }} }} }}

工具（只读）: list_volumes / search_files / scan_directory / list_directory / cache_stats / diagnostics
"#
    );
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    let has = |flag: &str| args.iter().any(|a| a == flag);

    if has("--help") || has("-h") {
        print_help();
        return;
    }
    if has("--selftest") {
        std::process::exit(flashdir::mcp::selftest().await);
    }
    if has("--selftest-endpoint") {
        std::process::exit(flashdir::mcp::selftest_endpoint().await);
    }
    if has("--selftest-bridge") {
        std::process::exit(flashdir::mcp::selftest_bridge().await);
    }
    if has("--bridge") {
        std::process::exit(flashdir::mcp::serve_bridge().await);
    }
    // 默认（含 --mcp）：stdio 独立模式
    flashdir::mcp::serve_stdio().await;
}

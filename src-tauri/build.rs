fn main() {
    // CLI 二进制文件不需要 Tauri 构建步骤
    let is_cli = std::env::var("CARGO_BIN_NAME")
        .map(|n| n != "flashdir")
        .unwrap_or(false);
    if !is_cli {
        tauri_build::build()
    }
}

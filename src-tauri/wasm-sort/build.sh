#!/bin/bash
set -e

echo "Building WASM sort module..."

# 确保 wasm-pack 已安装
if ! command -v wasm-pack &> /dev/null; then
    echo "wasm-pack not found, installing..."
    cargo install wasm-pack
fi

# 构建 WASM 包
echo "Building with wasm-pack..."
wasm-pack build --target web --out-dir pkg

echo "WASM build complete!"
echo "Output: src-tauri/wasm-sort/pkg/"

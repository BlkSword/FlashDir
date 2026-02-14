#!/usr/bin/env pwsh
$ErrorActionPreference = "Stop"

Write-Host "Building WASM sort module..." -ForegroundColor Green

# 确保 wasm-pack 已安装
$wasmPack = Get-Command wasm-pack -ErrorAction SilentlyContinue
if (-not $wasmPack) {
    Write-Host "wasm-pack not found, installing..." -ForegroundColor Yellow
    cargo install wasm-pack
}

# 构建 WASM 包
Write-Host "Building with wasm-pack..." -ForegroundColor Green
wasm-pack build --target web --out-dir pkg

Write-Host "WASM build complete!" -ForegroundColor Green
Write-Host "Output: src-tauri/wasm-sort/pkg/" -ForegroundColor Cyan

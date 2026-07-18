// 自定义紧凑二进制扫描结果解析器
//
// 对应后端 scan::encode_scan_result 的布局（小端）：
//   u32 magic=0x4644 | u8 version | u8 flags (v2: bit0 = mftAvailable)
//   i64 total_size | f64 scan_time | u32 item_count | u32 file_count | u32 dir_count
//   f64 io_ms | f64 compute_ms | f64 serialize_ms
//   u32 path_len | path_utf8
//   逐项: u32 path_len|path_utf8 | u32 name_len|name_utf8 | i64 size | u8 is_dir [| i64 mtime (v2)]
//
// 大结果（百万级条目）分块解码，每块之间让出主线程，避免渲染进程长时间无法重绘。

import { formatSize } from './format.js'

const MAGIC = 0x4644
const DECODE_CHUNK = 50000

// Tauri 2 把 ipc::Response 的字节交给 JS 时，不同小版本可能呈现为 ArrayBuffer 或 Uint8Array；
// 这里统一归一为 ArrayBuffer，避免 DataView 构造报错。
function toArrayBuffer(input) {
  if (input instanceof ArrayBuffer) return input
  if (ArrayBuffer.isView(input)) {
    const view = input
    return view.byteOffset === 0 && view.byteLength === view.buffer.byteLength
      ? view.buffer
      : view.buffer.slice(view.byteOffset, view.byteOffset + view.byteLength)
  }
  if (input && typeof input === 'object' && Array.isArray(input.data)) {
    return new Uint8Array(input.data).buffer
  }
  throw new Error('无法识别的二进制扫描响应类型: ' + Object.prototype.toString.call(input))
}

/**
 * 解码二进制扫描结果。
 * @param {ArrayBuffer|Uint8Array} buffer invoke('scan_directory_binary') 的返回值
 * @param {(decoded: number, total: number) => void} [onProgress] 大结果分块进度回调
 */
export async function decodeScanResult(buffer, onProgress) {
  const arrayBuffer = toArrayBuffer(buffer)
  const u8 = new Uint8Array(arrayBuffer)
  const dv = new DataView(arrayBuffer)
  let off = 0

  const magic = dv.getUint32(off, true); off += 4
  if (magic !== MAGIC) {
    throw new Error(`二进制扫描结果格式错误: magic=0x${magic.toString(16)}`)
  }
  const version = u8[off]; off += 1
  const flags = u8[off]; off += 1

  const totalSize = Number(dv.getBigInt64(off, true)); off += 8
  const scanTime = dv.getFloat64(off, true); off += 8
  const itemCount = dv.getUint32(off, true); off += 4
  const fileCount = dv.getUint32(off, true); off += 4
  const dirCount = dv.getUint32(off, true); off += 4
  const ioMs = dv.getFloat64(off, true); off += 8
  const computeMs = dv.getFloat64(off, true); off += 8
  const serializeMs = dv.getFloat64(off, true); off += 8

  const decoder = new TextDecoder('utf-8', { fatal: false })
  const readStr = () => {
    const len = dv.getUint32(off, true); off += 4
    const s = decoder.decode(u8.subarray(off, off + len)); off += len
    return s
  }

  const scannedPath = readStr()

  const items = new Array(itemCount)
  for (let i = 0; i < itemCount; i++) {
    const path = readStr()
    const name = readStr()
    const size = Number(dv.getBigInt64(off, true)); off += 8
    const isDir = u8[off] === 1; off += 1
    const mtime = version >= 2 ? Number(dv.getBigInt64(off, true)) : 0
    if (version >= 2) off += 8
    items[i] = { path, name, size, sizeFormatted: formatSize(size), isDir, mtime }

    // 大结果分块让出主线程，保持 UI 可响应
    if ((i + 1) % DECODE_CHUNK === 0 && i + 1 < itemCount) {
      onProgress?.(i + 1, itemCount)
      await new Promise((r) => setTimeout(r, 0))
    }
  }

  return {
    items,
    totalSize,
    totalSizeFormatted: formatSize(totalSize),
    scanTime,
    path: scannedPath,
    mftAvailable: (flags & 1) !== 0,
    perfMetrics: {
      filesScanned: fileCount,
      dirsScanned: dirCount,
      ioPhaseMs: ioMs,
      computePhaseMs: computeMs,
      serializePhaseMs: serializeMs
    }
  }
}

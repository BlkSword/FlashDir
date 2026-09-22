export function formatSize(bytes) {
  if (bytes < 1024) return `${bytes} B`
  const kb = bytes / 1024
  if (kb < 1024) return `${kb.toFixed(1)} KB`
  const mb = kb / 1024
  if (mb < 1024) return `${mb.toFixed(1)} MB`
  const gb = mb / 1024
  if (gb < 1024) return `${gb.toFixed(1)} GB`
  return `${(gb / 1024).toFixed(1)} TB`
}

/** 统一错误对象格式化（Tauri 错误可能是字符串、Error 或任意对象） */
export function formatError(e) {
  if (typeof e === 'string') return e
  return e?.message || String(e)
}

export function formatTime(timestamp) {
  if (!timestamp) return '-'

  const date = new Date(timestamp)
  const now = new Date()
  const diff = now - date

  // 小于 1 分钟
  if (diff < 60000) {
    return '刚刚'
  }

  // 小于 1 小时
  if (diff < 3600000) {
    const minutes = Math.floor(diff / 60000)
    return `${minutes} 分钟前`
  }

  // 小于 1 天
  if (diff < 86400000) {
    const hours = Math.floor(diff / 3600000)
    return `${hours} 小时前`
  }

  // 小于 7 天
  if (diff < 604800000) {
    const days = Math.floor(diff / 86400000)
    return `${days} 天前`
  }

  // 超过 7 天，显示具体日期
  const year = date.getFullYear()
  const month = String(date.getMonth() + 1).padStart(2, '0')
  const day = String(date.getDate()).padStart(2, '0')
  const hours = String(date.getHours()).padStart(2, '0')
  const minutes = String(date.getMinutes()).padStart(2, '0')

  return `${year}-${month}-${day} ${hours}:${minutes}`
}

export function formatScanTime(seconds) {
  if (seconds < 1) {
    return `${Math.round(seconds * 1000)}ms`
  }
  if (seconds < 60) {
    return `${seconds.toFixed(2)}秒`
  }
  const minutes = Math.floor(seconds / 60)
  const remainingSeconds = (seconds % 60).toFixed(0)
  return `${minutes}分${remainingSeconds}秒`
}

export function debounce(fn, delay) {
  let timeoutId = null
  return (...args) => {
    clearTimeout(timeoutId)
    timeoutId = setTimeout(() => fn(...args), delay)
  }
}

export function normalizePath(path) {
  if (!path) return ''
  return path.replace(/\\/g, '/')
}

export function getParentPath(path) {
  if (!path) return ''
  const normalized = normalizePath(path)
  const lastSlashIndex = normalized.lastIndexOf('/')
  if (lastSlashIndex <= 0) return '/'
  return normalized.substring(0, lastSlashIndex)
}


/** 紧凑容量（表格窄列用）：9.1G / 612M / 1.2K */
export function formatSizeCompact(bytes) {
  if (bytes === null || bytes === undefined) return '-'
  if (bytes < 1024) return `${bytes}B`
  const kb = bytes / 1024
  if (kb < 1024) return `${kb.toFixed(0)}K`
  const mb = kb / 1024
  if (mb < 1024) return `${mb < 10 ? mb.toFixed(1) : mb.toFixed(0)}M`
  const gb = mb / 1024
  if (gb < 1024) return `${gb < 10 ? gb.toFixed(1) : gb.toFixed(0)}G`
  return `${(gb / 1024).toFixed(1)}T`
}

/** 绝对时间：MM-DD HH:mm（同年省略年份） */
export function formatDateTime(ts) {
  if (!ts) return '-'
  const d = new Date(ts * 1000)
  const now = new Date()
  const p = (n) => String(n).padStart(2, '0')
  const md = `${p(d.getMonth() + 1)}-${p(d.getDate())}`
  const hm = `${p(d.getHours())}:${p(d.getMinutes())}`
  if (d.getFullYear() !== now.getFullYear()) return `${d.getFullYear()}-${md}`
  return `${md} ${hm}`
}

/** 相对时间：3 分钟前 / 2 天前 / 2024-06-11 */
export function formatRelative(ts) {
  if (!ts) return '-'
  const diff = Date.now() / 1000 - ts
  if (diff < 60) return '刚刚'
  if (diff < 3600) return `${Math.floor(diff / 60)} 分钟前`
  if (diff < 86400) return `${Math.floor(diff / 3600)} 小时前`
  if (diff < 86400 * 30) return `${Math.floor(diff / 86400)} 天前`
  return formatDateTime(ts)
}

/** 占比（0-100 的数字） */
export function percentOf(part, total) {
  if (!total || total <= 0) return 0
  return Math.min(100, (part / total) * 100)
}

/** 体积热力档位 1..5（用于占比条/热图着色，只表达数据） */
export function heatLevel(value, max) {
  if (!max || max <= 0) return 1
  const r = value / max
  if (r >= 0.5) return 5
  if (r >= 0.2) return 4
  if (r >= 0.08) return 3
  if (r >= 0.02) return 2
  return 1
}

export function heatVar(level) {
  return `var(--heat-${Math.min(5, Math.max(1, level))})`
}

/** 容量使用率 → 语义色（正常/注意/危险） */
export function usageClass(usedRatio) {
  if (usedRatio >= 0.9) return 'bad'
  if (usedRatio >= 0.75) return 'warn'
  return ''
}

/** 判断卷是否真的在更新"访问时间"：样本里 atime 普遍等于/早于 mtime 视为未启用 */
export function accessTimeReliable(items) {
  let checked = 0
  let same = 0
  for (const it of items) {
    if (it.isDir || !it.atime || !it.mtime) continue
    checked++
    if (it.atime <= it.mtime + 120) same++
    if (checked >= 200) break
  }
  if (checked < 10) return true
  return same / checked < 0.8
}

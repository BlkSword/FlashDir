export function formatSize(bytes) {
  if (bytes < 1024) return `${bytes} B`
  const kb = bytes / 1024
  if (kb < 1024) return `${kb.toFixed(1)} KB`
  const mb = kb / 1024
  if (mb < 1024) return `${mb.toFixed(1)} MB`
  const gb = mb / 1024
  return `${gb.toFixed(1)} GB`
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


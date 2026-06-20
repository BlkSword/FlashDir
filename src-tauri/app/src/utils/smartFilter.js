// Everything-style smart filter
// Supports:
//   ext:zip | ext:"tar.gz"
//   size:>100MB | size:>=1GB | size:1MB..10MB
//   dir:foo | dir:"Program Files"
//   type:file|dir|folder
//   name:keyword
//   mtime:>7d | mtime:<1h
//   plain text (matches name or path)
//   NOT keyword, NOT ext:zip
//   keyword1 AND keyword2 (explicit AND is allowed but default is already AND)

const SIZE_UNITS = {
  B: 1,
  KB: 1024,
  MB: 1024 * 1024,
  GB: 1024 * 1024 * 1024,
  TB: 1024 * 1024 * 1024 * 1024
}

const TIME_UNITS = {
  s: 1,
  m: 60,
  h: 60 * 60,
  d: 24 * 60 * 60,
  w: 7 * 24 * 60 * 60,
  mo: 30 * 24 * 60 * 60,
  y: 365 * 24 * 60 * 60
}

function tokenize(input) {
  const tokens = []
  let i = 0
  while (i < input.length) {
    const ch = input[i]
    if (/\s/.test(ch)) {
      i++
      continue
    }
    if (ch === '(' || ch === ')') {
      tokens.push({ type: 'paren', value: ch })
      i++
      continue
    }
    if (ch === '"') {
      let j = i + 1
      while (j < input.length && input[j] !== '"') j++
      tokens.push({ type: 'quoted', value: input.slice(i + 1, j) })
      i = j + 1
      continue
    }
    let j = i
    while (
      j < input.length &&
      !/\s/.test(input[j]) &&
      input[j] !== '(' &&
      input[j] !== ')' &&
      input[j] !== '"'
    ) {
      j++
    }
    const word = input.slice(i, j)
    const upper = word.toUpperCase()
    if (upper === 'AND' || upper === 'OR') {
      tokens.push({ type: 'bool', value: upper })
    } else if (upper === 'NOT') {
      tokens.push({ type: 'not' })
    } else if (/^(ext|size|dir|type|name|mtime):/i.test(word)) {
      const idx = word.indexOf(':')
      tokens.push({
        type: 'keyword',
        key: word.slice(0, idx).toLowerCase(),
        value: word.slice(idx + 1)
      })
    } else {
      tokens.push({ type: 'text', value: word })
    }
    i = j
  }
  return tokens
}

function parseKeyword(key, value, negate = false) {
  switch (key) {
    case 'ext': {
      return { type: 'ext', value: value.toLowerCase(), negate }
    }
    case 'size': {
      const m = value.match(/^(>=?|<=?|!=|=)?(\d+(?:\.\d+)?)\s*(KB|MB|GB|TB|B)?$/i)
      if (!m) return null
      const op = m[1] || '>='
      const bytes = parseFloat(m[2]) * (SIZE_UNITS[m[3]] || 1)
      return { type: 'size', operator: op, value: Math.round(bytes), negate }
    }
    case 'mtime': {
      const m = value.match(/^(>=?|<=?|!=|=)?(\d+(?:\.\d+)?)\s*(s|m|h|d|w|mo|y)?$/i)
      if (!m) return null
      const op = m[1] || '<='
      const seconds = parseFloat(m[2]) * (TIME_UNITS[m[3]] || TIME_UNITS.d)
      return { type: 'mtime', operator: op, value: Math.round(seconds), negate }
    }
    case 'dir': {
      return { type: 'dir', value: value.toLowerCase(), negate }
    }
    case 'type': {
      const v = value.toLowerCase()
      return { type: 'type', value: v === 'folder' ? 'dir' : v, negate }
    }
    case 'name': {
      return { type: 'name', value: value.toLowerCase(), negate }
    }
  }
  return null
}

/**
 * Parse a filter string into structured filters
 * Returns: { text, filters: [{ type, operator?, value, negate? }] }
 */
export function parseFilter(input = '') {
  const trimmed = input.trim()
  if (!trimmed) return { text: '', filters: [] }

  const tokens = tokenize(trimmed)
  const filters = []
  const textParts = []
  let negateNext = false

  for (let i = 0; i < tokens.length; i++) {
    const tok = tokens[i]
    if (tok.type === 'keyword') {
      const f = parseKeyword(tok.key, tok.value, negateNext)
      negateNext = false
      if (f) filters.push(f)
    } else if (tok.type === 'not') {
      negateNext = true
    } else if (tok.type === 'bool') {
      // 当前默认所有条件均为 AND；显式 AND/OR 仅作为分隔符忽略
      negateNext = false
    } else if (tok.type === 'text' || tok.type === 'quoted') {
      const value = tok.value.toLowerCase()
      if (negateNext) {
        filters.push({ type: 'text', value, negate: true })
        negateNext = false
      } else {
        textParts.push(value)
      }
    }
  }

  const text = textParts.join(' ').trim()
  if (text) {
    filters.unshift({ type: 'text', value: text, negate: false })
  }

  return { text, filters }
}

function compareSize(size, op, value) {
  switch (op) {
    case '>': return size > value
    case '>=': return size >= value
    case '<': return size < value
    case '<=': return size <= value
    case '=': return size === value
    case '!=': return size !== value
    default: return size >= value
  }
}

function compareMtime(itemMtime, op, valueSeconds) {
  if (!itemMtime) return false
  const now = Date.now() / 1000
  const ageSeconds = now - itemMtime
  switch (op) {
    case '>': return ageSeconds > valueSeconds
    case '>=': return ageSeconds >= valueSeconds
    case '<': return ageSeconds < valueSeconds
    case '<=': return ageSeconds <= valueSeconds
    case '=': return Math.round(ageSeconds) === valueSeconds
    case '!=': return Math.round(ageSeconds) !== valueSeconds
    default: return ageSeconds <= valueSeconds
  }
}

function applyFilter(item, filter) {
  let matched = false
  switch (filter.type) {
    case 'text': {
      const text = filter.value
      const name = (item.name || '').toLowerCase()
      const path = (item.path || '').toLowerCase()
      matched = name.includes(text) || path.includes(text)
      break
    }
    case 'name': {
      matched = (item.name || '').toLowerCase().includes(filter.value)
      break
    }
    case 'ext': {
      if (item.isDir) {
        matched = false
      } else {
        const name = (item.name || '').toLowerCase()
        // 支持 ext:tar.gz（取最后一个点后的全部）或 ext:zip
        const dotIdx = name.lastIndexOf('.')
        const ext = dotIdx >= 0 ? name.slice(dotIdx + 1) : ''
        matched = ext === filter.value
      }
      break
    }
    case 'size': {
      matched = compareSize(item.size || 0, filter.operator, filter.value)
      break
    }
    case 'mtime': {
      matched = compareMtime(item.mtime, filter.operator, filter.value)
      break
    }
    case 'dir': {
      matched = (item.path || '').toLowerCase().includes(filter.value)
      break
    }
    case 'type': {
      if (filter.value === 'dir') matched = !!item.isDir
      else if (filter.value === 'file') matched = !item.isDir
      else matched = false
      break
    }
    default:
      matched = true
  }
  return filter.negate ? !matched : matched
}

/**
 * Apply parsed filters to an array of items
 * Returns filtered items
 */
export function applySmartFilter(items, filterInput) {
  if (!filterInput || !filterInput.trim()) return items

  const { filters } = parseFilter(filterInput)
  if (filters.length === 0) return items

  return items.filter(item => {
    for (const filter of filters) {
      if (!applyFilter(item, filter)) return false
    }
    return true
  })
}

/**
 * Get filter hints for the UI
 */
export function getFilterHints() {
  return [
    { syntax: 'ext:zip', description: '按扩展名过滤' },
    { syntax: 'ext:"tar.gz"', description: '含点的扩展名' },
    { syntax: 'size:>100MB', description: '大于 100MB 的文件' },
    { syntax: 'size:<1GB', description: '小于 1GB 的文件' },
    { syntax: 'type:dir', description: '仅显示目录' },
    { syntax: 'type:file', description: '仅显示文件' },
    { syntax: 'name:report', description: '文件名包含' },
    { syntax: 'dir:node_modules', description: '路径包含...' },
    { syntax: 'mtime:>7d', description: '修改时间超过 7 天' },
    { syntax: 'NOT .tmp', description: '排除 .tmp' }
  ]
}

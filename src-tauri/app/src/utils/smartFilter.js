// Everything-style smart filter
// Supports: ext:zip  size:>100MB  size:<10KB  name:keyword  plain text

/**
 * Parse a filter string into structured filters
 * Returns: { text, filters: [{ type, operator?, value }] }
 */
export function parseFilter(input = '') {
  const trimmed = input.trim()
  if (!trimmed) return { text: '', filters: [] }

  const filters = []
  let text = trimmed

  // ext:xxx — filter by extension
  const extRegex = /\bext:(\w+)/gi
  text = text.replace(extRegex, (_, ext) => {
    filters.push({ type: 'ext', value: ext.toLowerCase() })
    return ''
  })

  // size:>xxx or size:>=xxx or size:<xxx or size:<=xxx
  const sizeRegex = /\bsize:(>=?|<=?)?(\d+(?:\.\d+)?)\s*(KB|MB|GB|TB|B)?\b/gi
  text = text.replace(sizeRegex, (_, op, num, unit) => {
    const multiplier = { B: 1, KB: 1024, MB: 1024 * 1024, GB: 1024 * 1024 * 1024, TB: 1024 * 1024 * 1024 * 1024 }
    const bytes = parseFloat(num) * (multiplier[unit] || 1)
    filters.push({ type: 'size', operator: op || '>', value: Math.round(bytes) })
    return ''
  })

  // dir:xxx — filter by directory path
  const dirRegex = /\bdir:(\S+)/gi
  text = text.replace(dirRegex, (_, dir) => {
    filters.push({ type: 'dir', value: dir.toLowerCase() })
    return ''
  })

  // type:file or type:dir or type:folder
  const typeRegex = /\btype:(file|dir|folder)\b/gi
  text = text.replace(typeRegex, (_, typeVal) => {
    filters.push({ type: 'type', value: typeVal === 'folder' ? 'dir' : typeVal })
    return ''
  })

  // Clean up extra whitespace
  text = text.replace(/\s+/g, ' ').trim()

  return { text: text.toLowerCase(), filters }
}

/**
 * Apply parsed filters to an array of items
 * Returns filtered items
 */
export function applySmartFilter(items, filterInput) {
  if (!filterInput || !filterInput.trim()) return items

  const { text, filters } = parseFilter(filterInput)
  if (filters.length === 0 && !text) return items

  return items.filter(item => {
    // Plain text search (match name)
    if (text && !item.name.toLowerCase().includes(text)) {
      // Also try matching against path
      if (!item.path.toLowerCase().includes(text)) {
        return false
      }
    }

    for (const filter of filters) {
      switch (filter.type) {
        case 'ext': {
          if (item.isDir) return false
          const ext = item.name.split('.').pop()?.toLowerCase()
          if (ext !== filter.value) return false
          break
        }
        case 'size': {
          const size = item.size || 0
          switch (filter.operator) {
            case '>': if (!(size > filter.value)) return false; break
            case '>=': if (!(size >= filter.value)) return false; break
            case '<': if (!(size < filter.value)) return false; break
            case '<=': if (!(size <= filter.value)) return false; break
            default: if (!(size >= filter.value)) return false; break
          }
          break
        }
        case 'dir': {
          if (!item.path.toLowerCase().includes(filter.value)) return false
          break
        }
        case 'type': {
          if (filter.value === 'dir' && !item.isDir) return false
          if (filter.value === 'file' && item.isDir) return false
          break
        }
      }
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
    { syntax: 'size:>100MB', description: '大于 100MB 的文件' },
    { syntax: 'size:<1GB', description: '小于 1GB 的文件' },
    { syntax: 'type:dir', description: '仅显示目录' },
    { syntax: 'type:file', description: '仅显示文件' },
    { syntax: 'dir:node_modules', description: '路径包含...' },
  ]
}

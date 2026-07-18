// 排序/过滤工具（同步实现）。
// 历史上这里有一个 Blob Worker 异步路径，但所有调用方都只走同步分支，
// Worker 代码从未生效，已精简移除。文件列表按页渲染（≤1000 行/页），
// 同步排序在当前数据规模下足够快。

export function useSortWorker() {
  function sortItemsSync(items, sortColumn, sortDirection) {
    return [...items].sort((a, b) => {
      let aVal, bVal
      switch (sortColumn) {
        case 'name':
          aVal = a.name || a.path
          bVal = b.name || b.path
          return sortDirection === 'asc'
            ? aVal.localeCompare(bVal, 'zh-CN')
            : bVal.localeCompare(aVal, 'zh-CN')
        case 'type':
          aVal = a.isDir ? 0 : 1
          bVal = b.isDir ? 0 : 1
          if (aVal !== bVal) return sortDirection === 'asc' ? aVal - bVal : bVal - aVal
          aVal = a.name || a.path
          bVal = b.name || b.path
          return sortDirection === 'asc'
            ? aVal.localeCompare(bVal, 'zh-CN')
            : bVal.localeCompare(aVal, 'zh-CN')
        case 'size':
          aVal = a.size || 0
          bVal = b.size || 0
          return sortDirection === 'asc' ? aVal - bVal : bVal - aVal
        case 'mtime':
          aVal = a.mtime || 0
          bVal = b.mtime || 0
          return sortDirection === 'asc' ? aVal - bVal : bVal - aVal
        default:
          return 0
      }
    })
  }

  function filterItemsSync(items, keyword) {
    if (!keyword || !keyword.trim()) return items
    const lowerKeyword = keyword.toLowerCase().trim()
    return items.filter(item =>
      item.name.toLowerCase().includes(lowerKeyword) ||
      item.path.toLowerCase().includes(lowerKeyword)
    )
  }

  return {
    sortItemsSync,
    filterItemsSync
  }
}

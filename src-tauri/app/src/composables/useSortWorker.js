import { ref, onUnmounted } from 'vue'

let workerInstance = null
let workerReady = false
let pendingCallbacks = new Map()
let callbackId = 0

function getWorker() {
  if (!workerInstance) {
    const workerCode = `
      function sortItems(items, sortColumn, sortDirection) {
        return [...items].sort((a, b) => {
          let aVal, bVal;
          switch (sortColumn) {
            case 'name':
              aVal = a.name || a.path;
              bVal = b.name || b.path;
              return sortDirection === 'asc'
                ? aVal.localeCompare(bVal, 'zh-CN')
                : bVal.localeCompare(aVal, 'zh-CN');
            case 'type':
              aVal = a.isDir ? 0 : 1;
              bVal = b.isDir ? 0 : 1;
              if (aVal !== bVal) {
                return sortDirection === 'asc' ? aVal - bVal : bVal - aVal;
              }
              aVal = a.name || a.path;
              bVal = b.name || b.path;
              return sortDirection === 'asc'
                ? aVal.localeCompare(bVal, 'zh-CN')
                : bVal.localeCompare(aVal, 'zh-CN');
            case 'size':
              aVal = a.size || 0;
              bVal = b.size || 0;
              return sortDirection === 'asc' ? aVal - bVal : bVal - aVal;
            default:
              return 0;
          }
        });
      }

      function filterItems(items, keyword) {
        if (!keyword || !keyword.trim()) return items;
        const lowerKeyword = keyword.toLowerCase().trim();
        return items.filter(item => {
          return item.name.toLowerCase().includes(lowerKeyword) ||
                 item.path.toLowerCase().includes(lowerKeyword);
        });
      }

      self.onmessage = function(e) {
        const { type, data, id } = e.data;
        let result;
        switch (type) {
          case 'sort':
            result = sortItems(data.items, data.sortColumn, data.sortDirection);
            break;
          case 'filter':
            result = filterItems(data.items, data.keyword);
            break;
          case 'sortAndFilter':
            const filtered = filterItems(data.items, data.keyword);
            result = sortItems(filtered, data.sortColumn, data.sortDirection);
            break;
          case 'paginate':
            const { items, page, pageSize } = data;
            const start = (page - 1) * pageSize;
            const end = start + pageSize;
            result = { items: items.slice(start, end), total: items.length };
            break;
        }
        self.postMessage({ type, result, id });
      };
      self.postMessage({ type: 'ready' });
    `

    const blob = new Blob([workerCode], { type: 'application/javascript' })
    workerInstance = new Worker(URL.createObjectURL(blob))

    workerInstance.onmessage = (e) => {
      const { type, result, id } = e.data

      if (type === 'ready') {
        workerReady = true
        return
      }

      if (id !== undefined && pendingCallbacks.has(id)) {
        const callback = pendingCallbacks.get(id)
        pendingCallbacks.delete(id)
        callback(result)
      }
    }

    workerInstance.onerror = (error) => {
      console.error('Worker error:', error)
    }
  }

  return workerInstance
}

export function useSortWorker() {
  const isProcessing = ref(false)
  const lastProcessTime = ref(0)

  function processInWorker(type, data) {
    return new Promise((resolve, reject) => {
      const worker = getWorker()

      if (!worker) {
        reject(new Error('Worker not available'))
        return
      }

      if (data.items && data.items.length < 500) {
        resolve(processLocally(type, data))
        return
      }

      isProcessing.value = true
      const id = callbackId++

      pendingCallbacks.set(id, (result) => {
        isProcessing.value = false
        lastProcessTime.value = performance.now()
        resolve(result)
      })

      setTimeout(() => {
        if (pendingCallbacks.has(id)) {
          pendingCallbacks.delete(id)
          isProcessing.value = false
          reject(new Error('Worker timeout'))
        }
      }, 10000)

      worker.postMessage({ type, data, id })
    })
  }

  function processLocally(type, data) {
    switch (type) {
      case 'sort':
        return sortItemsSync(data.items, data.sortColumn, data.sortDirection)
      case 'filter':
        return filterItemsSync(data.items, data.keyword)
      case 'sortAndFilter':
        const filtered = filterItemsSync(data.items, data.keyword)
        return sortItemsSync(filtered, data.sortColumn, data.sortDirection)
      default:
        return data.items
    }
  }

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

  async function sort(items, sortColumn, sortDirection) {
    return processInWorker('sort', { items, sortColumn, sortDirection })
  }

  async function filter(items, keyword) {
    return processInWorker('filter', { items, keyword })
  }

  async function sortAndFilter(items, sortColumn, sortDirection, keyword) {
    return processInWorker('sortAndFilter', { items, sortColumn, sortDirection, keyword })
  }

  onUnmounted(() => {
    pendingCallbacks.clear()
  })

  return {
    isProcessing,
    lastProcessTime,
    sort,
    filter,
    sortAndFilter,
    sortItemsSync,
    filterItemsSync,
    isReady: () => workerReady
  }
}

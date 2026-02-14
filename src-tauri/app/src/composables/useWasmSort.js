import { ref, computed } from 'vue'

const wasmInitialized = ref(false)
const wasmError = ref(null)
let wasmModule = null

export function useWasmSort() {
  const isReady = computed(() => wasmInitialized.value)
  const error = computed(() => wasmError.value)

  async function initialize() {
    if (wasmInitialized.value) return true

    try {
      // 动态导入 WASM 模块
      const wasmPath = '../../../wasm-sort/pkg/flashdir_sort.js'
      const module = await import(/* @vite-ignore */ wasmPath)

      // WASM 模块会自动初始化（因为有 #[wasm_bindgen(start)]）
      wasmModule = module
      wasmInitialized.value = true
      wasmError.value = null
      return true
    } catch (err) {
      wasmError.value = err.message
      console.warn('WASM initialization failed (using fallback sort):', err.message)
      return false
    }
  }

  function sortFiles(files, field = 'size', order = 'desc') {
    // 如果 WASM 可用，使用 WASM 排序
    if (wasmInitialized.value && wasmModule) {
      try {
        const { sort_items } = wasmModule

        // 转换文件格式
        const wasmItems = files.map(f => ({
          path: f.path || '',
          name: f.name || '',
          size: f.size || 0,
          sizeFormatted: f.sizeFormatted || f.size_formatted || '',
          isDir: f.isDir || f.is_dir || false
        }))

        const sorted = sort_items(wasmItems, field, order)

        return sorted.map((f, index) => ({
          ...files.find(file => file.path === f.path),
          sortIndex: index
        }))
      } catch (err) {
        console.warn('WASM sort failed, using fallback:', err)
      }
    }

    // 回退到 JavaScript 排序
    return sortFilesFallback(files, field, order)
  }

  function sortFilesFallback(files, field, order) {
    const sorted = [...files]

    sorted.sort((a, b) => {
      let comparison = 0

      switch (field) {
        case 'name':
          comparison = (a.name || '').localeCompare(b.name || '')
          break
        case 'size':
          comparison = (a.size || 0) - (b.size || 0)
          break
        case 'type':
          const aIsDir = a.isDir || a.is_dir || false
          const bIsDir = b.isDir || b.is_dir || false
          comparison = (aIsDir ? 0 : 1) - (bIsDir ? 0 : 1)
          if (comparison === 0) {
            comparison = (a.name || '').localeCompare(b.name || '')
          }
          break
        default:
          comparison = (a.size || 0) - (b.size || 0)
      }

      return order === 'desc' ? -comparison : comparison
    })

    return sorted.map((file, index) => ({
      ...file,
      sortIndex: index
    }))
  }

  function filterFiles(files, keyword) {
    if (!keyword || keyword.trim() === '') {
      return files
    }

    const lowerKeyword = keyword.toLowerCase()

    return files.filter(file => {
      const name = (file.name || '').toLowerCase()
      const path = (file.path || '').toLowerCase()
      return name.includes(lowerKeyword) || path.includes(lowerKeyword)
    })
  }

  return {
    isReady,
    error,
    initialize,
    sortFiles,
    filterFiles
  }
}

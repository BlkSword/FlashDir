import { invoke } from '@tauri-apps/api/core'

/**
 * 调用后端全局搜索并归一化返回值。
 *
 * 后端 `global_search` 返回的是对象：
 *   { ready, state, results, indexSize?, sampleNames? }   // camelCase（serde rename_all）
 * 早期前端直接把它当数组使用（`rows.map(...)`），会抛
 * "…map is not a function" 并让命令面板渲染失败。这里统一收敛。
 */
export async function searchGlobal(query, limit = 500) {
  const res = await invoke('global_search', { query, limit })

  // 兼容极少数情况下直接返回数组的实现
  if (Array.isArray(res)) {
    return { ready: true, state: null, results: res, indexSize: null, sampleNames: [] }
  }

  return {
    ready: !!res?.ready,
    state: res?.state ?? null,
    results: Array.isArray(res?.results) ? res.results : [],
    indexSize: typeof res?.indexSize === 'number' ? res.indexSize : null,
    sampleNames: Array.isArray(res?.sampleNames) ? res.sampleNames : [],
  }
}

import { invoke } from '@tauri-apps/api/core'

/**
 * 调用后端全局搜索并归一化返回值。
 *
 * 后端 `global_search` 返回对象：
 *   { ready, state, results, total, truncated, indexSize?, sampleNames? }  // camelCase
 * 早期前端直接把它当数组用（`rows.map(...)`）会抛 "…map is not a function"，
 * 导致命令面板渲染失败；这里统一收敛，并补齐分页所需字段。
 *
 * @param {string} query  查询表达式
 * @param {{limit?:number, offset?:number}|number} [opts] 兼容旧的数字形式（limit）
 */
export async function searchGlobal(query, opts = {}) {
  const { limit = 500, offset = 0 } = typeof opts === 'number' ? { limit: opts } : opts
  const res = await invoke('global_search', { query, limit, offset })

  if (Array.isArray(res)) {
    return {
      ready: true,
      state: null,
      results: res,
      total: res.length,
      truncated: false,
      indexSize: null,
      sampleNames: [],
    }
  }

  const results = Array.isArray(res?.results) ? res.results : []
  return {
    ready: !!res?.ready,
    state: res?.state ?? null,
    results,
    total: typeof res?.total === 'number' ? res.total : results.length,
    truncated: !!res?.truncated,
    indexSize: typeof res?.indexSize === 'number' ? res.indexSize : null,
    sampleNames: Array.isArray(res?.sampleNames) ? res.sampleNames : [],
  }
}

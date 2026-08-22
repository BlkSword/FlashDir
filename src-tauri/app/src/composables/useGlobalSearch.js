// 全局搜索状态单源（App 状态栏与 GlobalSearchDropdown 共享）
//
// 之前 App.vue 和 GlobalSearchDropdown 各自 invoke('global_search_status')、
// 各自 listen('global-search-progress')，两份状态可能不一致。
// 现在事件监听与状态在此模块内单例维护，所有消费者读取同一份。

import { reactive, computed } from 'vue'
import { listen } from '@tauri-apps/api/event'
import { useTauri } from './useTauri'

const { invoke } = useTauri()

const state = reactive({
  // global_search_status 返回值：{ kind: 'notLoaded' | 'loading' | 'ready' | 'failed', data? }
  index: { kind: 'notLoaded' },
  // global-search-progress 事件载荷：{ drive, scanned, phase }
  progress: null,
})

let listenStarted = false
let pollTimer = null

function stopPolling() {
  if (pollTimer) {
    clearInterval(pollTimer)
    pollTimer = null
  }
}

function startPolling() {
  if (pollTimer) return
  pollTimer = setInterval(() => {
    fetchStatus()
  }, 1000)
}

async function fetchStatus() {
  try {
    state.index = await invoke('global_search_status')
    if (state.index?.kind === 'loading') {
      startPolling()
    } else {
      stopPolling()
    }
  } catch (e) {
    console.error('获取全局索引状态失败:', e)
  }
}

function ensureListener() {
  if (listenStarted) return
  listenStarted = true
  listen('global-search-progress', (event) => {
    state.progress = event.payload
    if (event.payload?.phase === 'done') {
      stopPolling()
      fetchStatus()
    } else if (event.payload?.phase === 'loading-persisted') {
      state.index = { kind: 'loading', data: { drive: '索引缓存', scanned: 0 } }
      startPolling()
    } else {
      state.index = {
        kind: 'loading',
        data: { drive: event.payload?.drive || '', scanned: event.payload?.scanned || 0 }
      }
      startPolling()
    }
  })
}

// 模块加载即启动监听（原 App.vue onMounted 行为），并拉取一次当前状态
ensureListener()
fetchStatus()

export function useGlobalSearch() {
  const kind = computed(() => state.index?.kind)
  const ready = computed(() => kind.value === 'ready')
  const loading = computed(() => kind.value === 'loading')
  const failed = computed(() => kind.value === 'failed')
  const failedReason = computed(() =>
    kind.value === 'failed' ? (state.index?.data?.reason || '未知错误') : ''
  )
  // ready 时的索引元数据（fileCount / dirCount / driveCount / failedDrives）
  const indexMeta = computed(() => (ready.value ? state.index.data : null))

  const statusText = computed(() => {
    if (loading.value) {
      if (state.progress?.phase === 'loading-persisted') {
        return '全局索引：正在加载索引缓存…'
      }
      const drive = state.progress?.drive || state.index?.data?.drive || '…'
      const scanned = (state.progress?.scanned > 0
        ? state.progress.scanned
        : state.index?.data?.scanned) || 0
      return `全局索引：正在扫描 ${drive} · ${scanned.toLocaleString()} 项`
    }
    if (failed.value) {
      return `全局索引失败：${failedReason.value}`
    }
    if (ready.value) {
      const data = state.index.data
      const total = (data?.fileCount || 0) + (data?.dirCount || 0)
      return `全局索引就绪 · ${total.toLocaleString()} 项`
    }
    return ''
  })

  const search = async (query, limit) => {
    return await invoke('global_search', { query, limit })
  }

  const ensureIndex = async () => {
    await invoke('global_search_ensure_index')
    await fetchStatus()
    return state.index
  }

  const refreshIndex = async () => {
    await invoke('global_search_refresh')
    await fetchStatus()
  }

  const restartAsAdmin = async () => {
    await invoke('restart_as_admin')
  }

  return {
    state,
    kind,
    ready,
    loading,
    failed,
    failedReason,
    indexMeta,
    statusText,
    search,
    fetchStatus,
    ensureIndex,
    refreshIndex,
    restartAsAdmin,
  }
}

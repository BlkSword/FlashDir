// 全局搜索状态单源（App 状态栏与 GlobalSearchDropdown 共享）
//
// 之前 App.vue 和 GlobalSearchDropdown 各自 invoke('global_search_status')、
// 各自 listen('global-search-progress')，两份状态可能不一致。
// 现在事件监听与状态在此模块内单例维护，所有消费者读取同一份。

import { computed, reactive, ref } from 'vue'
import { listen } from '@tauri-apps/api/event'
import { invoke } from '@tauri-apps/api/core'

const state = reactive({
  // global_search_status 返回值：{ kind: 'notLoaded' | 'loading' | 'ready' | 'failed', data? }
  index: { kind: 'notLoaded' },
  // global-search-progress 事件载荷：{ drive, scanned, phase }
  progress: null,
})

let listenStarted = false
let pollTimer = null
let pollInterval = 0
let visibilityBound = false
/** 处于 notLoaded 时已轮询的次数：只在启动后一段有界时间内轮询，避免永久空转 IPC */
let idlePollCount = 0
const IDLE_POLL_MS = 3000
const IDLE_POLL_MAX = 40

function stopPolling() {
  if (pollTimer) {
    clearInterval(pollTimer)
    pollTimer = null
  }
  pollInterval = 0
}

/** 页面是否处于前台；后台标签页不该继续空转 IPC */
function isVisible() {
  return typeof document === 'undefined' || document.visibilityState !== 'hidden'
}

/** 可见性变化：隐藏时立刻停表，恢复前台时立即补一次查询（避免等到下一个 tick） */
function onVisibilityChange() {
  if (isVisible()) {
    fetchStatus()
  } else {
    stopPolling()
  }
}

function bindVisibility() {
  if (visibilityBound || typeof document === 'undefined') return
  visibilityBound = true
  document.addEventListener('visibilitychange', onVisibilityChange)
}

function startPolling(intervalMs = 1000) {
  if (pollTimer && pollInterval === intervalMs) return
  if (pollTimer) stopPolling()
  // 旧实现无条件 setInterval：窗口最小化/被切走后仍每秒 invoke 一次，
  // 进程常驻期间持续产生后台 IPC。隐藏时不启动。
  if (!isVisible()) return
  bindVisibility()
  pollInterval = intervalMs
  pollTimer = setInterval(() => {
    if (!isVisible()) return
    fetchStatus()
  }, 1000)
}

/** 是否运行在 Tauri 容器内（纯浏览器/预览环境下不调用 IPC，避免模块加载期抛错导致界面空白） */
function hasTauri() {
  return typeof window !== 'undefined' && !!window.__TAURI_INTERNALS__
}

async function fetchStatus() {
  if (!hasTauri()) return
  try {
    state.index = await invoke('global_search_status')
    const kind = state.index?.kind
    if (kind === 'loading') {
      idlePollCount = 0
      startPolling(1000)
    } else if (kind === 'notLoaded' && idlePollCount < IDLE_POLL_MAX) {
      // 首次启动时后端"开始构建"的进度事件可能早于 webview 注册监听（事件会丢），
      // 只依赖事件就会让整个构建过程在界面上一直停在"未建立"。
      // 因此 notLoaded 也在启动后一段**有界**时间内轮询（3s 一次、最多 IDLE_POLL_MAX 次）：
      // 后端一转入 Loading 就立刻切成 1s 轮询并显示"构建中 · Ns"，
      // 而长期没有索引时也不会永久空转 IPC。
      idlePollCount += 1
      startPolling(IDLE_POLL_MS)
    } else {
      stopPolling()
    }
  } catch (e) {
    console.error('获取全局索引状态失败:', e)
  }
}

function ensureListener() {
  if (listenStarted || !hasTauri()) return
  listenStarted = true
  try {
    listen('global-search-progress', (event) => {
    state.progress = event.payload
    if (event.payload?.phase === 'done') {
      stopPolling()
      fetchStatus()
    } else if (event.payload?.phase === 'loading-persisted') {
      state.index = { kind: 'loading', data: { drive: '索引缓存', scanned: 0 } }
      idlePollCount = 0
      startPolling(1000)
    } else {
      state.index = {
        kind: 'loading',
        data: { drive: event.payload?.drive || '', scanned: event.payload?.scanned || 0 }
      }
      idlePollCount = 0
      startPolling(1000)
    }
    })
  } catch (e) {
    // 事件订阅失败不应影响界面：状态保持未加载，搜索时再按需重试
    console.warn('[GlobalIndex] 事件订阅失败:', e)
  }
}

// 模块加载即启动监听（原 App.vue onMounted 行为），并拉取一次当前状态
ensureListener()
bindVisibility()
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

    // USN 增量同步状态（就绪后可见）：最近同步时间与"增量窗口已失效"的盘
    const usnLastSync = computed(() => (ready.value ? state.index.data?.usnLastSync || 0 : 0))
    const usnStaleDrives = computed(() => (ready.value ? state.index.data?.usnStaleDrives || [] : []))
    const syncing = ref(false)

    /** 手动触发一次 USN 增量同步；后端有预算，未追平会留到下次继续 */
    const syncNow = async () => {
      if (syncing.value) return null
      syncing.value = true
      try {
        const summary = await invoke('global_search_sync_now')
        await fetchStatus()
        return summary
      } finally {
        syncing.value = false
      }
    }

  // 已删除未被任何调用方使用的 search(query, limit) 导出：
  // 搜索统一走 utils/globalSearchApi.js 的 searchGlobal()，此处保留只会造成两套入口
  //
  // 同时删除 statusText：它从未被任何组件使用，而 App.vue 已按"进度事件 + 已用时长"
  // 生成状态栏与进度卡文案（indexBusyLabel / indexProgressText）。保留两份文案逻辑
  // 只会让它们随进度事件字段变化而悄悄漂移。
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
    fetchStatus,
    ensureIndex,
    refreshIndex,
        syncNow,
        syncing,
        usnLastSync,
        usnStaleDrives,
    restartAsAdmin,
  }
}

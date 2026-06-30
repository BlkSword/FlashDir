<template>
  <div class="fd-app" :class="{ 'fd-sidebar-collapsed': sidebarCollapsed }">
    <Toolbar
      :path="currentPath"
      :can-go-back="canGoBack"
      :can-go-forward="canGoForward"
      :can-go-up="canGoUp"
      :loading="loading"
      @scan="handleScan"
      @browse="handleBrowse"
      @navigate="handleNavigate"
      @show-history="historyVisible = true"
      @open-global-search="globalSearchVisible = true"
      @toggle-sidebar="sidebarCollapsed = !sidebarCollapsed"
    />

    <Sidebar
      :tree-data="treeData"
      :selected-path="currentPath"
      :history="history"
      :collapsed="sidebarCollapsed"
      @select="handleSelectPath"
      @quick-access="handleQuickAccess"
    />

    <main class="fd-main">
      <FileList
        :items="displayItems"
        :loading="loading || sortWorker.isProcessing.value"
        :total-size="totalSize"
        :current-path="currentPath"
        :sort-config="sortConfig"
        :current-page="currentPage"
        :page-size="pageSize"
        :total-items="filteredTotalItems"
        @sort="handleSort"
        @select="handleSelectItem"
        @page-change="handlePageChange"
        @size-change="handleSizeChange"
        @filter="handleSearchInput"
      />
    </main>

    <RightPanel
      :items="allItems"
      :total-size="totalSize"
      :current-path="currentPath"
      :active-tab="rightPanelTab"
      :scan-time="scanTime"
      @update:active-tab="rightPanelTab = $event"
    />

    <StatusBar
      :path="currentPath"
      :total-items="totalItems"
      :total-size="totalSize"
      :scan-time="scanTime"
      :backend-time="backendTime"
      :loading="loading"
      :mft-available="mftAvailable"
      :is-admin="isAdmin"
      :global-search-loading="globalSearchLoading"
      :global-search-failed="globalSearchFailed"
      :global-search-status="globalSearchStatusText"
    />

    <a-modal
      :open="historyVisible"
      title="历史记录"
      width="800px"
      :footer="null"
      @cancel="historyVisible = false"
    >
      <HistoryList
        :history="history"
        @select="handleSelectHistory"
        @clear="handleClearHistory"
      />
    </a-modal>

    <GlobalSearchModal
      v-model:visible="globalSearchVisible"
      @open-dir="handleOpenDirFromSearch"
    />
  </div>
</template>

<script setup>
import { ref, computed, watch, onMounted, onUnmounted, shallowRef, triggerRef } from 'vue'
import { message } from 'ant-design-vue'
import { listen } from '@tauri-apps/api/event'
import Toolbar from './components/Toolbar.vue'
import Sidebar from './components/Sidebar.vue'
import FileList from './components/FileList.vue'
import RightPanel from './components/RightPanel.vue'
import StatusBar from './components/StatusBar.vue'
import HistoryList from './components/HistoryList.vue'
import GlobalSearchModal from './components/GlobalSearchModal.vue'
import { useTauri } from './composables/useTauri'
import { useSortWorker } from './composables/useSortWorker'
import { debounce, getParentPath } from './utils/format.js'
import { applySmartFilter } from './utils/smartFilter.js'
import { homeDir, join } from '@tauri-apps/api/path'

const { invoke, openDialog } = useTauri()
const sortWorker = useSortWorker()

let unlistenScanBatch = null
const streamedItemCount = ref(0)

const currentPath = ref('')
const allItems = shallowRef([])
const loading = ref(false)
const scanTime = ref(0)
const backendTime = ref(0)
const treeData = shallowRef([])
const history = shallowRef([])
const mftAvailable = ref(false)
const isAdmin = ref(false)

const navigationHistory = ref([])
const navigationIndex = ref(-1)

const currentPage = ref(1)
const pageSize = ref(100)

const sortConfig = ref({
  column: 'size',
  direction: 'desc'
})

const searchKeyword = ref('')
const historyVisible = ref(false)
const globalSearchVisible = ref(false)
const rightPanelTab = ref('stats')
const sidebarCollapsed = ref(false)
const globalSearchState = ref({ kind: 'notLoaded' })
const globalSearchProgress = ref(null)

let unlistenGlobalSearchProgress = null

const globalSearchLoading = computed(() => globalSearchState.value?.kind === 'loading')
const globalSearchFailed = computed(() => globalSearchState.value?.kind === 'failed')
const globalSearchStatusText = computed(() => {
  const kind = globalSearchState.value?.kind
  if (kind === 'loading') {
    const drive = globalSearchProgress.value?.drive || globalSearchState.value?.data?.drive || '…'
    const scanned = globalSearchProgress.value?.scanned ?? globalSearchState.value?.data?.scanned ?? 0
    return `全局索引：正在扫描 ${drive} · ${scanned.toLocaleString()} 项`
  }
  if (kind === 'failed') {
    return `全局索引失败：${globalSearchState.value?.data?.reason || '未知错误'}`
  }
  if (kind === 'ready') {
    const data = globalSearchState.value?.data
    const total = (data?.fileCount || 0) + (data?.dirCount || 0)
    return `全局索引就绪 · ${total.toLocaleString()} 项`
  }
  return ''
})

const lastSortKey = ref('')
const presortedAllItems = shallowRef([])

const canGoBack = computed(() => navigationIndex.value > 0)
const canGoForward = computed(() => navigationIndex.value < navigationHistory.value.length - 1)
const canGoUp = computed(() => {
  if (!currentPath.value) return false
  const parts = currentPath.value.split(/[/\\]/)
  return parts.length > 1
})

const totalItems = computed(() => allItems.value.length)
const backendTotalSize = ref(0)
const totalSize = computed(() => backendTotalSize.value)

const filteredItems = computed(() => {
  const keyword = searchKeyword.value.trim()
  if (!keyword) return presortedAllItems.value
  return applySmartFilter(presortedAllItems.value, keyword)
})

const filteredTotalItems = computed(() => filteredItems.value.length)

const displayItems = computed(() => {
  const start = (currentPage.value - 1) * pageSize.value
  const end = start + pageSize.value
  return filteredItems.value.slice(start, end)
})

const handleScan = async (path, addToHistory = true) => {
  if (!path || path.trim() === '') {
    message.warning('请输入有效的目录路径')
    return
  }

  loading.value = true
  scanTime.value = 0
  backendTime.value = 0
  streamedItemCount.value = 0
  backendTotalSize.value = 0

  allItems.value = []
  treeData.value = []

  if (unlistenScanBatch) {
    unlistenScanBatch()
  }
  unlistenScanBatch = await listen('scan-batch', (event) => {
    const batch = event.payload
    if (Array.isArray(batch) && batch.length > 0) {
      allItems.value.push(...batch)
      triggerRef(allItems)
      streamedItemCount.value = allItems.value.length
      for (let i = 0; i < batch.length; i++) {
        if (!batch[i].isDir) {
          backendTotalSize.value += batch[i].size || 0
        }
      }
    }
  })

  const fullStartTime = performance.now()

  try {
    const result = await invoke('scan_directory', {
      path: path.trim(),
      forceRefresh: false
    })

    backendTime.value = typeof result.scanTime === 'number' ? result.scanTime : 0

    allItems.value = result.items || []
    backendTotalSize.value = result.totalSize || 0
    presortedAllItems.value = sortWorker.sortItemsSync(result.items || [], sortConfig.value.column, sortConfig.value.direction)
    lastSortKey.value = `${sortConfig.value.column}-${sortConfig.value.direction}`

    currentPath.value = path
    lastSortKey.value = ''

    mftAvailable.value = result.mftAvailable || false

    if ('requestIdleCallback' in window) {
      requestIdleCallback(() => buildTreeData(), { timeout: 100 })
    } else {
      setTimeout(() => buildTreeData(), 50)
    }

    if (addToHistory) {
      navigationHistory.value = navigationHistory.value.slice(0, navigationIndex.value + 1)
      navigationHistory.value.push(path)
      navigationIndex.value = navigationHistory.value.length - 1
    }

    const fullEndTime = performance.now()
    scanTime.value = parseFloat(((fullEndTime - fullStartTime) / 1000).toFixed(2))

    try {
      await invoke('global_search_add_scan', { path: path.trim(), items: result.items })
    } catch {}

    message.success(`扫描完成 (总计: ${scanTime.value}s，找到 ${allItems.value.length} 个项目)`)
  } catch (error) {
    console.error('扫描失败:', error)
    message.error('扫描失败: ' + error)
  } finally {
    loading.value = false
    if (unlistenScanBatch) {
      unlistenScanBatch()
      unlistenScanBatch = null
    }
    streamedItemCount.value = 0
  }
}

const handleBrowse = async () => {
  try {
    const selected = await openDialog({
      title: '选择要扫描的目录',
      multiple: false,
      directory: true
    })
    if (selected) {
      await handleScan(selected)
    }
  } catch (error) {
    console.error('选择目录失败:', error)
    message.error('选择目录失败: ' + error)
  }
}

const handleNavigate = async (direction) => {
  if (direction === 'back' && canGoBack.value) {
    navigationIndex.value--
    const path = navigationHistory.value[navigationIndex.value]
    await handleScan(path, false)
  } else if (direction === 'forward' && canGoForward.value) {
    navigationIndex.value++
    const path = navigationHistory.value[navigationIndex.value]
    await handleScan(path, false)
  } else if (direction === 'up' && canGoUp.value) {
    const parentPath = getParentPath(currentPath.value)
    if (parentPath && parentPath !== currentPath.value) {
      await handleScan(parentPath)
    }
  }
}

const handleSearchInput = debounce((keyword) => {
  searchKeyword.value = keyword
  currentPage.value = 1
  lastSortKey.value = ''
}, 200)

const handleSelectPath = async (path) => {
  if (!path) return

  try {
    const isDir = await invoke('is_directory', { path })
    if (isDir) {
      await handleScan(path)
    } else {
      await invoke('open_path', { path })
    }
  } catch (error) {
    console.error('选择路径失败:', error)
    message.error('选择路径失败: ' + error)
  }
}

const handleQuickAccess = async (action) => {
  if (action === 'computer') {
    await handleBrowse()
    return
  }
  try {
    const home = await homeDir()
    if (!home) {
      message.warning('无法获取用户目录')
      return
    }
    let target = home
    if (action === 'downloads') {
      target = await join(home, 'Downloads')
    } else if (action === 'desktop') {
      target = await join(home, 'Desktop')
    }
    await handleScan(target)
  } catch (error) {
    console.error('快速访问失败:', error)
    message.error('快速访问失败: ' + error)
  }
}

const handleSelectItem = async (item) => {
  if (item.isDir) {
    await handleScan(item.path)
  } else {
    try {
      await invoke('open_path', { path: item.path })
    } catch (error) {
      console.error('打开文件失败:', error)
      message.error('打开文件失败: ' + error)
    }
  }
}

const handleSort = (column, direction) => {
  let newDirection = direction
  if (!newDirection) {
    newDirection = sortConfig.value.column === column
      ? (sortConfig.value.direction === 'asc' ? 'desc' : 'asc')
      : (column === 'name' ? 'asc' : 'desc')
  }
  sortConfig.value.column = column
  sortConfig.value.direction = newDirection
  lastSortKey.value = ''
  if (allItems.value.length > 0) {
    const newSortKey = `${sortConfig.value.column}-${sortConfig.value.direction}`
    if (newSortKey !== lastSortKey.value) {
      presortedAllItems.value = sortWorker.sortItemsSync(allItems.value, sortConfig.value.column, sortConfig.value.direction)
      lastSortKey.value = newSortKey
    }
  }
}

const handleSelectHistory = async (path) => {
  historyVisible.value = false
  await handleScan(path)
}

const handleClearHistory = async () => {
  try {
    await invoke('clear_history')
    history.value = []
    message.success('历史记录已清除')
  } catch (error) {
    message.error('清除历史记录失败: ' + error)
  }
}

const handlePageChange = (page) => {
  currentPage.value = page
}

const handleSizeChange = (current, size) => {
  pageSize.value = size
  currentPage.value = current
}

const buildTreeData = () => {
  const dirs = allItems.value.filter(item => item.isDir)
  if (dirs.length === 0) {
    treeData.value = []
    return
  }

  const nodeMap = new Map()

  for (const dir of dirs) {
    const pathParts = dir.path.split('/')
    const name = pathParts[pathParts.length - 1] || dir.path

    nodeMap.set(dir.path, {
      key: dir.path,
      title: name,
      size: dir.size,
      sizeFormatted: dir.sizeFormatted,
      isLeaf: true,
      children: []
    })
  }

  const topLevelNodes = []

  for (const [path, node] of nodeMap) {
    const lastSlashIndex = path.lastIndexOf('/')

    if (lastSlashIndex === -1 || lastSlashIndex === 0) {
      topLevelNodes.push(node)
    } else {
      const parentPath = path.substring(0, lastSlashIndex)
      const parentNode = nodeMap.get(parentPath)

      if (parentNode) {
        parentNode.isLeaf = false
        parentNode.children.push(node)
      } else {
        topLevelNodes.push(node)
      }
    }
  }

  const sortBySize = (a, b) => (b.size || 0) - (a.size || 0)

  const sortChildren = (nodes) => {
    nodes.sort(sortBySize)
    for (const node of nodes) {
      if (node.children && node.children.length > 0) {
        sortChildren(node.children)
      }
    }
  }

  sortChildren(topLevelNodes)
  treeData.value = topLevelNodes
}

const handleOpenDirFromSearch = (path) => {
  if (path) handleScan(path)
}

const fetchGlobalSearchStatus = async () => {
  try {
    globalSearchState.value = await invoke('global_search_status')
  } catch (error) {
    console.error('获取全局搜索状态失败:', error)
  }
}

const loadHistory = async () => {
  try {
    const historyData = await invoke('get_history_summary')
    history.value = historyData || []
  } catch (error) {
    console.error('加载历史记录失败:', error)
  }
}

const onGlobalSearchKeydown = (e) => {
  if ((e.ctrlKey || e.metaKey) && (e.key === 'k' || e.key === 'K')) {
    e.preventDefault()
    globalSearchVisible.value = true
  }
}

onMounted(async () => {
  loadHistory()
  document.addEventListener('keydown', onGlobalSearchKeydown)

  unlistenGlobalSearchProgress = await listen('global-search-progress', (event) => {
    globalSearchProgress.value = event.payload
    if (event.payload?.phase === 'done') {
      fetchGlobalSearchStatus()
    } else {
      globalSearchState.value = {
        kind: 'loading',
        data: { drive: event.payload?.drive || '', scanned: event.payload?.scanned || 0 }
      }
    }
  })

  try {
    isAdmin.value = await invoke('is_admin')
  } catch {
    isAdmin.value = false
  }

  await fetchGlobalSearchStatus()
})

onUnmounted(() => {
  if (unlistenScanBatch) {
    unlistenScanBatch()
    unlistenScanBatch = null
  }
  if (unlistenGlobalSearchProgress) {
    unlistenGlobalSearchProgress()
    unlistenGlobalSearchProgress = null
  }
  document.removeEventListener('keydown', onGlobalSearchKeydown)
})

watch(historyVisible, (isOpen) => {
  if (isOpen) {
    loadHistory()
  }
})

watch(() => allItems.value.length, () => {
  currentPage.value = 1
  lastSortKey.value = ''
})
</script>

<style scoped>
.fd-app {
  display: grid;
  grid-template-rows: 38px 1fr 24px;
  grid-template-columns: 220px 1fr 300px;
  height: 100vh;
  background: var(--fd-bg-0);
  color: var(--fd-text-1);
}
.fd-app.fd-sidebar-collapsed {
  grid-template-columns: 0px 1fr 300px;
}
.fd-main {
  grid-row: 2 / 3;
  grid-column: 2 / 3;
  display: flex;
  flex-direction: column;
  background: var(--fd-bg-0);
  min-width: 0;
  overflow: hidden;
}
</style>

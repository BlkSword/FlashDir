<template>
  <div class="fd-app" :class="{ 'fd-sidebar-collapsed': sidebarCollapsed }">
    <Toolbar
      ref="toolbarRef"
      :path="currentPath"
      :can-go-back="canGoBack"
      :can-go-forward="canGoForward"
      :can-go-up="canGoUp"
      :loading="loading"
      :watching="watching"
      @scan="handleScan"
      @cancel-scan="handleCancelScan"
      @browse="handleBrowse"
      @navigate="handleNavigate"
      @toggle-watch="handleToggleWatch"
      @show-history="historyVisible = true"
      @show-about="aboutVisible = true"
      @show-diagnostics="openDiagnostics"
      @open-dir="handleOpenDirFromSearch"
      @toggle-sidebar="sidebarCollapsed = !sidebarCollapsed"
    />

    <Sidebar
      :tree-data="treeData"
      :selected-path="currentPath"
      :history="history"
      :collapsed="sidebarCollapsed"
      :tree-key="treeKey"
      @select="handleSelectPath"
      @quick-access="handleQuickAccess"
      @load-children="handleLoadTreeChildren"
      @collapse-tree="handleCollapseTree"
    />

    <main class="fd-main">
      <FileList
        :items="pageItems"
        :loading="loading"
        :total-size="totalSize"
        :current-path="currentPath"
        :sort-config="sortConfig"
        :current-page="currentPage"
        :page-size="pageSize"
        :total-items="totalItems"
        @sort="handleSort"
        @select="handleSelectItem"
        @page-change="handlePageChange"
        @size-change="handleSizeChange"
        @filter="handleSearchInput"
      />
    </main>

    <RightPanel
      :items="pageItems"
      :total-size="totalSize"
      :current-path="currentPath"
      :active-tab="rightPanelTab"
      :scan-time="scanTime"
      :file-count="fileCount"
      :dir-count="dirCount"
      :top-files="topFiles"
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
      :scan-phase="scanPhase"
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

    <a-modal
      :open="diagnosticsVisible"
      title="运行诊断"
      width="760px"
      :footer="null"
      @cancel="diagnosticsVisible = false"
    >
      <pre class="diagnostics-pre">{{ diagnosticsText || '正在获取诊断信息…' }}</pre>
    </a-modal>

    <a-modal
      :open="aboutVisible"
      title="关于 FlashDir"
      width="520px"
      :footer="null"
      @cancel="aboutVisible = false"
    >
      <div class="about-card">
        <div class="about-logo">
          <svg fill="none" stroke="currentColor" viewBox="0 0 24 24" width="48" height="48"><path stroke-linecap="round" stroke-linejoin="round" stroke-width="1.6" d="M4 4h6l2 2h8a2 2 0 012 2v10a2 2 0 01-2 2H4a2 2 0 01-2-2V6a2 2 0 012-2z"/><path d="M2 10h20"/></svg>
        </div>
        <div class="about-title">FlashDir</div>
        <div class="about-subtitle">磁盘可观测性平台 · v3.4.2</div>
        <p class="about-desc">
          FlashDir 是一款面向 Windows 的磁盘空间分析与可观测性工具：MFT 直读扫描、
          USN 增量刷新、开发者目录分析、快照对比、重复文件检测与跨盘全局文件搜索。
        </p>
        <div class="about-meta">
          <span>本地优先 · 无遥测 · Apache-2.0</span>
          <a href="https://github.com/BlkSword/FlashDir" target="_blank">GitHub</a>
        </div>
      </div>
    </a-modal>
  </div>
</template>

<script setup>
import { ref, computed, watch, onMounted, onUnmounted, shallowRef } from 'vue'
import { message } from 'ant-design-vue'
import { listen } from '@tauri-apps/api/event'
import Toolbar from './components/Toolbar.vue'
import Sidebar from './components/Sidebar.vue'
import FileList from './components/FileList.vue'
import RightPanel from './components/RightPanel.vue'
import StatusBar from './components/StatusBar.vue'
import HistoryList from './components/HistoryList.vue'
import { useTauri } from './composables/useTauri'
import { useGlobalSearch } from './composables/useGlobalSearch'
import { debounce, getParentPath, formatError } from './utils/format.js'
import { homeDir, join } from '@tauri-apps/api/path'

const { invoke, openDialog } = useTauri()

const scanPhase = ref({ phase: '', message: '' })
let unlistenScanPhase = null
let unlistenDirChanges = null

const currentPath = ref('')
const pageItems = shallowRef([])
const totalItems = ref(0)
const totalSize = ref(0)
const fileCount = ref(0)
const dirCount = ref(0)
const topFiles = ref([])
const loading = ref(false)
const scanTime = ref(0)
const backendTime = ref(0)
const treeData = shallowRef([])
const treeKey = ref(0)
const history = shallowRef([])
const mftAvailable = ref(false)
const isAdmin = ref(false)

const navigationHistory = ref([])
const navigationIndex = ref(-1)

const currentPage = ref(1)
const pageSize = ref(100)
const sortConfig = ref({ column: 'size', direction: 'desc' })
const searchKeyword = ref('')

const historyVisible = ref(false)
const aboutVisible = ref(false)
const diagnosticsVisible = ref(false)
const diagnosticsText = ref('')
const watching = ref(false)
const toolbarRef = ref(null)
const rightPanelTab = ref('stats')
const sidebarCollapsed = ref(false)

const { loading: globalSearchLoading, failed: globalSearchFailed, statusText: globalSearchStatusText } = useGlobalSearch()

const canGoBack = computed(() => navigationIndex.value > 0)
const canGoForward = computed(() => navigationIndex.value < navigationHistory.value.length - 1)
const canGoUp = computed(() => {
  if (!currentPath.value) return false
  const parts = currentPath.value.split(/[/\\]/)
  return parts.length > 1
})

const loadPage = async () => {
  if (!currentPath.value) return
  loading.value = true
  try {
    const result = await invoke('scan_directory_paged', {
      path: currentPath.value,
      forceRefresh: false,
      page: currentPage.value,
      pageSize: pageSize.value,
      sortColumn: sortConfig.value.column,
      sortDirection: sortConfig.value.direction,
      filter: searchKeyword.value.trim(),
    })

    pageItems.value = result.items || []
    totalItems.value = result.totalItems || 0
    totalSize.value = result.totalSize || 0
    fileCount.value = result.fileCount || 0
    dirCount.value = result.dirCount || 0
    topFiles.value = result.topFiles || []
    backendTime.value = typeof result.scanTime === 'number' ? result.scanTime : 0
    mftAvailable.value = !!result.mftAvailable
  } catch (error) {
    console.error('加载分页失败:', error)
    message.error('加载分页失败: ' + formatError(error))
  } finally {
    loading.value = false
  }
}

const buildTreeData = async () => {
  if (!currentPath.value) {
    treeData.value = []
    return
  }
  try {
    const children = await invoke('get_dir_children', { path: currentPath.value })
    treeData.value = (children || []).map(item => ({
      key: item.path,
      title: item.name,
      size: item.size,
      sizeFormatted: item.sizeFormatted,
      isLeaf: false,
      loaded: false,
      children: [],
    }))
  } catch (error) {
    console.error('加载目录树失败:', error)
    treeData.value = []
  }
}

const handleCollapseTree = () => {
  treeData.value = treeData.value.map(node => ({ ...node, loaded: false, children: [] }))
  treeKey.value += 1
}

const handleLoadTreeChildren = async (node) => {
  try {
    const children = await invoke('get_dir_children', { path: node.key })
    const childNodes = (children || []).map(item => ({
      key: item.path,
      title: item.name,
      size: item.size,
      sizeFormatted: item.sizeFormatted,
      isLeaf: false,
      loaded: false,
      children: [],
    }))
    node.children = childNodes
    node.loaded = true
    treeData.value = JSON.parse(JSON.stringify(treeData.value))
  } catch (error) {
    console.error('展开目录失败:', error)
  }
}

const handleScan = async (path, addToHistory = true) => {
  if (!path || path.trim() === '') {
    message.warning('请输入有效的目录路径')
    return
  }

  loading.value = true
  scanTime.value = 0
  backendTime.value = 0
  totalSize.value = 0
  pageItems.value = []
  treeData.value = []
  currentPage.value = 1
  scanPhase.value = { phase: '', message: '' }

  const fullStartTime = performance.now()

  try {
    currentPath.value = path

    const result = await invoke('scan_directory_paged', {
      path: path.trim(),
      forceRefresh: false,
      page: 1,
      pageSize: pageSize.value,
      sortColumn: sortConfig.value.column,
      sortDirection: sortConfig.value.direction,
      filter: searchKeyword.value.trim(),
    })

    pageItems.value = result.items || []
    totalItems.value = result.totalItems || 0
    totalSize.value = result.totalSize || 0
    fileCount.value = result.fileCount || 0
    dirCount.value = result.dirCount || 0
    topFiles.value = result.topFiles || []
    backendTime.value = typeof result.scanTime === 'number' ? result.scanTime : 0
    mftAvailable.value = !!result.mftAvailable

    const fullEndTime = performance.now()
    scanTime.value = parseFloat(((fullEndTime - fullStartTime) / 1000).toFixed(2))

    await buildTreeData()

    if (addToHistory) {
      navigationHistory.value = navigationHistory.value.slice(0, navigationIndex.value + 1)
      navigationHistory.value.push(path)
      navigationIndex.value = navigationHistory.value.length - 1
    }

    // 全局索引追加放到后台，不阻塞 UI
    invoke('global_search_add_scan_from_cache', { path: path.trim() }).catch(() => {})

    message.success(`扫描完成 (总计: ${scanTime.value}s，找到 ${totalItems.value.toLocaleString()} 个项目)`)
  } catch (error) {
    console.error('扫描失败:', error)
    message.error('扫描失败: ' + formatError(error))
  } finally {
    loading.value = false
    scanPhase.value = { phase: '', message: '' }
  }
}

const handleSearchInput = debounce((keyword) => {
  searchKeyword.value = keyword
  currentPage.value = 1
  loadPage()
}, 250)

const handleSort = (column, direction) => {
  let newDirection = direction
  if (!newDirection) {
    newDirection = sortConfig.value.column === column
      ? (sortConfig.value.direction === 'asc' ? 'desc' : 'asc')
      : (column === 'name' ? 'asc' : 'desc')
  }
  sortConfig.value.column = column
  sortConfig.value.direction = newDirection
  currentPage.value = 1
  loadPage()
}

const handlePageChange = (page) => {
  currentPage.value = page
  loadPage()
}

const handleSizeChange = (current, size) => {
  pageSize.value = size
  currentPage.value = current
  loadPage()
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

const handleBrowse = async () => {
  try {
    const selected = await openDialog({ title: '选择要扫描的目录', multiple: false, directory: true })
    if (selected) await handleScan(selected)
  } catch (error) {
    message.error('选择目录失败: ' + formatError(error))
  }
}

const handleSelectPath = async (path) => {
  if (!path) return
  try {
    const isDir = await invoke('is_directory', { path })
    if (isDir) await handleScan(path)
    else await invoke('open_path', { path })
  } catch (error) {
    message.error('选择路径失败: ' + formatError(error))
  }
}

const handleSelectItem = async (item) => {
  if (!item) return
  if (item.isDir) await handleScan(item.path)
  else {
    try { await invoke('open_path', { path: item.path }) } catch (error) { message.error('打开文件失败: ' + formatError(error)) }
  }
}

const handleQuickAccess = async (action) => {
  if (action === 'computer') { await handleBrowse(); return }
  try {
    const home = await homeDir()
    let target = home
    if (action === 'downloads') target = await join(home, 'Downloads')
    else if (action === 'desktop') target = await join(home, 'Desktop')
    await handleScan(target)
  } catch (error) { message.error('快速访问失败: ' + formatError(error)) }
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
  } catch (error) { message.error('清除历史记录失败: ' + formatError(error)) }
}

const handleOpenDirFromSearch = (path) => {
  if (path) handleScan(path)
}

const loadHistory = async () => {
  try {
    history.value = await invoke('get_history_summary') || []
  } catch (error) { console.error('加载历史记录失败:', error) }
}

const openDiagnostics = async () => {
  diagnosticsVisible.value = true
  diagnosticsText.value = ''
  try {
    diagnosticsText.value = JSON.stringify(await invoke('get_diagnostics'), null, 2)
  } catch (error) {
    diagnosticsText.value = '获取诊断信息失败: ' + formatError(error)
  }
}

const handleCancelScan = async () => {
  try { await invoke('cancel_scan'); message.info('正在取消扫描…') } catch (error) { console.error('取消失败:', error) }
}

const handleToggleWatch = async () => {
  if (watching.value) {
    try { await invoke('stop_watch'); watching.value = false; message.info('已停止目录监听') } catch (error) { message.error('停止监听失败: ' + formatError(error)) }
    return
  }
  if (!currentPath.value) { message.warning('请先扫描一个目录'); return }
  try { await invoke('start_watch', { path: currentPath.value }); watching.value = true; message.success('开始监听目录变更') } catch (error) { message.error('启动监听失败: ' + formatError(error)) }
}

const onGlobalSearchKeydown = (e) => {
  if ((e.ctrlKey || e.metaKey) && (e.key === 'k' || e.key === 'K')) {
    e.preventDefault()
    toolbarRef.value?.focusGlobalSearch?.()
  }
}

onMounted(async () => {
  loadHistory()
  document.addEventListener('keydown', onGlobalSearchKeydown)

  unlistenScanPhase = await listen('scan-phase', (event) => {
    scanPhase.value = event.payload || { phase: '', message: '' }
  })

  unlistenDirChanges = await listen('dir-changes', (event) => {
    const payload = event.payload || {}
    if (payload.changed > 0) {
      message.info(`目录变更：+${payload.added} -${payload.removed} ~${payload.modified}`)
    }
  })

  try { isAdmin.value = await invoke('is_admin') } catch { isAdmin.value = false }
})

onUnmounted(() => {
  if (unlistenScanPhase) { unlistenScanPhase(); unlistenScanPhase = null }
  if (unlistenDirChanges) { unlistenDirChanges(); unlistenDirChanges = null }
  document.removeEventListener('keydown', onGlobalSearchKeydown)
})

watch(historyVisible, (isOpen) => { if (isOpen) loadHistory() })
</script>

<style scoped>
.fd-app {
  display: grid;
  grid-template-rows: 56px 1fr 28px;
  grid-template-columns: 240px 1fr 360px;
  height: 100vh;
  background: var(--fd-bg-0);
  color: var(--fd-text-1);
}
.fd-app.fd-sidebar-collapsed {
  grid-template-columns: 0px 1fr 360px;
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
.diagnostics-pre {
  max-height: 60vh;
  overflow: auto;
  background: var(--fd-bg-0);
  color: var(--fd-text-0);
  border: 1px solid var(--fd-border);
  border-radius: 6px;
  padding: 12px;
  font-size: 12px;
  font-family: Consolas, 'JetBrains Mono', monospace;
  white-space: pre-wrap;
  word-break: break-all;
}
.about-card {
  text-align: center;
  padding: 12px 8px;
}
.about-logo {
  width: 64px;
  height: 64px;
  margin: 0 auto 12px;
  border-radius: 16px;
  background: var(--fd-accent);
  color: #fff;
  display: grid;
  place-items: center;
}
.about-title {
  font-size: 20px;
  font-weight: 700;
  color: var(--fd-text-0);
}
.about-subtitle {
  font-size: 12px;
  color: var(--fd-text-2);
  margin: 4px 0 12px;
}
.about-desc {
  font-size: 13px;
  color: var(--fd-text-1);
  line-height: 1.7;
  max-width: 420px;
  margin: 0 auto 16px;
}
.about-meta {
  display: flex;
  justify-content: center;
  gap: 16px;
  font-size: 12px;
  color: var(--fd-text-2);
}
.about-meta a {
  color: var(--fd-accent);
  text-decoration: none;
}
</style>
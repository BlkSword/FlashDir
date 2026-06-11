<template>
  <div class="app-container">
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
      @search="handleSearchInput"
    />

    <div class="main-container">
      <Sidebar
        :tree-data="treeData"
        :selected-path="currentPath"
        @select="handleSelectPath"
      />

      <div class="content-area">
        <FileList
          :items="displayItems"
          :loading="loading || sortWorker.isProcessing.value"
          :sort-config="sortConfig"
          @sort="handleSort"
          @select="handleSelectItem"
        />

        <div class="pagination-wrapper">
          <a-pagination
            :current="currentPage"
            :page-size="pageSize"
            :total="filteredTotalItems"
            :show-size-changer="true"
            :show-quick-jumper="true"
            :page-size-options="['50', '100', '200', '500', '1000']"
            size="small"
            show-total
            @change="handlePageChange"
            @showSizeChange="handleSizeChange"
          />
        </div>
      </div>

      <div class="right-panel">
        <a-tabs v-model:activeKey="rightPanelTab" size="small" class="panel-tabs">
          <a-tab-pane key="charts" tab="📊 统计">
            <Charts
              :items="allItems"
              :total-size="totalSize"
            />
          </a-tab-pane>
          <a-tab-pane key="dev" tab="🛠️ 开发者">
            <DevAnalyzer
              :items="allItems"
              :total-size="totalSize"
            />
          </a-tab-pane>
          <a-tab-pane key="snapshots" tab="📸 快照">
            <SnapshotCompare
              :items="allItems"
              :total-size="totalSize"
              :current-path="currentPath"
            />
          </a-tab-pane>
          <a-tab-pane key="treemap" tab="🗺️ 热图">
            <Treemap
              :items="allItems"
              :total-size="totalSize"
            />
          </a-tab-pane>
        </a-tabs>
      </div>
    </div>

    <StatusBar
      :path="currentPath"
      :total-items="totalItems"
      :total-size="totalSize"
      :scan-time="scanTime"
      :backend-time="backendTime"
    />

    <a-modal
      :visible="historyVisible"
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
  </div>
</template>

<script setup>
import { ref, computed, watch, onMounted, onUnmounted, shallowRef, triggerRef } from 'vue'
import { message } from 'ant-design-vue'
import { listen } from '@tauri-apps/api/event'
import Toolbar from './components/Toolbar.vue'
import Sidebar from './components/Sidebar.vue'
import FileList from './components/FileList.vue'
import Charts from './components/Charts.vue'
import DevAnalyzer from './components/DevAnalyzer.vue'
import SnapshotCompare from './components/SnapshotCompare.vue'
import Treemap from './components/Treemap.vue'
import StatusBar from './components/StatusBar.vue'
import HistoryList from './components/HistoryList.vue'
import { useTauri } from './composables/useTauri'
import { useSortWorker } from './composables/useSortWorker'
import { debounce, getParentPath } from './utils/format.js'
import { applySmartFilter, getFilterHints } from './utils/smartFilter.js'

const { invoke, openDialog } = useTauri()
const sortWorker = useSortWorker()

// 渐进式流式传输：scan-batch 事件监听器
let unlistenScanBatch = null
const streamedItemCount = ref(0)

const currentPath = ref('')
const allItems = shallowRef([])
const loading = ref(false)
const scanTime = ref(0)
const backendTime = ref(0)
const treeData = shallowRef([])
const history = shallowRef([])

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
const rightPanelTab = ref('charts')

const sortedItemsCache = shallowRef([])
const lastSortKey = ref('')

const canGoBack = computed(() => navigationIndex.value > 0)
const canGoForward = computed(() => navigationIndex.value < navigationHistory.value.length - 1)
const canGoUp = computed(() => {
  if (!currentPath.value) return false
  const parts = currentPath.value.split(/[/\\]/)
  return parts.length > 1
})

const totalItems = computed(() => allItems.value.length)

const totalSize = computed(() => {
  let sum = 0
  const items = allItems.value
  for (let i = 0; i < items.length; i++) {
    const item = items[i]
    if (!item.isDir) {
      sum += item.size || 0
    }
  }
  return sum
})

const filteredItems = computed(() => {
  const keyword = searchKeyword.value.trim()
  if (!keyword) return allItems.value
  // Everything-style smart filter: supports ext:zip size:>100MB type:dir etc.
  return applySmartFilter(allItems.value, keyword)
})

const filteredTotalItems = computed(() => filteredItems.value.length)

const displayItems = computed(() => {
  const items = filteredItems.value
  const sortColumn = sortConfig.value.column
  const sortDirection = sortConfig.value.direction

  const newSortKey = `${sortColumn}-${sortDirection}-${items.length}`

  let sorted
  if (newSortKey !== lastSortKey.value) {
    sorted = sortWorker.sortItemsSync(items, sortColumn, sortDirection)
    lastSortKey.value = newSortKey
    sortedItemsCache.value = sorted
  } else {
    sorted = sortedItemsCache.value
  }

  const start = (currentPage.value - 1) * pageSize.value
  const end = start + pageSize.value
  return sorted.slice(start, end)
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

  // 清除旧数据
  allItems.value = []
  treeData.value = []

  // 注册流式事件监听器（在发起扫描前）
  if (unlistenScanBatch) {
    unlistenScanBatch()
  }
  unlistenScanBatch = await listen('scan-batch', (event) => {
    const batch = event.payload
    if (Array.isArray(batch) && batch.length > 0) {
      // 使用 push + triggerRef 避免 O(n²) 的数组展开拷贝
      allItems.value.push(...batch)
      triggerRef(allItems)
      streamedItemCount.value = allItems.value.length
    }
  })

  const fullStartTime = performance.now()

  try {
    const result = await invoke('scan_directory', {
      path: path.trim(),
      forceRefresh: false
    })

    backendTime.value = typeof result.scanTime === 'number' ? result.scanTime : 0

    // 始终以最终结果为权威（流式数据仅作预览，最终结果保证一致性和排序）
    allItems.value = result.items || []

    currentPath.value = path
    lastSortKey.value = ''

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

    message.success(`扫描完成 (总计: ${scanTime.value}s，找到 ${allItems.value.length} 个项目)`)
  } catch (error) {
    console.error('扫描失败:', error)
    message.error('扫描失败: ' + error)
  } finally {
    loading.value = false
    // 清理事件监听器
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

const handleSelectPath = (path) => {
  handleScan(path)
}

const handleSelectItem = (item) => {
  if (item.isDir) {
    const fullPath = currentPath.value + '/' + item.path
    handleScan(fullPath)
  } else {
    message.info(`文件: ${item.name}`)
  }
}

const handleSort = (column) => {
  if (sortConfig.value.column === column) {
    sortConfig.value.direction = sortConfig.value.direction === 'asc' ? 'desc' : 'asc'
  } else {
    sortConfig.value.column = column
    sortConfig.value.direction = column === 'name' ? 'asc' : 'desc'
  }
  lastSortKey.value = ''
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

const loadHistory = async () => {
  try {
    const historyData = await invoke('get_history_summary')
    history.value = historyData || []
  } catch (error) {
    console.error('加载历史记录失败:', error)
  }
}

onMounted(() => {
  loadHistory()
})

onUnmounted(() => {
  if (unlistenScanBatch) {
    unlistenScanBatch()
    unlistenScanBatch = null
  }
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
.app-container {
  display: flex;
  flex-direction: column;
  height: 100vh;
  overflow: hidden;
  contain: strict;
}

.main-container {
  display: flex;
  flex: 1;
  overflow: hidden;
  contain: content;
}

.content-area {
  flex: 1;
  display: flex;
  flex-direction: column;
  overflow: hidden;
  padding-left: 16px;
  contain: content;
}

.pagination-wrapper {
  padding: 8px 16px;
  background: white;
  border-top: 1px solid #f0f0f0;
  display: flex;
  justify-content: flex-end;
  align-items: center;
  contain: content;
}

.right-panel {
  width: 320px;
  display: flex;
  flex-direction: column;
  border-left: 1px solid #f0f0f0;
  background: #fafafa;
  overflow: hidden;
}

.panel-tabs {
  height: 100%;
  display: flex;
  flex-direction: column;
}

.panel-tabs :deep(.ant-tabs-nav) {
  margin: 0;
  padding: 0 12px;
  background: white;
  border-bottom: 1px solid #f0f0f0;
}

.panel-tabs :deep(.ant-tabs-content-holder) {
  flex: 1;
  overflow: hidden;
}

.panel-tabs :deep(.ant-tabs-content) {
  height: 100%;
}

.panel-tabs :deep(.ant-tabs-tabpane) {
  height: 100%;
  overflow-y: auto;
}
</style>

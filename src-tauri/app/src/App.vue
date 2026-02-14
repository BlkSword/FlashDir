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

      <Charts
        :items="allItems"
        :total-size="totalSize"
      />
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
import { ref, computed, watch, onMounted, shallowRef } from 'vue'
import { message } from 'ant-design-vue'
import Toolbar from './components/Toolbar.vue'
import Sidebar from './components/Sidebar.vue'
import FileList from './components/FileList.vue'
import Charts from './components/Charts.vue'
import StatusBar from './components/StatusBar.vue'
import HistoryList from './components/HistoryList.vue'
import { useTauri } from './composables/useTauri'
import { useSortWorker } from './composables/useSortWorker'
import { debounce, getParentPath } from './utils/format.js'

const { invoke, openDialog } = useTauri()
const sortWorker = useSortWorker()

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
  return sortWorker.filterItemsSync(allItems.value, keyword)
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

  const fullStartTime = performance.now()

  try {
    const result = await invoke('scan_directory', {
      path: path.trim(),
      forceRefresh: false
    })

    backendTime.value = typeof result.scanTime === 'number' ? result.scanTime : 0

    await new Promise(resolve => {
      requestAnimationFrame(() => {
        allItems.value = result.items || []
        currentPath.value = path
        lastSortKey.value = ''

        if ('requestIdleCallback' in window) {
          requestIdleCallback(() => buildTreeData(), { timeout: 100 })
        } else {
          setTimeout(() => buildTreeData(), 50)
        }

        resolve()
      })
    })

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
</style>

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
import { ref, computed, watch, onMounted, shallowRef, nextTick } from 'vue'
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

// 缓存排序结果
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

// 过滤计算
const filteredItems = computed(() => {
  const keyword = searchKeyword.value.trim()
  if (!keyword) return allItems.value

  const lowerKeyword = keyword.toLowerCase()
  return sortWorker.filterItemsSync(allItems.value, keyword)
})

const filteredTotalItems = computed(() => filteredItems.value.length)

// 显示项目计算 - 使用缓存
const displayItems = computed(() => {
  const items = filteredItems.value
  const sortColumn = sortConfig.value.column
  const sortDirection = sortConfig.value.direction

  const newSortKey = `${sortColumn}-${sortDirection}-${items.length}`

  let sorted
  if (newSortKey !== lastSortKey.value) {
    // 使用 Worker 进行排序（大数据量）或同步排序（小数据量）
    if (items.length > 1000) {
      // 异步排序将在 watch 中处理
      sorted = sortWorker.sortItemsSync(items, sortColumn, sortDirection)
    } else {
      sorted = sortWorker.sortItemsSync(items, sortColumn, sortDirection)
    }
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

    // 使用 requestIdleCallback 或 setTimeout 分片处理
    await new Promise(resolve => {
      requestAnimationFrame(() => {
        allItems.value = result.items || []
        currentPath.value = path
        lastSortKey.value = '' // 重置排序缓存

        // 使用 requestIdleCallback 延迟构建树
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

    const timingInfo = result.timing
    if (timingInfo) {
      const safeNum = (n) => typeof n === 'number' ? n.toFixed(2) + 's' : 'N/A'
      console.log('性能详情:', {
        扫描阶段: safeNum(timingInfo.scan_phase),
        统计阶段: safeNum(timingInfo.compute_phase),
        格式化阶段: safeNum(timingInfo.format_phase),
        后端总计: safeNum(timingInfo.total)
      })
    }

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
  lastSortKey.value = '' // 重置排序缓存
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
  lastSortKey.value = '' // 重置排序缓存
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

// 优化的树数据构建 - 懒加载模式
const buildTreeData = () => {
  const dirs = allItems.value.filter(item => item.isDir)
  if (dirs.length === 0) {
    treeData.value = []
    return
  }

  // 只构建顶层目录，子目录按需加载
  const topLevelNodes = []
  const nodeMap = new Map()

  // 分批处理，避免阻塞主线程
  const batchSize = 500
  let processedCount = 0

  const processBatch = () => {
    const endIndex = Math.min(processedCount + batchSize, dirs.length)

    for (let i = processedCount; i < endIndex; i++) {
      const dir = dirs[i]
      const pathParts = dir.path.split('/')

      const node = {
        key: dir.path,
        title: pathParts[pathParts.length - 1] || dir.path,
        size: dir.size,
        sizeFormatted: dir.sizeFormatted,
        isLeaf: true // 初始标记为叶子节点
      }

      nodeMap.set(dir.path, node)
    }

    processedCount = endIndex

    if (processedCount < dirs.length) {
      // 继续处理下一批
      if ('requestIdleCallback' in window) {
        requestIdleCallback(processBatch, { timeout: 50 })
      } else {
        setTimeout(processBatch, 0)
      }
    } else {
      // 所有节点创建完成，构建顶层
      finalizeTree(nodeMap, topLevelNodes)
    }
  }

  const finalizeTree = (nodeMap, topLevelNodes) => {
    for (const [path, node] of nodeMap) {
      const lastSlashIndex = path.lastIndexOf('/')

      if (lastSlashIndex === -1) {
        topLevelNodes.push(node)
      } else {
        const parentPath = path.substring(0, lastSlashIndex)
        const parentNode = nodeMap.get(parentPath)

        if (parentNode) {
          parentNode.isLeaf = false
        } else {
          topLevelNodes.push(node)
        }
      }
    }

    // 对顶层节点排序
    topLevelNodes.sort((a, b) => (b.size || 0) - (a.size || 0))
    treeData.value = topLevelNodes
  }

  // 开始分批处理
  processBatch()
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

// 当数据变化时重置页码
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

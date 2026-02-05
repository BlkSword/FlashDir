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
          :loading="loading"
          :sort-config="sortConfig"
          @sort="handleSort"
          @select="handleSelectItem"
        />

        <div class="pagination-wrapper">
          <a-pagination
            v-model:current="currentPage"
            v-model:page-size="pageSize"
            :total="filteredTotalItems"
            :show-size-changer="true"
            :show-quick-jumper="true"
            :page-size-options="['50', '100', '200', '500', '1000']"
            size="small"
            show-total
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
      v-model:open="historyVisible"
      title="历史记录"
      width="800px"
      :footer="null"
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
import { ref, computed, watch, onMounted } from 'vue'
import { message } from 'ant-design-vue'
import Toolbar from './components/Toolbar.vue'
import Sidebar from './components/Sidebar.vue'
import FileList from './components/FileList.vue'
import Charts from './components/Charts.vue'
import StatusBar from './components/StatusBar.vue'
import HistoryList from './components/HistoryList.vue'
import { useTauri } from './composables/useTauri'
import { debounce, getParentPath } from './utils/format.js'

const { invoke, openDialog } = useTauri()

const currentPath = ref('')
const allItems = ref([])
const loading = ref(false)
const scanTime = ref(0)  // 总耗时（秒）
const backendTime = ref(0)  // 后端扫描耗时（秒）
const treeData = ref([])
const history = ref([])

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

const canGoBack = computed(() => navigationIndex.value > 0)
const canGoForward = computed(() => navigationIndex.value < navigationHistory.value.length - 1)
const canGoUp = computed(() => {
  if (!currentPath.value) return false
  const parts = currentPath.value.split(/[/\\]/)
  return parts.length > 1
})

const totalItems = computed(() => allItems.value.length)

const totalSize = computed(() => {
  return allItems.value
    .filter(item => !item.isDir)
    .reduce((sum, item) => sum + (item.size || 0), 0)
})

const displayItems = ref([])
const filteredItems = ref([])

const updateDisplayItems = () => {
  // 先过滤
  let items = allItems.value
  if (searchKeyword.value.trim()) {
    const keyword = searchKeyword.value.toLowerCase().trim()
    items = items.filter(item => {
      return item.name.toLowerCase().includes(keyword) ||
             item.path.toLowerCase().includes(keyword)
    })
  }
  filteredItems.value = items

  // 再排序
  const sortColumn = sortConfig.value.column
  const sortDirection = sortConfig.value.direction

  const sorted = [...items].sort((a, b) => {
    let aVal, bVal

    switch (sortColumn) {
      case 'name':
        aVal = a.name || a.path
        bVal = b.name || b.path
        return sortDirection === 'asc'
          ? aVal.localeCompare(bVal, 'zh-CN')
          : bVal.localeCompare(aVal, 'zh-CN')

      case 'type':
        aVal = a.isDir ? 0 : 1
        bVal = b.isDir ? 0 : 1
        if (aVal !== bVal) {
          return sortDirection === 'asc' ? aVal - bVal : bVal - aVal
        }
        aVal = a.name || a.path
        bVal = b.name || b.path
        return sortDirection === 'asc'
          ? aVal.localeCompare(bVal, 'zh-CN')
          : bVal.localeCompare(aVal, 'zh-CN')

      case 'size':
        aVal = a.size || 0
        bVal = b.size || 0
        return sortDirection === 'asc' ? aVal - bVal : bVal - aVal

      default:
        return 0
    }
  })

  // 分页
  const start = (currentPage.value - 1) * pageSize.value
  const end = start + pageSize.value
  displayItems.value = sorted.slice(start, end)
}

const debouncedUpdate = debounce(updateDisplayItems, 10)

watch([allItems, searchKeyword, sortConfig, currentPage, pageSize], debouncedUpdate)

const filteredTotalItems = computed(() => filteredItems.value.length)

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

    const backendEndTime = performance.now()
    backendTime.value = typeof result.scanTime === 'number' ? result.scanTime : 0

    await new Promise(resolve => {
      requestAnimationFrame(() => {
        allItems.value = result.items || []
        currentPath.value = path

        setTimeout(() => {
          buildTreeData()
        }, 50)

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
}, 300)

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

const buildTreeData = () => {
  const dirs = allItems.value.filter(item => item.isDir)
  if (dirs.length === 0) {
    treeData.value = []
    return
  }

  // 使用 Map 优化查找性能，从 O(n²) 降低到 O(n)
  const nodeMap = new Map()

  // 首先创建所有节点
  dirs.forEach(dir => {
    nodeMap.set(dir.path, {
      key: dir.path,
      title: dir.path.split('/').pop() || dir.path,
      size: dir.size,
      sizeFormatted: dir.sizeFormatted,
      children: []
    })
  })

  // 构建父子关系
  const topLevelNodes = []

  dirs.forEach(dir => {
    const node = nodeMap.get(dir.path)
    const lastSlashIndex = dir.path.lastIndexOf('/')

    if (lastSlashIndex === -1) {
      // 顶级目录
      topLevelNodes.push(node)
    } else {
      const parentPath = dir.path.substring(0, lastSlashIndex)
      const parentNode = nodeMap.get(parentPath)

      if (parentNode) {
        parentNode.children.push(node)
      } else {
        // 父节点不存在，作为顶级节点
        topLevelNodes.push(node)
      }
    }
  })

  // 移除空的 children 数组以优化渲染
  const cleanEmptyChildren = (nodes) => {
    nodes.forEach(node => {
      if (node.children && node.children.length === 0) {
        delete node.children
      } else if (node.children) {
        cleanEmptyChildren(node.children)
      }
    })
  }

  cleanEmptyChildren(topLevelNodes)
  treeData.value = topLevelNodes
}

const loadHistory = async () => {
  try {
    // 使用轻量级摘要，不加载完整的 items 列表
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
})
</script>

<style scoped>
.app-container {
  display: flex;
  flex-direction: column;
  height: 100vh;
  overflow: hidden;
}

.main-container {
  display: flex;
  flex: 1;
  overflow: hidden;
}

.content-area {
  flex: 1;
  display: flex;
  flex-direction: column;
  overflow: hidden;
  padding-left: 16px;
}

.pagination-wrapper {
  padding: 8px 16px;
  background: white;
  border-top: 1px solid #f0f0f0;
  display: flex;
  justify-content: flex-end;
  align-items: center;
}
</style>

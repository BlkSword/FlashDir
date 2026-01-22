<template>
  <div class="app-container">
    <!-- 工具栏 -->
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
    />

    <!-- 主内容区 -->
    <div class="main-container">
      <!-- 侧边栏 -->
      <Sidebar
        :tree-data="treeData"
        :selected-path="currentPath"
        @select="handleSelectPath"
      />

      <!-- 内容区域 -->
      <div class="content-area">
        <FileList
          :items="displayItems"
          :loading="loading"
          :sort-config="sortConfig"
          @sort="handleSort"
          @select="handleSelectItem"
        />

        <!-- 分页 -->
        <div class="pagination-wrapper">
          <a-pagination
            v-model:current="currentPage"
            v-model:page-size="pageSize"
            :total="totalItems"
            :show-size-changer="true"
            :show-quick-jumper="true"
            :page-size-options="['50', '100', '200', '500', '1000']"
            size="small"
            show-total
          />
        </div>
      </div>

      <!-- 统计图表面板 -->
      <Charts
        :items="allItems"
        :total-size="totalSize"
      />
    </div>

    <!-- 状态栏 -->
    <StatusBar
      :path="currentPath"
      :total-items="totalItems"
      :total-size="totalSize"
      :scan-time="scanTime"
    />

    <!-- 历史记录模态框 -->
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

const { invoke, openDialog } = useTauri()

// ==================== 状态管理 ====================
const currentPath = ref('')
const allItems = ref([])
const loading = ref(false)
const scanTime = ref(0)
const treeData = ref([])
const history = ref([])

// 导航历史
const navigationHistory = ref([])
const navigationIndex = ref(-1)

// 分页
const currentPage = ref(1)
const pageSize = ref(100)

// 排序
const sortConfig = ref({
  column: 'size',
  direction: 'desc'
})

// 历史记录模态框
const historyVisible = ref(false)

// ==================== 计算属性 ====================
const canGoBack = computed(() => navigationIndex.value > 0)
const canGoForward = computed(() => navigationIndex.value < navigationHistory.value.length - 1)
const canGoUp = computed(() => {
  if (!currentPath.value) return false
  const parts = currentPath.value.split(/[/\\]/)
  return parts.length > 1
})

const totalItems = computed(() => allItems.value.length)

const totalSize = computed(() => {
  return allItems.value.reduce((sum, item) => sum + (item.size || 0), 0)
})

// 分页后的显示数据
const displayItems = computed(() => {
  let items = [...allItems.value]

  // 排序
  items.sort((a, b) => {
    let aVal, bVal

    if (sortConfig.value.column === 'name') {
      aVal = a.name || a.path
      bVal = b.name || b.path
      return sortConfig.value.direction === 'asc'
        ? aVal.localeCompare(bVal)
        : bVal.localeCompare(aVal)
    }

    if (sortConfig.value.column === 'type') {
      aVal = a.isDir ? 0 : 1
      bVal = b.isDir ? 0 : 1
      if (aVal !== bVal) {
        return sortConfig.value.direction === 'asc' ? aVal - bVal : bVal - aVal
      }
      aVal = a.name || a.path
      bVal = b.name || b.path
      return sortConfig.value.direction === 'asc'
        ? aVal.localeCompare(bVal)
        : bVal.localeCompare(aVal)
    }

    if (sortConfig.value.column === 'size') {
      aVal = a.size || 0
      bVal = b.size || 0
      return sortConfig.value.direction === 'asc' ? aVal - bVal : bVal - aVal
    }

    return 0
  })

  // 分页
  const start = (currentPage.value - 1) * pageSize.value
  const end = start + pageSize.value
  return items.slice(start, end)
})

// ==================== 方法 ====================
const handleScan = async (path, addToHistory = true) => {
  if (!path || path.trim() === '') {
    message.warning('请输入有效的目录路径')
    return
  }

  loading.value = true
  try {
    const result = await invoke('scan_directory', {
      path: path.trim(),
      forceRefresh: false
    })

    allItems.value = result.items || []
    scanTime.value = result.scanTime || 0
    currentPath.value = path

    // 更新导航历史
    if (addToHistory) {
      navigationHistory.value = navigationHistory.value.slice(0, navigationIndex.value + 1)
      navigationHistory.value.push(path)
      navigationIndex.value = navigationHistory.value.length - 1
    }

    // 构建树形数据
    buildTreeData()

    message.success(`扫描完成，找到 ${allItems.value.length} 个项目`)
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
    const parts = currentPath.value.split(/[/\\]/)
    parts.pop()
    const parentPath = parts.join('/')
    if (parentPath) {
      await handleScan(parentPath)
    }
  }
}

const handleSelectPath = (path) => {
  handleScan(path)
}

const handleSelectItem = (item) => {
  if (item.isDir) {
    const fullPath = currentPath.value + '/' + item.path
    handleScan(fullPath)
  } else {
    // 打开文件或显示文件信息
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

// 构建树形数据
const buildTreeData = () => {
  const dirs = allItems.value.filter(item => item.isDir)
  const tree = {}

  dirs.forEach(dir => {
    const parts = dir.path.split('/')
    let current = tree

    parts.forEach((part, index) => {
      if (!current[part]) {
        current[part] = {
          name: part,
          children: {},
          fullPath: dir.path,
          isLeaf: index === parts.length - 1
        }
      }
      current = current[part].children
    })
  })

  treeData.value = convertTreeToArray(tree, currentPath.value)
}

const convertTreeToArray = (tree, basePath = '') => {
  return Object.keys(tree).map(key => {
    const node = tree[key]
    const fullPath = basePath ? `${basePath}/${node.fullPath}` : node.fullPath

    return {
      key: fullPath,
      title: node.name,
      isLeaf: node.isLeaf,
      children: node.children && Object.keys(node.children).length > 0
        ? convertTreeToArray(node.children, fullPath)
        : undefined
    }
  })
}

// 加载历史记录
const loadHistory = async () => {
  try {
    const historyData = await invoke('get_history')
    history.value = historyData || []
  } catch (error) {
    console.error('加载历史记录失败:', error)
  }
}

// ==================== 生命周期 ====================
onMounted(() => {
  loadHistory()
})

// 监听历史记录模态框打开，重新加载最新数据
watch(historyVisible, (isOpen) => {
  if (isOpen) {
    loadHistory()
  }
})

// 监听分页变化，重置到第一页
watch(() => allItems.value, () => {
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

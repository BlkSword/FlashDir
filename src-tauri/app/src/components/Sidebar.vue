<template>
  <div class="sidebar">
    <div class="sidebar-header">文件夹</div>
    <div class="sidebar-content">
      <!-- 使用虚拟滚动优化大数据量树形组件 -->
      <a-tree
        v-model:selectedKeys="selectedKeys"
        :tree-data="treeData"
        :show-icon="true"
        :show-line="true"
        :height="treeHeight"
        :virtual="true"
        @select="handleSelect"
      >
        <template #icon="{ isLeaf }">
          <FolderOutlined v-if="!isLeaf" />
          <FileOutlined v-else />
        </template>
        <template #title="{ title, sizeFormatted }">
          <span class="tree-node-title">
            <span class="node-name">{{ title }}</span>
            <span v-if="sizeFormatted" class="node-size">{{ sizeFormatted }}</span>
          </span>
        </template>
      </a-tree>
    </div>
  </div>
</template>

<script setup>
import { FolderOutlined, FileOutlined } from '@ant-design/icons-vue'
import { ref, watch, onMounted, onUnmounted } from 'vue'

const props = defineProps({
  treeData: {
    type: Array,
    default: () => []
  },
  selectedPath: {
    type: String,
    default: ''
  }
})

const emit = defineEmits(['select'])

const selectedKeys = ref([])
const treeHeight = ref(600)

// 动态计算树形组件高度
const updateTreeHeight = () => {
  const sidebarContent = document.querySelector('.sidebar-content')
  if (sidebarContent) {
    treeHeight.value = sidebarContent.clientHeight
  }
}

const handleSelect = (keys) => {
  if (keys.length > 0) {
    emit('select', keys[0])
  }
}

// 监听选中路径变化
watch(() => props.selectedPath, (newVal) => {
  if (newVal) {
    selectedKeys.value = [newVal]
  } else {
    selectedKeys.value = []
  }
})

// 监听 treeData 变化，更新高度
watch(() => props.treeData, () => {
  setTimeout(updateTreeHeight, 100)
}, { flush: 'post' })

// 窗口大小变化时更新高度
let resizeObserver = null

onMounted(() => {
  updateTreeHeight()

  // 使用 ResizeObserver 监听容器大小变化
  if (window.ResizeObserver) {
    const sidebarContent = document.querySelector('.sidebar-content')
    if (sidebarContent) {
      resizeObserver = new ResizeObserver(() => {
        updateTreeHeight()
      })
      resizeObserver.observe(sidebarContent)
    }
  } else {
    // 降级方案
    window.addEventListener('resize', updateTreeHeight)
  }
})

onUnmounted(() => {
  if (resizeObserver) {
    resizeObserver.disconnect()
  } else {
    window.removeEventListener('resize', updateTreeHeight)
  }
})
</script>

<style scoped>
.sidebar {
  width: 300px;
  background: #fafafa;
  border-right: 1px solid #f0f0f0;
  overflow: hidden;
  display: flex;
  flex-direction: column;
  contain: strict;
}

.sidebar-header {
  padding: 8px 12px;
  font-size: 12px;
  font-weight: 600;
  color: #8c8c8c;
  text-transform: uppercase;
  border-bottom: 1px solid #f0f0f0;
  contain: content;
}

.sidebar-content {
  flex: 1;
  overflow: hidden;
  padding: 8px 0;
  contain: content;
  will-change: scroll-position;
}

/* 使用 :deep() 穿透 scoped 样式 */
:deep(.ant-tree-node-content-wrapper) {
  padding: 4px 8px;
  display: flex;
  align-items: center;
  flex: 1;
  min-width: 0;
}

:deep(.ant-tree-node-content-wrapper:hover) {
  background: #f5f5f5;
}

:deep(.ant-tree-node-selected .ant-tree-node-content-wrapper) {
  background: #e6f7ff;
}

:deep(.ant-tree-iconEle) {
  flex-shrink: 0;
  margin-right: 4px;
}

:deep(.ant-tree-title) {
  flex: 1;
  min-width: 0;
  padding: 0;
}

.tree-node-title {
  display: inline-flex;
  align-items: center;
  justify-content: space-between;
  gap: 8px;
  width: 100%;
  min-width: 0;
}

.node-name {
  flex: 1;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  min-width: 0;
}

.node-size {
  flex-shrink: 0;
  font-size: 11px;
  color: #999;
  font-family: 'Consolas', 'Monaco', monospace;
  text-align: right;
  min-width: 50px;
}

:deep(.ant-tree-node-selected .node-size) {
  color: #1890ff;
}
</style>

<template>
  <div class="sidebar">
    <div class="sidebar-header">文件夹</div>
    <div class="sidebar-content">
      <a-tree
        v-model:selectedKeys="selectedKeys"
        :tree-data="treeData"
        :show-icon="true"
        :show-line="true"
        @select="handleSelect"
      >
        <template #icon="{ isLeaf }">
          <FolderOutlined v-if="!isLeaf" />
          <FileOutlined v-else />
        </template>
      </a-tree>
    </div>
  </div>
</template>

<script setup>
import { ref, watch } from 'vue'
import { FolderOutlined, FileOutlined } from '@ant-design/icons-vue'

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

watch(() => props.selectedPath, (newVal) => {
  if (newVal) {
    selectedKeys.value = [newVal]
  } else {
    selectedKeys.value = []
  }
})

const handleSelect = (keys) => {
  if (keys.length > 0) {
    emit('select', keys[0])
  }
}
</script>

<style scoped>
.sidebar {
  width: 250px;
  background: #fafafa;
  border-right: 1px solid #f0f0f0;
  overflow: hidden;
  display: flex;
  flex-direction: column;
}

.sidebar-header {
  padding: 8px 12px;
  font-size: 12px;
  font-weight: 600;
  color: #8c8c8c;
  text-transform: uppercase;
  border-bottom: 1px solid #f0f0f0;
}

.sidebar-content {
  flex: 1;
  overflow-y: auto;
  padding: 8px 0;
}

:deep(.ant-tree-node-content-wrapper) {
  padding: 4px 8px;
}

:deep(.ant-tree-node-content-wrapper:hover) {
  background: #f5f5f5;
}

:deep(.ant-tree-node-selected .ant-tree-node-content-wrapper) {
  background: #e6f7ff;
}
</style>

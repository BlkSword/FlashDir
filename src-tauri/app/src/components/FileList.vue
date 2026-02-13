<template>
  <div class="file-list-container">
    <a-table
      :columns="columns"
      :data-source="items"
      :loading="loading"
      :pagination="false"
      :scroll="{ y: tableHeight }"
      :virtual="true"
      :row-height="40"
      size="small"
      row-key="path"
      @row-click="handleRowClick"
      :custom-row="customRow"
    >
      <template #headerCell="{ column }">
        <template v-if="column.key === 'name'">
          <span @click="handleSort('name')" class="sortable">
            名称
            <span v-if="sortConfig.column === 'name'" class="sort-icon">
              {{ sortConfig.direction === 'asc' ? '↑' : '↓' }}
            </span>
          </span>
        </template>
        <template v-else-if="column.key === 'size'">
          <span @click="handleSort('size')" class="sortable" style="float: right; margin-right: 16px;">
            大小
            <span v-if="sortConfig.column === 'size'" class="sort-icon">
              {{ sortConfig.direction === 'asc' ? '↑' : '↓' }}
            </span>
          </span>
        </template>
        <template v-else-if="column.key === 'type'">
          <span @click="handleSort('type')" class="sortable" style="display: flex; justify-content: center;">
            类型
            <span v-if="sortConfig.column === 'type'" class="sort-icon">
              {{ sortConfig.direction === 'asc' ? '↑' : '↓' }}
            </span>
          </span>
        </template>
      </template>

      <template #bodyCell="{ column, record }">
        <template v-if="column.key === 'name'">
          <span class="name-cell">
            <FolderOutlined v-if="record.isDir" class="folder-icon" />
            <FileOutlined v-else class="file-icon" />
            <span :title="record.path">{{ record.name }}</span>
          </span>
        </template>

        <template v-else-if="column.key === 'size'">
          <span class="size-cell">{{ record.sizeFormatted }}</span>
        </template>

        <template v-else-if="column.key === 'type'">
          <span class="type-cell">
            <a-tag v-if="record.isDir" color="orange">文件夹</a-tag>
            <a-tag v-else color="default">文件</a-tag>
          </span>
        </template>
      </template>
    </a-table>
  </div>
</template>

<script setup>
import { FolderOutlined, FileOutlined } from '@ant-design/icons-vue'
import { ref, computed } from 'vue'

const props = defineProps({
  items: {
    type: Array,
    default: () => []
  },
  loading: {
    type: Boolean,
    default: false
  },
  sortConfig: {
    type: Object,
    default: () => ({ column: 'size', direction: 'desc' })
  }
})

const emit = defineEmits(['sort', 'select'])

const tableHeight = ref('calc(100vh - 220px)')

const columns = ref([
  {
    title: '名称',
    dataIndex: 'name',
    key: 'name',
    width: '50%',
    ellipsis: true
  },
  {
    title: '大小',
    dataIndex: 'sizeFormatted',
    key: 'size',
    width: '25%',
    align: 'right'
  },
  {
    title: '类型',
    dataIndex: 'type',
    key: 'type',
    width: '25%',
    align: 'center'
  }
])

const customRow = (record) => {
  return {
    style: {
      cursor: record.isDir ? 'pointer' : 'default'
    },
    onClick: () => {
      handleRowClick(record)
    }
  }
}

const handleSort = (column) => {
  emit('sort', column)
}

const handleRowClick = (record) => {
  emit('select', record)
}
</script>

<style scoped>
.file-list-container {
  flex: 1;
  overflow: hidden;
  contain: content;
  will-change: transform;
}

.name-cell {
  display: flex;
  align-items: center;
  gap: 8px;
}

.folder-icon {
  color: #faad14;
  font-size: 16px;
  flex-shrink: 0;
}

.file-icon {
  color: #8c8c8c;
  font-size: 16px;
  flex-shrink: 0;
}

.sortable {
  cursor: pointer;
  user-select: none;
}

.sortable:hover {
  color: #1890ff;
}

.sort-icon {
  margin-left: 4px;
  color: #1890ff;
}

.type-cell {
  display: flex;
  justify-content: center;
}

.ant-table {
  font-size: 13px;
}

.ant-table-thead > tr > th {
  background: #fafafa;
  font-weight: 600;
}

.ant-table-tbody > tr {
  cursor: pointer;
}

.ant-table-tbody > tr:hover > td {
  background: #f5f5f5 !important;
}

.ant-table-tbody > tr.ant-table-row-selected > td {
  background: #e6f7ff !important;
}
</style>

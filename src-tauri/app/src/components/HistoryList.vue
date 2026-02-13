<template>
  <div class="history-list">
    <a-empty
      v-if="history.length === 0"
      description="暂无历史记录"
      :image="Empty.PRESENTED_IMAGE_SIMPLE"
    />

    <a-list
      v-else
      :data-source="history"
      size="small"
      :pagination="listPagination"
    >
      <template #renderItem="{ item }">
        <a-list-item
          class="history-item"
          @click="handleSelect(item.path)"
        >
          <a-list-item-meta>
            <template #title>
              <div class="history-path">{{ item.path }}</div>
            </template>
            <template #description>
              <div class="history-meta">
                <span>{{ item.sizeFormat || '未知大小' }}</span>
                <span>{{ formatTime(item.scanTime) }}</span>
                <span v-if="item.itemCount !== undefined">{{ item.itemCount }} 个项目</span>
                <span v-else-if="item.items">{{ item.items.length }} 个项目</span>
              </div>
            </template>
          </a-list-item-meta>
        </a-list-item>
      </template>
    </a-list>

    <div v-if="history.length > 0" class="history-actions">
      <a-space>
        <a-button size="small" danger @click="$emit('clear')">
          <template #icon>
            <DeleteOutlined />
          </template>
          清除历史
        </a-button>
      </a-space>
    </div>
  </div>
</template>

<script setup>
import { Empty } from 'ant-design-vue'
import { DeleteOutlined } from '@ant-design/icons-vue'
import { computed } from 'vue'

const props = defineProps({
  history: {
    type: Array,
    default: () => []
  }
})

const emit = defineEmits(['select', 'clear'])

// 分页配置 - 避免一次性渲染大量历史记录
const listPagination = computed(() => {
  if (props.history.length <= 10) {
    return false  // 少于10条时不分页
  }
  return {
    pageSize: 10,
    showSizeChanger: false,
    showTotal: (total) => `共 ${total} 条`,
    size: 'small',
  }
})

const handleSelect = (path) => {
  emit('select', path)
}

// 缓存时间格式化结果
const formatTimeCache = new Map()

const formatTime = (timestamp) => {
  if (!timestamp) return '未知时间'
  
  // 使用缓存避免重复计算
  if (formatTimeCache.has(timestamp)) {
    return formatTimeCache.get(timestamp)
  }

  let date
  if (typeof timestamp === 'number') {
    const isSeconds = timestamp < 10000000000
    date = new Date(isSeconds ? timestamp * 1000 : timestamp)
  } else {
    date = new Date(timestamp)
  }

  if (isNaN(date.getTime())) return '未知时间'

  const now = new Date()
  const diff = now - date

  let result
  if (diff < 60000) result = '刚刚'
  else if (diff < 3600000) result = `${Math.floor(diff / 60000)} 分钟前`
  else if (diff < 86400000) result = `${Math.floor(diff / 3600000)} 小时前`
  else if (diff < 604800000) result = `${Math.floor(diff / 86400000)} 天前`
  else result = date.toLocaleDateString('zh-CN')

  // 缓存结果
  formatTimeCache.set(timestamp, result)
  
  // 限制缓存大小
  if (formatTimeCache.size > 100) {
    const firstKey = formatTimeCache.keys().next().value
    formatTimeCache.delete(firstKey)
  }
  
  return result
}
</script>

<style scoped>
.history-list {
  max-height: 400px;
  overflow-y: auto;
}

.history-item {
  cursor: pointer;
  padding: 8px 12px;
  border-radius: 4px;
  margin-bottom: 4px;
  transition: background-color 0.2s;
}

.history-item:hover {
  background: #f5f5f5;
}

.history-path {
  font-family: 'Consolas', 'Monaco', monospace;
  font-size: 13px;
  word-break: break-all;
}

.history-meta {
  display: flex;
  gap: 16px;
  font-size: 12px;
  color: #8c8c8c;
}

.history-actions {
  padding: 12px;
  border-top: 1px solid #f0f0f0;
  display: flex;
  justify-content: center;
}

:deep(.ant-list-item-meta-description) {
  margin-top: 4px;
}

:deep(.ant-list-pagination) {
  margin-top: 12px;
  text-align: center;
}
</style>

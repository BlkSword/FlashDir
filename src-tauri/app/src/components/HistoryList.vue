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
                <span>{{ item.sizeFormat }}</span>
                <span>{{ formatTime(item.scanTime) }}</span>
                <span>{{ item.items.length }} 个项目</span>
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
import { DeleteOutlined } from '@ant-design/icons-vue'
import { Empty } from 'ant-design-vue'

const props = defineProps({
  history: {
    type: Array,
    default: () => []
  }
})

const emit = defineEmits(['select', 'clear'])

const handleSelect = (path) => {
  emit('select', path)
}

const formatTime = (timestamp) => {
  if (!timestamp) return '未知时间'

  const date = new Date(timestamp * 1000)
  const now = new Date()
  const diff = now - date

  if (diff < 60000) return '刚刚'
  if (diff < 3600000) return `${Math.floor(diff / 60000)} 分钟前`
  if (diff < 86400000) return `${Math.floor(diff / 3600000)} 小时前`
  if (diff < 604800000) return `${Math.floor(diff / 86400000)} 天前`

  return date.toLocaleDateString('zh-CN')
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
}

.history-item:hover {
  background: #f5f5f5;
}

.history-path {
  font-family: monospace;
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
</style>

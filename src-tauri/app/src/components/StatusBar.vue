<template>
  <div class="statusbar">
    <span class="statusbar-item">{{ path || '就绪' }}</span>
    <span class="statusbar-item">{{ totalItems }} 个项目</span>
    <span class="statusbar-item">{{ formattedSize }}</span>
    <span v-if="scanTime > 0" class="statusbar-item">
      <span class="time-label">总耗时:</span>
      <span class="time-value">{{ scanTime }}s</span>
    </span>
    <span v-if="shouldShowBackendTime" class="statusbar-item">
      <span class="time-label">后端:</span>
      <span class="time-value backend">{{ backendTime }}s</span>
    </span>
  </div>
</template>

<script setup>
import { computed } from 'vue'
import { formatSize } from '../utils/format.js'

const props = defineProps({
  path: String,
  totalItems: {
    type: Number,
    default: 0
  },
  totalSize: {
    type: Number,
    default: 0
  },
  scanTime: {
    type: [String, Number],
    default: 0
  },
  backendTime: {
    type: [String, Number],
    default: 0
  }
})

const formattedSize = computed(() => formatSize(props.totalSize))

// 判断是否显示后端时间
const shouldShowBackendTime = computed(() => {
  const bt = Number(props.backendTime)
  const st = Number(props.scanTime)
  return bt > 0 && bt !== st && Math.abs(bt - st) > 0.1  // 差异大于 0.1s 才显示
})
</script>

<style scoped>
.statusbar {
  background: #fafafa;
  border-top: 1px solid #f0f0f0;
  padding: 4px 12px;
  display: flex;
  align-items: center;
  font-size: 11px;
  color: #8c8c8c;
  gap: 16px;
}

.statusbar-item {
  white-space: nowrap;
}

.time-label {
  color: #999;
}

.time-value {
  color: #52c41a;
  font-family: 'Consolas', 'Monaco', monospace;
  font-weight: 600;
}

.time-value.backend {
  color: #1890ff;
}
</style>

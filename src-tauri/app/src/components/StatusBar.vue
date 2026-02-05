<template>
  <div class="statusbar">
    <span class="statusbar-item">{{ path || '就绪' }}</span>
    <span class="statusbar-item">{{ totalItems }} 个项目</span>
    <span class="statusbar-item">{{ formattedSize }}</span>
    <span v-if="scanTime > 0" class="statusbar-item">{{ scanTime.toFixed(2) }} 秒</span>
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
    type: Number,
    default: 0
  }
})

const formattedSize = computed(() => formatSize(props.totalSize))
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
</style>

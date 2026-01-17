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

const formattedSize = computed(() => {
  const bytes = props.totalSize
  if (bytes < 1024) return `${bytes} B`
  const kb = bytes / 1024
  if (kb < 1024) return `${kb.toFixed(1)} KB`
  const mb = kb / 1024
  if (mb < 1024) return `${mb.toFixed(1)} MB`
  const gb = mb / 1024
  return `${gb.toFixed(1)} GB`
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
</style>

<template>
  <footer
    class="h-7 shrink-0 flex items-center px-3 text-2xs border-t"
    :class="isDark ? 'bg-slate-900 border-slate-700 text-slate-400' : 'bg-slate-100 border-slate-300 text-slate-500'"
  >
    <div class="flex items-center gap-4 flex-1 min-w-0">
      <span v-if="loading" class="flex items-center gap-1.5">
        <svg class="w-3 h-3 animate-spin" fill="none" viewBox="0 0 24 24">
          <circle class="opacity-25" cx="12" cy="12" r="10" stroke="currentColor" stroke-width="4"></circle>
          <path class="opacity-75" fill="currentColor" d="M4 12a8 8 0 018-8V0C5.373 0 0 5.373 0 12h4zm2 5.291A7.962 7.962 0 014 12H0c0 3.042 1.135 5.824 3 7.938l3-2.647z"></path>
        </svg>
        扫描中...
      </span>
      <span v-else>就绪</span>
      <span class="mono truncate">{{ path || '未选择目录' }}</span>
    </div>
    <div class="flex items-center gap-4 shrink-0">
      <span>{{ totalItems.toLocaleString() }} 项</span>
      <span class="mono">{{ formatSize(totalSize) }}</span>
      <span v-if="backendTime > 0" class="mono">后端 {{ backendTime.toFixed(2) }}s</span>
      <span v-if="scanTime > 0" class="mono">总耗时 {{ scanTime.toFixed(2) }}s</span>
      <span
        v-if="mftAvailable"
        class="px-1.5 py-0.5 rounded text-xs font-medium"
        :class="isDark ? 'bg-green-900/60 text-green-300' : 'bg-green-100 text-green-700'"
        title="使用 NTFS MFT 直接读取"
      >MFT</span>
      <span
        v-else
        class="px-1.5 py-0.5 rounded text-xs font-medium"
        :class="isDark ? 'bg-amber-900/60 text-amber-300' : 'bg-amber-100 text-amber-700'"
        title="使用目录遍历"
      >遍历</span>
      <span
        v-if="isAdmin"
        class="px-1.5 py-0.5 rounded text-xs font-medium"
        :class="isDark ? 'bg-blue-900/60 text-blue-300' : 'bg-blue-100 text-blue-700'"
        title="当前进程已提升为管理员"
      >管理员</span>
    </div>
  </footer>
</template>

<script setup>
defineProps({
  path: { type: String, default: '' },
  totalItems: { type: Number, default: 0 },
  totalSize: { type: Number, default: 0 },
  scanTime: { type: Number, default: 0 },
  backendTime: { type: Number, default: 0 },
  loading: { type: Boolean, default: false },
  mftAvailable: { type: Boolean, default: false },
  isAdmin: { type: Boolean, default: false },
  isDark: { type: Boolean, default: false },
})

const formatSize = (bytes) => {
  if (bytes === 0) return '0 B'
  const units = ['B', 'KB', 'MB', 'GB', 'TB']
  const i = Math.floor(Math.log(bytes) / Math.log(1024))
  return (bytes / Math.pow(1024, i)).toFixed(i === 0 ? 0 : 2) + ' ' + units[i]
}
</script>

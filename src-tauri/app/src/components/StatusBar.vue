<template>
  <footer class="fd-statusbar">
    <div class="fd-status-left">
      <span v-if="loading" class="fd-status-loading">
        <svg class="animate-spin" width="12" height="12" fill="none" viewBox="0 0 24 24">
          <circle class="opacity-25" cx="12" cy="12" r="10" stroke="currentColor" stroke-width="4"></circle>
          <path class="opacity-75" fill="currentColor" d="M4 12a8 8 0 018-8V0C5.373 0 0 5.373 0 12h4zm2 5.291A7.962 7.962 0 014 12H0c0 3.042 1.135 5.824 3 7.938l3-2.647z"></path>
        </svg>
        扫描中…
      </span>
      <span v-else-if="globalSearchFailed" class="fd-status-warning truncate" :title="globalSearchStatus">
        {{ globalSearchStatus }}
      </span>
      <span v-else-if="globalSearchLoading" class="fd-status-loading truncate" :title="globalSearchStatus">
        <svg class="animate-spin" width="12" height="12" fill="none" viewBox="0 0 24 24">
          <circle class="opacity-25" cx="12" cy="12" r="10" stroke="currentColor" stroke-width="4"></circle>
          <path class="opacity-75" fill="currentColor" d="M4 12a8 8 0 018-8V0C5.373 0 0 5.373 0 12h4zm2 5.291A7.962 7.962 0 014 12H0c0 3.042 1.135 5.824 3 7.938l3-2.647z"></path>
        </svg>
        {{ globalSearchStatus }}
      </span>
      <span v-else>就绪</span>
      <span class="fd-status-path truncate">{{ path || '未选择目录' }}</span>
    </div>
    <div class="fd-status-right">
      <span>{{ totalItems.toLocaleString() }} 项</span>
      <span class="mono">{{ formatSize(totalSize) }}</span>
      <span v-if="backendTime > 0" class="mono">后端 {{ backendTime.toFixed(2) }}s</span>
      <span v-if="scanTime > 0" class="mono">总耗时 {{ scanTime.toFixed(2) }}s</span>
      <span v-if="mftAvailable" class="fd-status-pill fd-pill-mft" title="使用 NTFS MFT 直接读取">MFT</span>
      <span v-else class="fd-status-pill fd-pill-walk" title="使用目录遍历">遍历</span>
      <span v-if="isAdmin" class="fd-status-pill fd-pill-admin" title="当前进程已提升为管理员">管理员</span>
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
  globalSearchLoading: { type: Boolean, default: false },
  globalSearchFailed: { type: Boolean, default: false },
  globalSearchStatus: { type: String, default: '' },
})

const formatSize = (bytes) => {
  if (bytes === 0) return '0 B'
  const units = ['B', 'KB', 'MB', 'GB', 'TB']
  const i = Math.floor(Math.log(bytes) / Math.log(1024))
  return (bytes / Math.pow(1024, i)).toFixed(i === 0 ? 0 : 2) + ' ' + units[i]
}
</script>

<style scoped>
.fd-statusbar {
  grid-column: 1 / -1;
  display: flex;
  align-items: center;
  padding: 0 12px;
  background: var(--fd-accent);
  color: #fff;
  font-size: 12px;
  flex-shrink: 0;
}
.fd-status-left {
  display: flex;
  align-items: center;
  gap: 12px;
  flex: 1;
  min-width: 0;
}
.fd-status-path { color: rgba(255,255,255,0.85); }
.fd-status-right {
  display: flex;
  align-items: center;
  gap: 12px;
  flex-shrink: 0;
}
.fd-status-loading {
  display: inline-flex;
  align-items: center;
  gap: 4px;
}
.fd-status-warning {
  color: #ffe082;
  font-weight: 500;
}
.fd-status-pill {
  padding: 1px 6px;
  border-radius: 3px;
  font-size: 11px;
  font-weight: 600;
  border: 1px solid rgba(255,255,255,0.25);
}
.fd-pill-mft { background: rgba(137,209,133,0.2); }
.fd-pill-walk { background: rgba(220,182,122,0.2); }
.fd-pill-admin { background: rgba(255,255,255,0.2); }
.mono { font-family: Consolas, 'JetBrains Mono', monospace; }
</style>

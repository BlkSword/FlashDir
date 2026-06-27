<template>
  <div class="fd-stats">
    <div class="fd-stats-section">
      <div class="fd-stats-title">概览</div>
      <div class="fd-stat-row"><span class="fd-stat-label">总大小</span><span class="fd-stat-value">{{ formatSize(totalSize) }}</span></div>
      <div class="fd-stat-row"><span class="fd-stat-label">文件数</span><span class="fd-stat-value">{{ fileCount }}</span></div>
      <div class="fd-stat-row"><span class="fd-stat-label">目录数</span><span class="fd-stat-value">{{ dirCount }}</span></div>
      <div class="fd-stat-row"><span class="fd-stat-label">扫描耗时</span><span class="fd-stat-value">{{ scanTime.toFixed(2) }}s</span></div>
    </div>

    <div class="fd-stats-section">
      <div class="fd-stats-title">扩展名分布</div>
      <div v-for="(ext, index) in extStats" :key="index" class="fd-ext-row">
        <div class="fd-ext-head">
          <span>{{ ext.name }}</span>
          <span class="fd-ext-size">{{ ext.sizeFormatted }}</span>
        </div>
        <div class="fd-ext-bar"><div class="fd-ext-fill" :style="{ width: ext.percent + '%', background: ext.color }"></div></div>
      </div>
    </div>

    <div class="fd-stats-section">
      <div class="fd-stats-title">Top 5 大文件</div>
      <div v-for="(file, index) in topFiles" :key="index" class="fd-stat-row">
        <span class="truncate" style="max-width: 140px" :title="file.name">{{ file.name }}</span>
        <span class="fd-stat-value">{{ file.sizeFormatted }}</span>
      </div>
    </div>
  </div>
</template>

<script setup>
import { computed } from 'vue'

const props = defineProps({
  items: { type: Array, default: () => [] },
  totalSize: { type: Number, default: 0 },
  scanTime: { type: Number, default: 0 },
})

const fileCount = computed(() => props.items.filter(i => !i.isDir).length)
const dirCount = computed(() => props.items.filter(i => i.isDir).length)

const topFiles = computed(() => {
  return props.items
    .filter(i => !i.isDir)
    .sort((a, b) => b.size - a.size)
    .slice(0, 5)
    .map(i => ({ ...i, sizeFormatted: formatSize(i.size) }))
})

const extStats = computed(() => {
  const map = new Map()
  for (const item of props.items) {
    if (item.isDir) continue
    const ext = getExt(item.name)
    const key = ext || '无扩展名'
    const cur = map.get(key) || { size: 0, count: 0 }
    cur.size += item.size
    cur.count++
    map.set(key, cur)
  }

  const colors = ['#007acc', '#dcb67a', '#89d185', '#c586c0', '#a0a0a0']
  return Array.from(map.entries())
    .sort((a, b) => b[1].size - a[1].size)
    .slice(0, 5)
    .map(([name, data], idx) => ({
      name,
      sizeFormatted: formatSize(data.size),
      percent: props.totalSize ? Math.max(1, (data.size / props.totalSize) * 100) : 0,
      color: colors[idx % colors.length],
    }))
})

const getExt = (name) => {
  const dot = name.lastIndexOf('.')
  return dot > 0 ? name.slice(dot + 1).toLowerCase() : ''
}

const formatSize = (bytes) => {
  if (bytes === 0) return '0 B'
  const units = ['B', 'KB', 'MB', 'GB', 'TB']
  const i = Math.floor(Math.log(bytes) / Math.log(1024))
  return (bytes / Math.pow(1024, i)).toFixed(i === 0 ? 0 : 2) + ' ' + units[i]
}
</script>

<style scoped>
.fd-stats { display: flex; flex-direction: column; gap: 14px; }
.fd-stats-section { display: flex; flex-direction: column; gap: 4px; }
.fd-stats-title {
  font-size: 11px;
  color: var(--fd-text-2);
  font-weight: 700;
  text-transform: uppercase;
  letter-spacing: 0.5px;
  margin-bottom: 2px;
}
.fd-stat-row {
  display: flex;
  justify-content: space-between;
  padding: 3px 0;
  border-bottom: 1px solid var(--fd-border);
  font-size: 12px;
}
.fd-stat-label { color: var(--fd-text-2); }
.fd-stat-value { font-family: Consolas, 'JetBrains Mono', monospace; color: var(--fd-text-0); }
.fd-ext-row { margin-bottom: 6px; }
.fd-ext-head {
  display: flex;
  justify-content: space-between;
  font-size: 12px;
  margin-bottom: 3px;
}
.fd-ext-size { color: var(--fd-text-2); font-family: Consolas, 'JetBrains Mono', monospace; }
.fd-ext-bar {
  width: 100%;
  height: 4px;
  background: var(--fd-bg-3);
  border-radius: 2px;
  overflow: hidden;
}
.fd-ext-fill { height: 100%; border-radius: 2px; }
</style>

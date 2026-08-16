<template>
  <div class="fd-stats">
    <div class="fd-card">
      <div class="fd-card-title">扫描概览</div>
      <div class="fd-stat-grid">
        <div class="fd-stat">
          <b class="fd-stat-value">{{ formatSize(totalSize) }}</b>
          <span>总占用</span>
        </div>
        <div class="fd-stat">
          <b class="fd-stat-value">{{ fileCount.toLocaleString() }}</b>
          <span>文件</span>
        </div>
        <div class="fd-stat">
          <b class="fd-stat-value">{{ dirCount.toLocaleString() }}</b>
          <span>目录</span>
        </div>
        <div class="fd-stat">
          <b class="fd-stat-value">{{ scanTime.toFixed(2) }}s</b>
          <span>扫描耗时</span>
        </div>
      </div>
    </div>

    <div class="fd-card">
      <div class="fd-card-title">扩展名分布</div>
      <div v-for="(ext, index) in extStats" :key="index" class="fd-ext-row">
        <span class="fd-ext-label">{{ ext.name }}</span>
        <div class="fd-ext-track"><div class="fd-ext-fill" :style="{ width: ext.percent + '%', background: ext.color }"></div></div>
        <span class="fd-ext-size">{{ ext.sizeFormatted }}</span>
      </div>
    </div>

    <div class="fd-card">
      <div class="fd-card-title">Top 5 大文件</div>
      <div v-for="(file, index) in topFiles" :key="index" class="fd-top-row">
        <span class="fd-top-rank" :class="'r' + (index + 1)">{{ index + 1 }}</span>
        <span class="truncate fd-top-name" :title="file.name">{{ file.name }}</span>
        <span class="fd-top-size">{{ file.sizeFormatted }}</span>
      </div>
    </div>
  </div>
</template>

<script setup>
import { computed } from 'vue'
import { formatSize } from '../utils/format.js'

const props = defineProps({
  items: { type: Array, default: () => [] },
  totalSize: { type: Number, default: 0 },
  scanTime: { type: Number, default: 0 },
})

const fileCount = computed(() => props.items.filter(i => !i.isDir).length)
const dirCount = computed(() => props.items.filter(i => i.isDir).length)

const topFiles = computed(() => {
  // 单次遍历维护 Top 5，避免对数十万文件做全量排序
  const top = []
  for (const item of props.items) {
    if (item.isDir) continue
    const size = item.size || 0
    if (top.length < 5) {
      top.push(item)
      top.sort((a, b) => (b.size || 0) - (a.size || 0))
    } else if (size > (top[top.length - 1].size || 0)) {
      top[4] = item
      top.sort((a, b) => (b.size || 0) - (a.size || 0))
    }
  }
  return top.map(i => ({ ...i, sizeFormatted: formatSize(i.size) }))
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
</script>

<style scoped>
.fd-stats {
  display: flex;
  flex-direction: column;
  gap: 12px;
}
.fd-card {
  background: var(--fd-bg-2);
  border: 1px solid var(--fd-border);
  border-radius: 8px;
  padding: 12px;
}
.fd-card-title {
  font-size: 11px;
  color: var(--fd-text-2);
  font-weight: 700;
  text-transform: uppercase;
  letter-spacing: .5px;
  margin-bottom: 10px;
}
.fd-stat-grid {
  display: grid;
  grid-template-columns: 1fr 1fr;
  gap: 8px;
}
.fd-stat {
  background: var(--fd-bg-1);
  border: 1px solid var(--fd-border);
  border-radius: 6px;
  padding: 9px;
}
.fd-stat b {
  display: block;
  font-size: 15px;
  font-family: Consolas, 'JetBrains Mono', monospace;
  color: var(--fd-text-0);
}
.fd-stat span {
  font-size: 10.5px;
  color: var(--fd-text-2);
}
.fd-ext-row {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 4px 0;
}
.fd-ext-label {
  width: 56px;
  font-family: Consolas, 'JetBrains Mono', monospace;
  font-size: 11px;
  color: var(--fd-text-1);
}
.fd-ext-track {
  flex: 1;
  height: 5px;
  background: var(--fd-bg-3);
  border-radius: 3px;
  overflow: hidden;
}
.fd-ext-fill {
  height: 100%;
  border-radius: 3px;
}
.fd-ext-size {
  width: 58px;
  text-align: right;
  color: var(--fd-text-2);
  font-family: Consolas, 'JetBrains Mono', monospace;
  font-size: 10.5px;
}
.fd-top-row {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 6px 8px;
  background: var(--fd-bg-1);
  border-radius: 6px;
  margin-bottom: 4px;
}
.fd-top-rank {
  width: 16px;
  height: 16px;
  border-radius: 4px;
  background: var(--fd-bg-3);
  color: var(--fd-text-2);
  display: grid;
  place-items: center;
  font-size: 10px;
  font-family: Consolas, 'JetBrains Mono', monospace;
}
.fd-top-rank.r1 { background: var(--fd-selected); color: #fff; }
.fd-top-rank.r2 { background: rgba(0,122,204,.35); color: #fff; }
.fd-top-name {
  flex: 1;
  min-width: 0;
  color: var(--fd-text-1);
  font-size: 12px;
}
.fd-top-size {
  font-family: Consolas, 'JetBrains Mono', monospace;
  font-size: 11px;
  color: var(--fd-text-1);
}
</style>

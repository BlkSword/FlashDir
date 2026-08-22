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
      <div class="fd-card-title">Top 5 大文件</div>
      <div v-for="(file, index) in topFiles" :key="index" class="fd-top-row">
        <span class="fd-top-rank" :class="'r' + (index + 1)">{{ index + 1 }}</span>
        <span class="truncate fd-top-name" :title="file.name">{{ file.name }}</span>
        <span class="fd-top-size">{{ file.sizeFormatted || formatSize(file.size) }}</span>
      </div>
      <div v-if="topFiles.length === 0" class="fd-empty">暂无数据</div>
    </div>
  </div>
</template>

<script setup>
import { formatSize } from '../utils/format.js'

defineProps({
  totalSize: { type: Number, default: 0 },
  scanTime: { type: Number, default: 0 },
  fileCount: { type: Number, default: 0 },
  dirCount: { type: Number, default: 0 },
  topFiles: { type: Array, default: () => [] },
})
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
.fd-empty {
  text-align: center;
  color: var(--fd-text-3);
  font-size: 12px;
  padding: 12px 0;
}
</style>

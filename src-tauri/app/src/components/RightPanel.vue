<template>
  <aside class="fd-right-panel">
    <div class="fd-panel-tabs">
      <button
        v-for="tab in tabs"
        :key="tab.key"
        class="fd-panel-tab"
        :class="{ active: activeTab === tab.key }"
        @click="$emit('update:activeTab', tab.key)"
      >
        {{ tab.label }}
      </button>
    </div>

    <div class="fd-panel-body">
      <StatsTab
        v-if="activeTab === 'stats'"
        :total-size="totalSize"
        :scan-time="scanTime"
        :file-count="fileCount"
        :dir-count="dirCount"
        :top-files="topFiles"
      />
      <DevAnalyzer
        v-else-if="activeTab === 'dev'"
        :items="items"
        :total-size="totalSize"
        :current-path="currentPath"
      />
      <SnapshotCompare
        v-else-if="activeTab === 'snapshots'"
        :items="items"
        :total-size="totalSize"
        :current-path="currentPath"
      />
      <DuplicateFinder
        v-else-if="activeTab === 'duplicates'"
        :items="items"
        :current-path="currentPath"
      />
    </div>
  </aside>
</template>

<script setup>
import StatsTab from './StatsTab.vue'
import DevAnalyzer from './DevAnalyzer.vue'
import SnapshotCompare from './SnapshotCompare.vue'
import DuplicateFinder from './DuplicateFinder.vue'

const tabs = [
  { key: 'stats', label: '总览' },
  { key: 'dev', label: '开发者' },
  { key: 'snapshots', label: '快照' },
  { key: 'duplicates', label: '重复' },
]

defineProps({
  items: { type: Array, default: () => [] },
  totalSize: { type: Number, default: 0 },
  currentPath: { type: String, default: '' },
  activeTab: { type: String, default: 'stats' },
  scanTime: { type: Number, default: 0 },
  fileCount: { type: Number, default: 0 },
  dirCount: { type: Number, default: 0 },
  topFiles: { type: Array, default: () => [] },
})

defineEmits(['update:activeTab'])
</script>

<style scoped>
.fd-right-panel {
  grid-row: 2 / 3;
  grid-column: 3 / 4;
  background: var(--fd-bg-1);
  border-left: 1px solid var(--fd-border);
  display: flex;
  flex-direction: column;
  overflow: hidden;
}
.fd-panel-tabs {
  display: flex;
  gap: 6px;
  padding: 8px;
  border-bottom: 1px solid var(--fd-border);
  background: var(--fd-bg-1);
  flex-shrink: 0;
}
.fd-panel-tab {
  flex: 1;
  padding: 5px 0;
  border: none;
  background: transparent;
  color: var(--fd-text-2);
  font-size: 11px;
  font-weight: 600;
  letter-spacing: .4px;
  text-transform: uppercase;
  cursor: pointer;
  border-radius: 6px;
  transition: background .12s ease, color .12s ease;
}
.fd-panel-tab:hover {
  background: var(--fd-bg-2);
  color: var(--fd-text-0);
}
.fd-panel-tab.active {
  background: var(--fd-selected);
  color: #fff;
}
.fd-panel-body {
  flex: 1;
  overflow: auto;
  padding: 12px;
}
</style>

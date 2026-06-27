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
        :items="items"
        :total-size="totalSize"
        :scan-time="scanTime"
      />
      <Treemap
        v-else-if="activeTab === 'treemap'"
        :items="items"
        :total-size="totalSize"
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
    </div>
  </aside>
</template>

<script setup>
import StatsTab from './StatsTab.vue'
import Treemap from './Treemap.vue'
import DevAnalyzer from './DevAnalyzer.vue'
import SnapshotCompare from './SnapshotCompare.vue'

const tabs = [
  { key: 'stats', label: '统计' },
  { key: 'treemap', label: '热图' },
  { key: 'dev', label: '开发者' },
  { key: 'snapshots', label: '快照' },
]

defineProps({
  items: { type: Array, default: () => [] },
  totalSize: { type: Number, default: 0 },
  currentPath: { type: String, default: '' },
  activeTab: { type: String, default: 'stats' },
  scanTime: { type: Number, default: 0 },
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
  border-bottom: 1px solid var(--fd-border);
  flex-shrink: 0;
}
.fd-panel-tab {
  flex: 1;
  padding: 6px 0;
  border: none;
  background: transparent;
  color: var(--fd-text-2);
  font-size: 12px;
  cursor: pointer;
  border-bottom: 2px solid transparent;
  margin-bottom: -1px;
}
.fd-panel-tab:hover { color: var(--fd-text-1); }
.fd-panel-tab.active {
  color: var(--fd-text-0);
  border-bottom-color: var(--fd-accent);
  background: var(--fd-bg-2);
}
.fd-panel-body {
  flex: 1;
  overflow: auto;
  padding: 10px;
}
</style>

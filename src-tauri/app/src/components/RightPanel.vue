<template>
  <aside
    class="w-80 shrink-0 flex flex-col border-l"
    :class="isDark ? 'bg-slate-900 border-slate-700' : 'bg-white border-slate-300'"
  >
    <div class="flex border-b" :class="isDark ? 'border-slate-700' : 'border-slate-200'">
      <button
        v-for="tab in tabs"
        :key="tab.key"
        class="flex-1 px-2 py-2 text-xs font-medium transition-colors"
        :class="activeTab === tab.key
          ? (isDark ? 'text-blue-400 border-b-2 border-blue-500 bg-blue-900/20' : 'text-blue-600 border-b-2 border-blue-600 bg-blue-50/50')
          : (isDark ? 'text-slate-400 hover:bg-slate-800' : 'text-slate-500 hover:bg-slate-50')"
        @click="$emit('update:activeTab', tab.key)"
      >
        {{ tab.label }}
      </button>
    </div>

    <div class="flex-1 overflow-auto p-3">
      <StatsTab
        v-if="activeTab === 'stats'"
        :items="items"
        :total-size="totalSize"
        :scan-time="scanTime"
        :is-dark="isDark"
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
  isDark: { type: Boolean, default: false },
})

defineEmits(['update:activeTab'])
</script>

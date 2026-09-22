<script setup>
import { computed } from 'vue'
import Icon from './Icon.vue'
import Treemap from './Treemap.vue'
import GrowthPanel from './GrowthPanel.vue'
import DevAnalyzer from './DevAnalyzer.vue'
import DuplicateFinder from './DuplicateFinder.vue'
import SnapshotCompare from './SnapshotCompare.vue'
import { formatSize } from '../utils/format.js'

const props = defineProps({
  tab: { type: String, default: 'map' },
  items: { type: Array, default: () => [] },
  topFiles: { type: Array, default: () => [] },
  totalSize: { type: Number, default: 0 },
  currentPath: { type: String, default: '' },
  height: { type: Number, default: 230 },
  minHeight: { type: Number, default: 120 },
})
const emit = defineEmits(['update:tab', 'navigate', 'open', 'refresh', 'resize-start', 'reset-height'])

const tabs = [
  { key: 'map', label: '体积构成', hint: '按面积表示体积，点击进入子目录' },
  { key: 'big', label: '大文件', hint: '当前目录最大的文件' },
  { key: 'growth', label: '增长趋势', hint: '基于历史快照的体积变化' },
  { key: 'dupes', label: '重复文件', hint: '按内容哈希检测重复' },
  { key: 'snapshot', label: '快照对比', hint: '对比任意两份快照' },
  { key: 'dev', label: '开发缓存', hint: 'node_modules / target / 包缓存' },
]
const activeHint = computed(() => tabs.find((t) => t.key === props.tab)?.hint || '')
const isPanel = computed(() => ['dev', 'dupes', 'snapshot', 'growth'].includes(props.tab))

const bigList = computed(() => {
  const list = props.topFiles?.length ? props.topFiles : props.items.filter((i) => !i.isDir)
  return [...list].sort((a, b) => b.size - a.size).slice(0, 14)
})
const maxBig = computed(() => bigList.value.reduce((m, i) => Math.max(m, i.size), 0) || 1)
</script>

<template>
  <div class="dock" :style="{ '--dock-h': height + 'px', minHeight: minHeight + 'px' }">
    <!-- 顶部拖拽条：调整洞察坞高度（双击复位） -->
    <div
      class="dock-resizer"
      title="拖拽调整洞察坞高度（双击复位）"
      @mousedown="emit('resize-start', $event)"
      @dblclick="emit('reset-height')"
    />

    <div class="dock-tabs">
      <button
        v-for="(t, i) in tabs"
        :key="t.key"
        :class="{ on: tab === t.key }"
        :title="t.hint + `（Ctrl+${i + 1}）`"
        @click="emit('update:tab', t.key)"
      >
        {{ t.label }}
      </button>
      <span class="spacer" />
      <span class="grip"><Icon name="grip" :size="12" />{{ height }}px · 拖拽上边缘调整</span>
      <span class="note">{{ activeHint }}</span>
    </div>

    <div class="dock-body" :class="{ 'no-pad': isPanel }">
      <Treemap
        v-if="tab === 'map'"
        :items="items"
        :total="totalSize"
        @navigate="emit('navigate', $event)"
      />

      <div v-else-if="tab === 'big'" class="rows">
        <div v-if="!bigList.length" class="section-note">没有可展示的文件。</div>
        <div
          v-for="f in bigList"
          :key="f.path"
          class="rowline"
          style="cursor:pointer"
          :title="f.path"
          @click="emit('open', f)"
        >
          <span class="k">{{ formatSize(f.size).replace(' ', '') }}</span>
          <span class="p">{{ f.name }}</span>
          <span class="track"><i :style="{ width: (f.size / maxBig) * 100 + '%' }" /></span>
        </div>
      </div>

      <GrowthPanel
        v-else-if="tab === 'growth'"
        :current-path="currentPath"
        :total-size="totalSize"
        @refresh="emit('refresh')"
      />
      <DuplicateFinder v-else-if="tab === 'dupes'" :items="items" :current-path="currentPath" />
      <SnapshotCompare
        v-else-if="tab === 'snapshot'"
        :items="items"
        :total-size="totalSize"
        :current-path="currentPath"
        @refresh="emit('refresh')"
      />
      <DevAnalyzer
        v-else-if="tab === 'dev'"
        :items="items"
        :total-size="totalSize"
        :current-path="currentPath"
      />
    </div>
  </div>
</template>

<script setup>
import { ref, computed, watch } from 'vue'
import { invoke } from '@tauri-apps/api/core'
import Icon from './Icon.vue'
import { formatSize, formatSizeCompact, formatError } from '../utils/format.js'
import { useToasts } from '../composables/useToasts.js'

const props = defineProps({
  items: { type: Array, default: () => [] },
  totalSize: { type: Number, default: 0 },
  currentPath: { type: String, default: '' },
})
const toasts = useToasts()

const data = ref(null)
const scanning = ref(false)
const expanded = ref('')

async function analyze() {
  if (!props.currentPath) {
    toasts.warn('请先选择目录')
    return
  }
  scanning.value = true
  try {
    data.value = (await invoke('analyze_dev_disk', { path: props.currentPath })) || null
    expanded.value = data.value?.categories?.[0]?.category || ''
  } catch (e) {
    toasts.err('开发缓存分析失败：' + formatError(e))
    data.value = null
  } finally {
    scanning.value = false
  }
}
watch(() => props.currentPath, () => { data.value = null; expanded.value = '' })

const categories = computed(() => data.value?.categories || [])
const maxCat = computed(() => categories.value.reduce((m, c) => Math.max(m, c.totalSize), 0) || 1)
const toggle = (c) => { expanded.value = expanded.value === c.category ? '' : c.category }

/** Top 项只有名字没有路径：按名字+大小在当前页条目里反查，找到则可打开 */
const pathOf = (t) => {
  const hit = props.items.find((i) => i.name === t.name && i.size === t.size)
  return hit ? hit.path : null
}
async function openPath(p) {
  try { await invoke('open_path', { path: p }) } catch (e) { toasts.err('打开失败：' + formatError(e)) }
}
</script>

<template>
  <div class="dev">
    <div class="dev-bar">
      <button class="btn primary" :disabled="scanning || !currentPath" @click="analyze">
        <span v-if="scanning" class="spinner" />分析开发缓存
      </button>
      <span class="spacer" style="flex:1" />
      <span v-if="data" class="section-note">
        开发类占用 <b class="mono" style="color:var(--tx-0)">{{ formatSize(data.devTotalSize) }}</b>
        · 占全部 {{ (data.devPercent || 0).toFixed(1) }}%
        · 命中 {{ data.devItems.toLocaleString() }} / {{ data.totalItems.toLocaleString() }} 项
      </span>
    </div>

    <div v-if="scanning" class="section-note">正在分析当前目录…</div>
    <div v-else-if="!data" class="section-note" style="margin-top:8px">
      分析 node_modules、target、包管理器缓存、构建产物等开发类目录的占用与可回收空间。
    </div>

    <div v-else class="dev-list">
      <div v-for="c in categories" :key="c.category" class="dev-cat">
        <div class="dev-row" :class="{ open: expanded === c.category }" @click="toggle(c)">
          <Icon :name="expanded === c.category ? 'down' : 'right'" :size="11" />
          <span class="lbl">{{ c.label }}</span>
          <span class="cnt mono">{{ c.itemCount.toLocaleString() }} 项</span>
          <span class="bar"><i :style="{ width: (c.totalSize / maxCat) * 100 + '%' }" /></span>
          <span class="sz mono">{{ formatSize(c.totalSize) }}</span>
          <span class="pct mono">{{ (c.percentOfDev || 0).toFixed(0) }}%</span>
        </div>
        <div v-if="expanded === c.category" class="dev-detail">
          <div class="section-note" style="margin-bottom:4px">{{ c.description }}</div>
          <div class="rows">
            <div
              v-for="t in c.topItems"
              :key="t.name + t.size"
              class="rowline"
              :style="{ cursor: pathOf(t) ? 'pointer' : 'default' }"
              :title="pathOf(t) || t.name"
              @click="pathOf(t) && openPath(pathOf(t))"
            >
              <span class="k">{{ formatSizeCompact(t.size) }}</span>
              <span class="p">{{ t.name }}</span>
              <span v-if="pathOf(t)" class="tagline"><Icon name="folder-open" :size="12" />打开</span>
            </div>
            <div v-if="!c.topItems.length" class="section-note">该类别下没有可列出的具体项。</div>
          </div>
        </div>
      </div>
      <div v-if="!categories.length" class="section-note">当前目录没有识别到开发类缓存。</div>
    </div>
  </div>
</template>

<style scoped>
.dev { display: flex; flex-direction: column; height: 100%; min-height: 0; padding: 9px 10px; }
.dev-bar { display: flex; align-items: center; gap: 8px; margin-bottom: 8px; }
.dev-list { overflow: auto; flex: 1; min-height: 0; }
.dev-cat { border-bottom: 1px solid var(--bd-0); }
.dev-row {
  display: flex; align-items: center; gap: 8px; height: 24px; cursor: pointer; color: var(--tx-1);
}
.dev-row:hover { background: var(--hover); }
.dev-row.open { color: var(--tx-0); }
.dev-row .lbl { width: 150px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.dev-row .cnt { width: 78px; text-align: right; color: var(--tx-3); font-size: 10.5px; }
.dev-row .bar { flex: 1; height: 6px; background: var(--bg-3); border-radius: 2px; overflow: hidden; }
.dev-row .bar i { display: block; height: 100%; background: var(--heat-4); }
.dev-row .sz { width: 74px; text-align: right; color: var(--tx-0); }
.dev-row .pct { width: 40px; text-align: right; color: var(--tx-3); font-size: 10.5px; }
.dev-detail { padding: 4px 0 8px 20px; }
</style>

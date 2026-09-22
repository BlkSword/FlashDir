<script setup>
import { ref, computed, watch, onMounted } from 'vue'
import { invoke } from '@tauri-apps/api/core'
import Icon from './Icon.vue'
import { formatSize, formatSizeCompact, formatDateTime, formatError } from '../utils/format.js'
import { useToasts } from '../composables/useToasts.js'

const props = defineProps({
  currentPath: { type: String, default: '' },
  totalSize: { type: Number, default: 0 },
})
const emit = defineEmits(['refresh'])
const toasts = useToasts()

const snapshots = ref([])
const diff = ref(null)
const loading = ref(false)
const saving = ref(false)
const comparing = ref(false)

async function load() {
  if (!props.currentPath) {
    snapshots.value = []
    return
  }
  loading.value = true
  try {
    const list = await invoke('list_snapshots', { path: props.currentPath })
    snapshots.value = (list || []).slice().sort((a, b) => b.scanTime - a.scanTime)
  } catch (e) {
    snapshots.value = []
  } finally {
    loading.value = false
  }
}
onMounted(load)
watch(() => props.currentPath, () => { diff.value = null; load() })

async function save() {
  saving.value = true
  try {
    await invoke('save_snapshot_from_cache', { path: props.currentPath })
    toasts.ok('已保存当前快照')
    await load()
    emit('refresh')
  } catch (e) {
    toasts.err('保存快照失败：' + formatError(e))
  } finally {
    saving.value = false
  }
}

async function compareLatest() {
  if (snapshots.value.length < 2) {
    toasts.warn('至少需要两份快照才能对比')
    return
  }
  comparing.value = true
  try {
    diff.value = await invoke('compare_snapshots', {
      oldId: snapshots.value[1].id,
      newId: snapshots.value[0].id,
    })
  } catch (e) {
    toasts.err('对比失败：' + formatError(e))
  } finally {
    comparing.value = false
  }
}

async function compareCurrent() {
  comparing.value = true
  try {
    diff.value = await invoke('compare_with_latest_snapshot_from_cache', { path: props.currentPath })
    if (!diff.value) toasts.warn('当前目录还没有历史快照')
  } catch (e) {
    toasts.err('对比当前失败：' + formatError(e))
  } finally {
    comparing.value = false
  }
}

/* 趋势柱：按区间归一化高度（数值以标签为准），并把相邻差值标在柱下 */
const trend = computed(() => {
  const list = snapshots.value.slice().sort((a, b) => a.scanTime - b.scanTime)
  if (!list.length) return []
  const sizes = list.map((s) => s.totalSize || 0)
  const min = Math.min(...sizes)
  const max = Math.max(...sizes)
  return list.map((s, i) => {
    const prev = i > 0 ? sizes[i - 1] : null
    const delta = prev === null ? null : s.totalSize - prev
    const ratio = max > min ? (s.totalSize - min) / (max - min) : 1
    return { ...s, delta, height: 26 + ratio * 74 }
  })
})

const topAdded = computed(() =>
  (diff.value?.added || []).slice().sort((a, b) => b.size - a.size).slice(0, 8)
)
const topRemoved = computed(() =>
  (diff.value?.removed || []).slice().sort((a, b) => b.size - a.size).slice(0, 8)
)
const topModified = computed(() =>
  (diff.value?.modified || [])
    .slice()
    .sort((a, b) => Math.abs(b.delta) - Math.abs(a.delta))
    .slice(0, 8)
)
const netChange = computed(() => diff.value?.netChange ?? 0)
const summary = computed(() => diff.value?.summary || null)

async function openPath(p) {
  try {
    await invoke('open_path', { path: p })
  } catch (e) {
    toasts.err('打开失败：' + formatError(e))
  }
}
</script>

<template>
  <div class="growth">
    <div class="growth-bar">
      <button class="btn primary" :disabled="saving || !currentPath" @click="save">
        <span v-if="saving" class="spinner" />保存当前快照
      </button>
      <button class="btn" :disabled="comparing || snapshots.length < 2" @click="compareLatest">
        <span v-if="comparing" class="spinner" />对比最近两次
      </button>
      <button class="btn" :disabled="comparing || !snapshots.length" @click="compareCurrent">对比当前</button>
      <span class="spacer" style="flex:1" />
      <span class="section-note">
        <template v-if="snapshots.length">共 {{ snapshots.length }} 份快照 · 最新 {{ formatDateTime(snapshots[0].scanTime) }}</template>
        <template v-else>暂无快照</template>
      </span>
    </div>

    <div v-if="loading" class="section-note">正在读取快照…</div>

    <template v-else-if="!snapshots.length">
      <div class="section-note" style="margin-top:10px">
        保存两份以上快照即可看到体积变化趋势（快照保存在本地缓存库，保留 30 天）。
      </div>
    </template>

    <template v-else>
      <!-- 趋势柱：真实快照总量 -->
      <div class="trend">
        <div v-for="s in trend" :key="s.id" class="bar-wrap" :title="`${formatDateTime(s.scanTime)} · ${formatSize(s.totalSize)} · ${s.itemCount.toLocaleString()} 项`">
          <div class="bar" :style="{ height: s.height + '%' }" />
          <div class="val mono">{{ formatSizeCompact(s.totalSize) }}</div>
          <div class="time mono">{{ formatDateTime(s.scanTime) }}</div>
          <div class="delta mono" :class="{ up: s.delta > 0, down: s.delta < 0 }">
            <template v-if="s.delta === null">—</template>
            <template v-else>{{ s.delta > 0 ? '+' : '' }}{{ formatSizeCompact(s.delta) }}</template>
          </div>
        </div>
      </div>
      <div class="section-note" style="margin-bottom:8px">柱高按区间归一化，数值以标签为准。</div>

      <!-- 对比结果 -->
      <template v-if="diff">
        <div class="diff-summary">
          <span class="badge" :class="netChange > 0 ? 'warn' : 'ok'">
            净变化 {{ netChange > 0 ? '+' : '' }}{{ formatSize(netChange) }}
          </span>
          <span class="section-note">
            新增 {{ summary?.addedCount ?? diff.added?.length ?? 0 }} 项（+{{ formatSize(diff.addedTotalSize || 0) }}）·
            删除 {{ summary?.removedCount ?? diff.removed?.length ?? 0 }} 项（-{{ formatSize(diff.removedTotalSize || 0) }}）·
            修改 {{ summary?.modifiedCount ?? diff.modified?.length ?? 0 }} 项（{{ (diff.modifiedDelta || 0) >= 0 ? '+' : '' }}{{ formatSize(diff.modifiedDelta || 0) }}）
          </span>
        </div>
        <div class="cols2" style="margin-top:6px">
          <div>
            <div class="insp-h" style="margin-top:0">新增 / 增长（Top 8）</div>
            <div class="rows">
              <div v-for="i in topAdded" :key="'a' + i.path" class="rowline" style="cursor:pointer" :title="i.path" @click="openPath(i.path)">
                <span class="k" style="color:var(--bad)">+{{ formatSizeCompact(i.size) }}</span>
                <span class="p">{{ i.name }}</span>
              </div>
              <div v-for="i in topModified.filter((m) => m.delta > 0)" :key="'m' + i.path" class="rowline" style="cursor:pointer" :title="i.path" @click="openPath(i.path)">
                <span class="k" style="color:var(--bad)">+{{ formatSizeCompact(i.delta) }}</span>
                <span class="p">{{ i.name }}</span>
              </div>
              <div v-if="!topAdded.length" class="section-note">没有新增项。</div>
            </div>
          </div>
          <div>
            <div class="insp-h" style="margin-top:0">删除 / 缩小（Top 8）</div>
            <div class="rows">
              <div v-for="i in topRemoved" :key="'r' + i.path" class="rowline" style="cursor:pointer" :title="i.path" @click="openPath(i.path)">
                <span class="k" style="color:var(--ok)">-{{ formatSizeCompact(i.size) }}</span>
                <span class="p">{{ i.name }}</span>
              </div>
              <div v-for="i in topModified.filter((m) => m.delta < 0)" :key="'n' + i.path" class="rowline" style="cursor:pointer" :title="i.path" @click="openPath(i.path)">
                <span class="k" style="color:var(--ok)">-{{ formatSizeCompact(-i.delta) }}</span>
                <span class="p">{{ i.name }}</span>
              </div>
              <div v-if="!topRemoved.length" class="section-note">没有删除项。</div>
            </div>
          </div>
        </div>
      </template>
    </template>
  </div>
</template>

<style scoped>
.growth { display: flex; flex-direction: column; height: 100%; min-height: 0; }
.growth-bar { display: flex; align-items: center; gap: 6px; margin-bottom: 8px; }
.trend {
  display: flex; align-items: flex-end; gap: 8px; height: 92px;
  padding: 0 2px 2px; border-bottom: 1px solid var(--bd-0); margin-bottom: 4px;
}
.bar-
.bar-wrap { flex: 1; display: flex; flex-direction: column; align-items: center; justify-content: flex-end; height: 100%; min-width: 0; }
.bar { width: 100%; max-width: 46px; background: var(--heat-1); border-radius: 2px 2px 0 0; }
.bar-wrap:hover .bar { background: var(--accent); }
.val { font-size: 10px; color: var(--tx-1); margin-top: 2px; }
.time { font-size: 9.5px; color: var(--tx-3); }
.delta { font-size: 10px; color: var(--tx-2); }
.delta.up { color: var(--bad); }
.delta.down { color: var(--ok); }
.diff-summary { display: flex; align-items: center; gap: 10px; margin: 6px 0 2px; }
</style>

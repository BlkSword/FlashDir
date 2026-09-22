<script setup>
import { ref, computed, watch, onMounted } from 'vue'
import { invoke } from '@tauri-apps/api/core'
import Icon from './Icon.vue'
import { formatSize, formatSizeCompact, formatDateTime, formatError } from '../utils/format.js'
import { useToasts } from '../composables/useToasts.js'

const props = defineProps({
  items: { type: Array, default: () => [] },
  totalSize: { type: Number, default: 0 },
  currentPath: { type: String, default: '' },
})
const emit = defineEmits(['refresh'])
const toasts = useToasts()

const snapshots = ref([])
const selected = ref([])
const diff = ref(null)
const saving = ref(false)
const comparing = ref(false)

async function load() {
  if (!props.currentPath) { snapshots.value = []; return }
  try {
    const list = await invoke('list_snapshots', { path: props.currentPath })
    snapshots.value = (list || []).slice().sort((a, b) => b.scanTime - a.scanTime)
    selected.value = []
  } catch (e) {
    snapshots.value = []
  }
}
onMounted(load)
watch(() => props.currentPath, () => { diff.value = null; load() })

async function save() {
  if (!props.currentPath || !props.items.length) { toasts.warn('请先扫描目录'); return }
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

function pick(id) {
  const i = selected.value.indexOf(id)
  if (i >= 0) selected.value.splice(i, 1)
  else {
    selected.value.push(id)
    if (selected.value.length > 2) selected.value.shift()
  }
}

async function compareSelected() {
  if (selected.value.length !== 2) return
  comparing.value = true
  try {
    const [a, b] = selected.value.slice().sort((x, y) => x - y)
    diff.value = await invoke('compare_snapshots', { oldId: a, newId: b })
  } catch (e) {
    toasts.err('对比失败：' + formatError(e))
  } finally {
    comparing.value = false
  }
}

async function compareLatest() {
  if (snapshots.value.length < 2) { toasts.warn('至少需要两份快照'); return }
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
    const r = await invoke('compare_with_latest_snapshot_from_cache', { path: props.currentPath })
    if (!r) toasts.warn('当前目录还没有历史快照')
    diff.value = r || null
  } catch (e) {
    toasts.err('对比当前失败：' + formatError(e))
  } finally {
    comparing.value = false
  }
}

async function remove(id) {
  try {
    await invoke('delete_snapshot', { id })
    toasts.ok('已删除快照')
    await load()
  } catch (e) {
    toasts.err('删除失败：' + formatError(e))
  }
}

const added = computed(() => (diff.value?.added || []).slice().sort((a, b) => b.size - a.size).slice(0, 60))
const removed = computed(() => (diff.value?.removed || []).slice().sort((a, b) => b.size - a.size).slice(0, 60))
const modified = computed(() =>
  (diff.value?.modified || []).slice().sort((a, b) => Math.abs(b.delta) - Math.abs(a.delta)).slice(0, 60)
)
const summary = computed(() => diff.value?.summary || null)
const net = computed(() => diff.value?.netChange ?? 0)

async function openPath(p) {
  try { await invoke('open_path', { path: p }) } catch (e) { toasts.err('打开失败：' + formatError(e)) }
}
</script>

<template>
  <div class="snap">
    <div class="snap-bar">
      <button class="btn primary" :disabled="saving || !currentPath || !items.length" @click="save">
        <span v-if="saving" class="spinner" />保存当前快照
      </button>
      <button class="btn" :disabled="comparing || snapshots.length < 2" @click="compareLatest">
        <span v-if="comparing" class="spinner" />对比最近两次
      </button>
      <button class="btn" :disabled="comparing || !snapshots.length" @click="compareCurrent">对比当前</button>
      <button class="btn" :disabled="selected.length !== 2 || comparing" @click="compareSelected">
        对比所选（{{ selected.length }}/2）
      </button>
      <span class="spacer" style="flex:1" />
      <span class="section-note">{{ snapshots.length }} 份快照</span>
    </div>

    <div class="snap-body">
      <div class="snap-list">
        <div v-if="!snapshots.length" class="section-note" style="padding:6px 0">
          还没有快照。扫描目录后点"保存当前快照"，保存两份以上即可对比增长/清理情况。
        </div>
        <div
          v-for="s in snapshots"
          :key="s.id"
          class="snap-row"
          :class="{ on: selected.includes(s.id) }"
          @click="pick(s.id)"
        >
          <input type="checkbox" :checked="selected.includes(s.id)" @click.stop="pick(s.id)" />
          <span class="t mono">{{ formatDateTime(s.scanTime) }}</span>
          <span class="c mono">{{ s.itemCount.toLocaleString() }} 项</span>
          <span class="s mono">{{ formatSizeCompact(s.totalSize) }}</span>
          <span class="spacer" style="flex:1" />
          <button class="chip" title="删除该快照" @click.stop="remove(s.id)">删除</button>
        </div>
      </div>

      <div v-if="diff" class="snap-diff">
        <div class="diff-summary">
          <span class="badge" :class="net > 0 ? 'warn' : 'ok'">
            净变化 {{ net > 0 ? '+' : '' }}{{ formatSize(net) }}
          </span>
          <span class="section-note">
            新增 {{ summary?.addedCount ?? added.length }} 项（+{{ formatSize(diff.addedTotalSize || 0) }}）·
            删除 {{ summary?.removedCount ?? removed.length }} 项（-{{ formatSize(diff.removedTotalSize || 0) }}）·
            修改 {{ summary?.modifiedCount ?? modified.length }} 项（{{ (diff.modifiedDelta || 0) >= 0 ? '+' : '' }}{{ formatSize(diff.modifiedDelta || 0) }}）
          </span>
        </div>
        <div class="diff-cols">
          <div class="diff-col">
            <h5>新增 / 增长（{{ added.length + modified.filter((m) => m.delta > 0).length }}）</h5>
            <div class="list">
              <div v-for="i in added" :key="'a' + i.path" class="diff-row add" :title="i.path" @click="openPath(i.path)">
                <span class="d">+{{ formatSizeCompact(i.size) }}</span>
                <span class="p">{{ i.path }}</span>
              </div>
              <div v-for="i in modified.filter((m) => m.delta > 0)" :key="'m' + i.path" class="diff-row add" :title="i.path" @click="openPath(i.path)">
                <span class="d">+{{ formatSizeCompact(i.delta) }}</span>
                <span class="p">{{ i.path }}</span>
              </div>
            </div>
          </div>
          <div class="diff-col">
            <h5>删除 / 缩小（{{ removed.length + modified.filter((m) => m.delta < 0).length }}）</h5>
            <div class="list">
              <div v-for="i in removed" :key="'r' + i.path" class="diff-row del" :title="i.path">
                <span class="d">-{{ formatSizeCompact(i.size) }}</span>
                <span class="p">{{ i.path }}</span>
              </div>
              <div v-for="i in modified.filter((m) => m.delta < 0)" :key="'n' + i.path" class="diff-row del" :title="i.path" @click="openPath(i.path)">
                <span class="d">-{{ formatSizeCompact(-i.delta) }}</span>
                <span class="p">{{ i.path }}</span>
              </div>
            </div>
          </div>
        </div>
      </div>
    </div>
  </div>
</template>

<style scoped>
.snap { display: flex; flex-direction: column; height: 100%; min-height: 0; padding: 9px 10px; }
.snap-bar { display: flex; align-items: center; gap: 6px; margin-bottom: 8px; }
.snap-body { flex: 1; min-height: 0; display: flex; flex-direction: column; gap: 8px; }
.snap-list { max-height: 34%; overflow: auto; border: 1px solid var(--bd-0); border-radius: var(--r); }
.snap-row {
  display: flex; align-items: center; gap: 10px; height: 23px; padding: 0 8px;
  border-bottom: 1px solid var(--bd-0); color: var(--tx-1); cursor: pointer; font-size: 11.5px;
}
.snap-row:last-child { border-bottom: 0; }
.snap-row:hover { background: var(--hover); }
.snap-row.o
.snap-row.on { background: var(--sel); color: var(--tx-0); }
.snap-row .t { width: 120px; }
.snap-row .c { width: 84px; color: var(--tx-3); }
.snap-row .s { width: 66px; color: var(--tx-0); }
.snap-diff { flex: 1; min-height: 0; display: flex; flex-direction: column; }
.diff-summary { display: flex; align-items: center; gap: 10px; margin-bottom: 6px; }
.diff-cols { flex: 1; min-height: 0; }
</style>

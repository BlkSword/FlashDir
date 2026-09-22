<script setup>
import { ref, computed } from 'vue'
import { invoke } from '@tauri-apps/api/core'
import Icon from './Icon.vue'
import { formatSize, formatDateTime, formatError } from '../utils/format.js'
import { useToasts } from '../composables/useToasts.js'

const props = defineProps({
  items: { type: Array, default: () => [] },
  currentPath: { type: String, default: '' },
})
const toasts = useToasts()

const result = ref(null)
const scanning = ref(false)
const minSizeMB = ref(1)
const expanded = ref(null)

async function run() {
  if (!props.currentPath || !props.items.length) {
    toasts.warn('请先扫描目录')
    return
  }
  scanning.value = true
  try {
    const minSize = Math.max(0, Math.round((minSizeMB.value || 0) * 1024 * 1024))
    result.value = await invoke('find_duplicates', { path: props.currentPath, minSize })
    expanded.value = result.value?.groups?.[0]?.files?.[0]?.path || null
  } catch (e) {
    toasts.err('重复文件检测失败：' + formatError(e))
    result.value = null
  } finally {
    scanning.value = false
  }
}

const groups = computed(() => result.value?.groups || [])
const toggle = (g) => { expanded.value = expanded.value === g.files[0]?.path ? null : g.files[0]?.path }

async function openPath(p) {
  try { await invoke('open_path', { path: p }) } catch (e) { toasts.err('打开失败：' + formatError(e)) }
}
</script>

<template>
  <div class="dup">
    <div class="dup-bar">
      <button class="btn primary" :disabled="scanning || !currentPath || !items.length" @click="run">
        <span v-if="scanning" class="spinner" />扫描重复文件
      </button>
      <label class="filter-box" style="flex: 0 0 168px">
        <span class="section-note">最小</span>
        <input type="number" min="0" max="102400" v-model.number="minSizeMB" style="width: 58px" />
        <span class="section-note">MB</span>
      </label>
      <span class="spacer" style="flex:1" />
      <span v-if="result" class="section-note">
        {{ result.totalGroups.toLocaleString() }} 组 / {{ result.totalFiles.toLocaleString() }} 个文件 ·
        可回收 <b class="mono" style="color:var(--warn)">{{ result.totalWastedFormatted }}</b>
      </span>
    </div>

    <div v-if="scanning" class="section-note">正在按内容哈希比对（大目录可能需要几秒）…</div>
    <div v-else-if="!result" class="section-note" style="margin-top:8px">
      按大小分组后对同大小文件做内容哈希，找出真正重复的文件。检测结果只做展示，不会删除任何文件。
    </div>
    <div v-else-if="!groups.length" class="section-note" style="margin-top:8px">没有发现重复文件。</div>

    <div v-else class="dup-list">
      <div v-for="g in groups" :key="g.files[0]?.path" class="dup-group">
        <div class="dup-head" @click="toggle(g)">
          <Icon :name="expanded === g.files[0]?.path ? 'down' : 'right'" :size="11" />
          <span class="sz mono">{{ g.sizeFormatted }}</span>
          <span class="cnt mono">× {{ g.fileCount }}</span>
          <span class="lbl">可回收 {{ g.wastedFormatted }}</span>
          <span class="spacer" style="flex:1" />
          <span class="tagline">{{ g.files[0]?.name }}</span>
        </div>
        <div v-if="expanded === g.files[0]?.path" class="dup-files">
          <div v-for="f in g.files" :key="f.path" class="rowline" style="cursor:pointer" :title="f.path" @click="openPath(f.path)">
            <span class="k">{{ formatDateTime(f.mtime) }}</span>
            <span class="p">{{ f.path }}</span>
            <span class="tagline"><Icon name="folder-open" :size="12" />打开</span>
          </div>
        </div>
      </div>
    </div>
  </div>
</template>

<style scoped>
.dup { display: flex; flex-direction: column; height: 100%; min-height: 0; padding: 9px 10px; }
.dup-bar { display: flex; align-items: center; gap: 8px; margin-bottom: 8px; }
.dup-list { overflow: auto; flex: 1; min-height: 0; }
.dup-group { border-bottom: 1px solid var(--bd-0); }
.dup-head { display: flex; align-items: center; gap: 8px; height: 24px; cursor: pointer; color: var(--tx-1); }
.dup-head:hover { background: var(--hover); }
.dup-head .sz { width: 70px; color: var(--tx-0); }
.dup-head .cnt { width: 52px; color: var(--tx-2); font-size: 10.5px; }
.dup-head .lbl { color: var(--warn); font-size: 11.5px; }
.dup-files { padding: 2px 0 6px 20px; }
</style>

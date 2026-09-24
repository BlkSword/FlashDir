<script setup>
import { ref, computed } from 'vue'
import Icon from './Icon.vue'
import { formatSize, formatDateTime } from '../utils/format.js'

const props = defineProps({
  query: { type: String, default: '' },
  results: { type: Array, default: () => [] },
  loading: { type: Boolean, default: false },
  elapsedMs: { type: Number, default: 0 },
  indexCount: { type: Number, default: 0 },
  indexPartial: { type: Boolean, default: false },
  scope: { type: String, default: '' },
})
const emit = defineEmits(['open', 'navigate', 'back', 'retry', 'copy'])

const body = ref(null)
const active = ref(-1)

const rows = computed(() => props.results)
const maxSize = computed(() => rows.value.reduce((m, r) => Math.max(m, r.size || 0), 0) || 1)

function move(delta) {
  if (!rows.value.length) return
  active.value = Math.min(rows.value.length - 1, Math.max(0, (active.value < 0 ? -1 : active.value) + delta))
  const el = body.value?.querySelector(`[data-idx="${active.value}"]`)
  el?.scrollIntoView({ block: 'nearest' })
}
function openRow(i = active.value) {
  const r = rows.value[i]
  if (!r) return
  if (r.isDir) emit('navigate', r.path)
  else emit('open', r.path)
}
function onKey(e) {
  if (e.key === 'ArrowDown') { e.preventDefault(); move(1) }
  else if (e.key === 'ArrowUp') { e.preventDefault(); move(-1) }
  else if (e.key === 'Enter') { e.preventDefault(); openRow() }
  else if (e.key === 'Escape') { e.preventDefault(); emit('back') }
}
</script>

<template>
  <div class="table-wrap" tabindex="0" @keydown="onKey">
    <div class="search-head">
      <Icon name="search" :size="13" />
      <span class="mono">“{{ query }}”</span>
      <span class="badge" :class="results.length ? 'ok' : 'warn'">{{ results.length }} 命中</span>
      <span class="section-note">
        索引 {{ indexCount.toLocaleString() }} 项<template v-if="indexPartial">（部分目录）</template>
        · 耗时 {{ elapsedMs }}ms<template v-if="scope"> · 范围 {{ scope }}</template>
      </span>
      <span class="spacer" style="flex:1" />
      <button class="chip" title="重新搜索" @click="emit('retry')"><Icon name="refresh" />重试</button>
      <button class="chip" title="返回目录浏览（Esc）" @click="emit('back')"><Icon name="left" />返回目录</button>
    </div>

    <div class="thead" style="grid-template-columns:24px minmax(220px,1fr) minmax(200px,1.2fr) 96px 104px">
      <div />
      <div>名称</div>
      <div>路径</div>
      <div class="num">大小</div>
      <div>修改时间</div>
    </div>

    <div ref="body" class="tbody">
      <div v-if="loading" class="empty-state"><span class="spinner" /><span class="t">正在搜索…</span></div>
      <div v-else-if="!rows.length" class="empty-state">
        <Icon name="search" :size="26" />
        <span class="t">没有匹配的条目</span>
        <span class="hint">
          语法：<span class="mono">ext:zip</span> · <span class="mono">size:&gt;1GB</span> ·
          <span class="mono">dir:node_modules</span> · <span class="mono">!tmp</span> ·
          <span class="mono">type:file|dir</span> · <span class="mono">mtime:&gt;7d</span><br />
          若索引尚未建立，可在状态栏点击"索引"或按 Ctrl+K 里的"建立全局索引"。
        </span>
      </div>
      <div
        v-for="(r, i) in rows"
        :key="r.path"
        class="trow"
        :class="{ odd: i % 2 === 1, sel: i === active }"
        :data-idx="i"
        :style="{ gridTemplateColumns: '24px minmax(220px,1fr) minmax(200px,1.2fr) 96px 104px' }"
        @click="active = i"
        @dblclick="openRow(i)"
        @contextmenu.prevent="emit('copy', r.path)"
      >
        <div><Icon :name="r.isDir ? 'folder' : 'file'" :size="13" /></div>
        <div class="nm"><span style="overflow:hidden;text-overflow:ellipsis">{{ r.name }}</span></div>
        <div class="mono" style="color:var(--tx-3)" :title="r.path">{{ r.path }}</div>
        <div class="num">{{ formatSize(r.size) }}</div>
        <div class="mono">{{ formatDateTime(r.mtime) }}</div>
      </div>
    </div>

    <div class="tfoot">
      <span>双击打开 · <kbd>↑</kbd><kbd>↓</kbd> 选择 · <kbd>Enter</kbd> 打开 · <kbd>Esc</kbd> 返回</span>
      <span style="margin-left:auto">按相关性排序（名称匹配优先，其次体积）</span>
    </div>
  </div>
</template>

<style scoped>
.search-head {
  display: flex; align-items: center; gap: 9px; height: 30px; padding: 0 10px;
  border-bottom: 1px solid var(--bd-0); background: var(--bg-1); color: var(--tx-1);
}
.search-head .mono { color: var(--tx-0); }
</style>

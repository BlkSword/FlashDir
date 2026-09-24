<script setup>
import { ref, computed, onMounted, onUnmounted, watch } from 'vue'
import Icon from './Icon.vue'
import { formatSize, formatDateTime } from '../utils/format.js'

const props = defineProps({
  query: { type: String, default: '' },
  results: { type: Array, default: () => [] },
  total: { type: Number, default: 0 },
  truncated: { type: Boolean, default: false },
  loading: { type: Boolean, default: false },
  loadingMore: { type: Boolean, default: false },
  elapsedMs: { type: Number, default: 0 },
  indexCount: { type: Number, default: 0 },
  indexPartial: { type: Boolean, default: false },
  scope: { type: String, default: '' },
  exporting: { type: Boolean, default: false },
})
const emit = defineEmits(['open', 'navigate', 'back', 'retry', 'copy', 'more', 'export-all'])

const ROW_H = 23
const body = ref(null)
const active = ref(-1)
const scrollTop = ref(0)
const viewportH = ref(400)
let ro = null

const rows = computed(() => props.results)

/* ── 虚拟滚动：只渲染可视窗口内的行（几万条也不卡） ── */
const OVERSCAN = 12
const startIndex = computed(() => Math.max(0, Math.floor(scrollTop.value / ROW_H) - OVERSCAN))
const endIndex = computed(() =>
  Math.min(rows.value.length, Math.ceil((scrollTop.value + viewportH.value) / ROW_H) + OVERSCAN)
)
const visibleRows = computed(() =>
  rows.value.slice(startIndex.value, endIndex.value).map((r, i) => ({ r, idx: startIndex.value + i }))
)
const padTop = computed(() => startIndex.value * ROW_H)
const totalHeight = computed(() => rows.value.length * ROW_H)

function measure() {
  const el = body.value
  if (!el) return
  viewportH.value = el.clientHeight || 400
  scrollTop.value = el.scrollTop
}
function onScroll() {
  const el = body.value
  if (el) scrollTop.value = el.scrollTop
}
onMounted(() => {
  measure()
  if (typeof ResizeObserver !== 'undefined') {
    ro = new ResizeObserver(measure)
    if (body.value) ro.observe(body.value)
  }
})
onUnmounted(() => ro?.disconnect())
watch(
  () => props.query,
  () => {
    active.value = -1
    if (body.value) body.value.scrollTop = 0
    scrollTop.value = 0
  }
)

function move(delta) {
  if (!rows.value.length) return
  const next = Math.min(rows.value.length - 1, Math.max(0, (active.value < 0 ? -1 : active.value) + delta))
  active.value = next
  // 让目标行进入可视窗口（虚拟列表里行可能尚未渲染）
  const el = body.value
  if (el) {
    const top = next * ROW_H
    if (top < el.scrollTop) el.scrollTop = top
    else if (top + ROW_H > el.scrollTop + el.clientHeight) el.scrollTop = top + ROW_H - el.clientHeight
    scrollTop.value = el.scrollTop
  }
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
  else if (e.key === 'PageDown') { e.preventDefault(); move(Math.floor(viewportH.value / ROW_H)) }
  else if (e.key === 'PageUp') { e.preventDefault(); move(-Math.floor(viewportH.value / ROW_H)) }
  else if (e.key === 'Enter') { e.preventDefault(); openRow() }
  else if (e.key === 'Escape') { e.preventDefault(); emit('back') }
}
/** 滚到底部自动加载下一页（还有更多时） */
function onScrollLoad() {
  onScroll()
  const el = body.value
  if (!el || props.loadingMore || !props.truncated) return
  if (el.scrollTop + el.clientHeight >= el.scrollHeight - 120) emit('more')
}
</script>

<template>
  <div class="table-wrap" tabindex="0" @keydown="onKey">
    <div class="search-head">
      <Icon name="search" :size="13" />
      <span class="mono">“{{ query }}”</span>
      <span class="badge" :class="results.length ? 'ok' : 'warn'">
        命中 {{ (total || results.length).toLocaleString() }} 项
      </span>
      <span class="section-note">
        <template v-if="results.length < (total || results.length)">
          已显示前 {{ results.length.toLocaleString() }} 项
        </template>
        · 索引 {{ indexCount.toLocaleString() }} 项<template v-if="indexPartial">（部分目录）</template>
        · 耗时 {{ elapsedMs }}ms<template v-if="scope"> · 范围 {{ scope }}</template>
      </span>
      <span class="spacer" style="flex:1" />
      <button
        v-if="truncated"
        class="chip"
        :disabled="loadingMore"
        title="再加载 1000 条"
        @click="emit('more')"
      >
        <span v-if="loadingMore" class="spinner" /><Icon v-else name="down" />加载更多
      </button>
      <button
        class="chip"
        :disabled="exporting || !results.length"
        title="导出全部命中（最多 5 万条）为 CSV"
        @click="emit('export-all')"
      >
        <span v-if="exporting" class="spinner" /><Icon v-else name="tray" />导出全部
      </button>
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

    <div ref="body" class="tbody" @scroll="onScrollLoad">
      <div v-if="loading" class="empty-state"><span class="spinner" /><span class="t">正在搜索…</span></div>
      <div v-else-if="!rows.length" class="empty-state">
        <Icon name="search" :size="26" />
        <span class="t">没有匹配的条目</span>
        <span class="hint">
          语法：<span class="mono">ext:zip</span> · <span class="mono">size:&gt;1GB</span> ·
          <span class="mono">dir:node_modules</span> · <span class="mono">!tmp</span> ·
          <span class="mono">type:file|dir</span> · <span class="mono">mtime:&gt;7d</span>
        </span>
      </div>

      <template v-else>
        <!-- 虚拟滚动：外层撑满总高度，内层按可视窗口偏移渲染 -->
        <div :style="{ height: totalHeight + 'px', position: 'relative' }">
          <div :style="{ transform: `translateY(${padTop}px)` }">
            <div
              v-for="row in visibleRows"
              :key="row.r.path"
              class="trow"
              :class="{ odd: row.idx % 2 === 1, sel: row.idx === active }"
              :data-idx="row.idx"
              :style="{
                gridTemplateColumns: '24px minmax(220px,1fr) minmax(200px,1.2fr) 96px 104px',
                height: ROW_H + 'px',
              }"
              @click="active = row.idx"
              @dblclick="openRow(row.idx)"
              @contextmenu.prevent="emit('copy', row.r.path)"
            >
              <div><Icon :name="row.r.isDir ? 'folder' : 'file'" :size="13" /></div>
              <div class="nm"><span style="overflow:hidden;text-overflow:ellipsis">{{ row.r.name }}</span></div>
              <div class="mono" style="color:var(--tx-3)" :title="row.r.path">{{ row.r.path }}</div>
              <div class="num">{{ formatSize(row.r.size) }}</div>
              <div class="mono">{{ formatDateTime(row.r.mtime) }}</div>
            </div>
          </div>
        </div>
      </template>
    </div>

    <div class="tfoot">
      <span>
        <template v-if="total > results.length">
          共 <b>{{ total.toLocaleString() }}</b> 项命中 · 已加载
          <b>{{ results.length.toLocaleString() }}</b> 项 · 滚到底部或点"加载更多"
        </template>
        <template v-else>共 <b>{{ results.length.toLocaleString() }}</b> 项命中</template>
      </span>
      <span style="margin-left:auto">
        <kbd>↑</kbd><kbd>↓</kbd>/<kbd>PgUp</kbd><kbd>PgDn</kbd> 选择 · <kbd>Enter</kbd> 打开 ·
        <kbd>Esc</kbd> 返回 · 双击打开
      </span>
    </div>
  </div>
</template>

<style scoped>
.search-head {
  display: flex; align-items: center; gap: 9px; height: 30px; padding: 0 10px;
  border-bottom: 1px solid var(--bd-0); background: var(--bg-1); color: var(--tx-1);
}
.search-head .mono { color: var(--tx-0); }
.tbody { position: relative; }
</style>

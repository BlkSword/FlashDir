<script setup>
import { computed, ref } from 'vue'
import Icon from './Icon.vue'
import {
  formatSize, formatSizeCompact, formatDateTime, formatRelative,
  percentOf, heatLevel, heatVar, accessTimeReliable,
} from '../utils/format.js'

const props = defineProps({
  items: { type: Array, default: () => [] },
  totalSize: { type: Number, default: 0 },
  parentTotal: { type: Number, default: 0 },
  totalItems: { type: Number, default: 0 },
  loading: { type: Boolean, default: false },
  sortConfig: { type: Object, default: () => ({ column: 'size', direction: 'desc' }) },
  page: { type: Number, default: 1 },
  pageSize: { type: Number, default: 100 },
  filter: { type: String, default: '' },
  selectedPath: { type: String, default: '' },
})
const emit = defineEmits([
  'sort', 'select', 'open', 'context', 'page-change', 'page-size-change', 'up',
])

const body = ref(null)
const selected = ref(-1)

const columns = [
  { key: 'name', label: '名称', sortable: true, width: 'minmax(180px,1fr)' },
  { key: 'size', label: '大小', sortable: true, num: true },
  { key: 'pct', label: '占父目录', sortable: false },
  { key: 'mtime', label: '修改时间', sortable: true },
  { key: 'atime', label: '最近访问', sortable: true },
  { key: 'hint', label: '提示', sortable: false },
]

const maxSize = computed(() => props.items.reduce((m, i) => Math.max(m, i.size), 0))
const atimeReliable = computed(() => accessTimeReliable(props.items))
const parentTotal = computed(() => props.parentTotal || props.totalSize || 0)

const totalPages = computed(() => Math.max(1, Math.ceil(props.totalItems / props.pageSize)))
const rangeStart = computed(() => (props.totalItems ? (props.page - 1) * props.pageSize + 1 : 0))
const rangeEnd = computed(() => Math.min(props.totalItems, props.page * props.pageSize))

const sortArrow = (key) =>
  props.sortConfig.column === key ? (props.sortConfig.direction === 'desc' ? '▼' : '▲') : ''

const isTempPath = (p) => /(\\|\/)(temp|tmp|crashdumps|cache|caches|logs?)(\\|\/|$)/i.test(p || '')
const hintOf = (item) => {
  if (item.isDir) return { text: 'DIR', cls: '' }
  if (isTempPath(item.path)) return { text: '临时/缓存', cls: 'warn' }
  if (item.mtime && Date.now() / 1000 - item.mtime > 86400 * 180) return { text: '冷数据', cls: '' }
  return { text: '', cls: '' }
}

const selectRow = (idx) => {
  selected.value = idx
  const item = props.items[idx]
  if (item) emit('select', item)
}

const onKeydown = (e) => {
  if (!props.items.length) return
  if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
    e.preventDefault()
    const next = Math.min(props.items.length - 1, Math.max(0, (selected.value < 0 ? -1 : selected.value) + (e.key === 'ArrowDown' ? 1 : -1)))
    selectRow(next)
    const el = body.value?.querySelector(`[data-idx="${next}"]`)
    el?.scrollIntoView({ block: 'nearest' })
  } else if (e.key === 'Enter') {
    const item = props.items[selected.value]
    if (item) emit('open', item)
  } else if (e.key === 'Backspace') {
    e.preventDefault()
    emit('up')
  } else if (e.key === ' ') {
    e.preventDefault()
    const item = props.items[selected.value]
    if (item) emit('select', item)
  } else if (e.altKey && /^[1-6]$/.test(e.key)) {
    e.preventDefault()
    const col = columns[Number(e.key) - 1]
    if (col?.sortable) emit('sort', col.key)
  }
}

const onContext = (item, idx, e) => {
  selectRow(idx)
  emit('context', { item, event: e })
}

const changePage = (p) => {
  if (p < 1 || p > totalPages.value) return
  selected.value = -1
  emit('page-change', p)
}
</script>

<template>
  <div class="table-wrap" tabindex="0" @keydown="onKeydown">
    <div class="thead">
      <div />
      <div
        v-for="c in columns"
        :key="c.key"
        :class="{ num: c.num, sortable: c.sortable }"
        :style="c.key === 'name' ? 'grid-column: span 1' : ''"
        :title="c.sortable ? '点击排序（Alt+' + (columns.indexOf(c) + 1) + '）' : ''"
        @click="c.sortable && emit('sort', c.key)"
      >
        {{ c.label }}
        <span v-if="sortArrow(c.key)" class="arrow">{{ sortArrow(c.key) }}</span>
        <span v-if="c.key === 'atime' && !atimeReliable" class="section-note" style="font-size:10px">(未启用)</span>
      </div>
    </div>

    <div ref="body" class="tbody">
      <div v-if="loading && !items.length" class="empty-state">
        <span class="spinner" />
        <span class="t">正在读取目录…</span>
      </div>
      <div v-else-if="!items.length" class="empty-state">
        <Icon name="search" :size="26" />
        <span class="t">{{ filter ? '没有匹配的条目' : '该目录为空或尚未扫描' }}</span>
        <span class="hint">
          <template v-if="filter">
            查询：<span class="mono">{{ filter }}</span> —— 试试去掉 <span class="mono">size</span> 限制，
            或检查 <span class="mono">ext:</span> 拼写。语法：<span class="mono">ext:zip size:&gt;1GB dir:node_modules !tmp type:file mtime:&gt;7d</span>
          </template>
          <template v-else>
            在工具栏输入路径后按 Enter 扫描；管理员模式下走 MFT 直读（秒级），否则回退目录遍历。
          </template>
        </span>
      </div>

      <div
        v-for="(item, idx) in items"
        :key="item.path"
        class="trow"
        :class="{ odd: idx % 2 === 1, sel: item.path === selectedPath }"
        :data-idx="idx"
        @click="selectRow(idx)"
        @dblclick="emit('open', item)"
        @contextmenu.prevent="onContext(item, idx, $event)"
      >
        <div>
          <Icon :name="item.isDir ? 'folder' : 'file'" :size="13" />
        </div>
        <div class="nm">
          <span style="overflow:hidden;text-overflow:ellipsis">{{ item.name }}</span>
          <span v-if="!item.isDir && item.name.includes('.')" class="ext">
            {{ item.name.slice(item.name.lastIndexOf('.')).toLowerCase() }}
          </span>
        </div>
        <div class="num">{{ formatSize(item.size) }}</div>
        <div class="pct">
          <span class="track">
            <i
              :style="{ width: percentOf(item.size, maxSize) + '%', background: heatVar(heatLevel(item.size, maxSize)) }"
            />
          </span>
          <span class="v">{{ percentOf(item.size, parentTotal).toFixed(1) }}%</span>
        </div>
        <div class="mono" :title="formatDateTime(item.mtime)">{{ formatDateTime(item.mtime) }}</div>
        <div class="mono" :title="item.atime ? formatDateTime(item.atime) : '无访问时间'">
          <template v-if="atimeReliable">{{ formatRelative(item.atime) }}</template>
          <template v-else>{{ formatRelative(item.mtime) }}</template>
        </div>
        <div>
          <span v-if="hintOf(item).text" class="badge" :class="hintOf(item).cls">{{ hintOf(item).text }}</span>
        </div>
      </div>
    </div>

    <div class="tfoot">
      <span>
        显示 <b>{{ rangeStart }}–{{ rangeEnd }}</b> / <b>{{ totalItems.toLocaleString() }}</b> 项
      </span>
      <span>合计 <b>{{ formatSizeCompact(totalSize) }}</b></span>
      <span v-if="filter">过滤 <span class="mono" style="color:var(--tx-1)">{{ filter }}</span></span>
      <span class="pager">
        <button :disabled="page <= 1" title="第一页" @click="changePage(1)">«</button>
        <button :disabled="page <= 1" @click="changePage(page - 1)">上一页</button>
        <span class="mono">{{ page }} / {{ totalPages }}</span>
        <button :disabled="page >= totalPages" @click="changePage(page + 1)">下一页</button>
        <button :disabled="page >= totalPages" title="最后一页" @click="changePage(totalPages)">»</button>
        <select
          :value="pageSize"
          style="height:18px;background:var(--bg-3);color:var(--tx-1);border:1px solid var(--bd-1);border-radius:3px;font:11px var(--font-mono)"
          @change="emit('page-size-change', Number($event.target.value))"
        >
          <option :value="100">100/页</option>
          <option :value="200">200/页</option>
          <option :value="500">500/页</option>
          <option :value="1000">1000/页</option>
        </select>
      </span>
    </div>
  </div>
</template>

<script setup>
import { computed, ref, nextTick } from 'vue'
import Icon from './Icon.vue'
import { formatSizeCompact } from '../utils/format.js'

const props = defineProps({
  path: { type: String, default: '' },
  loading: { type: Boolean, default: false },
  filter: { type: String, default: '' },
  hits: { type: Number, default: 0 },
  totalItems: { type: Number, default: 0 },
  totalSize: { type: Number, default: 0 },
  view: { type: String, default: 'list' },
  canBack: { type: Boolean, default: false },
  canForward: { type: Boolean, default: false },
  canUp: { type: Boolean, default: false },
  forceBusy: { type: Boolean, default: false },
})
const emit = defineEmits([
  'scan', 'force-scan', 'cancel-scan', 'refresh', 'navigate',
  'update:filter', 'update:view', 'export', 'up', 'back', 'forward', 'search-global',
])

const editing = ref(false)
const draft = ref('')
const pathInput = ref(null)

const segments = computed(() => {
  const p = (props.path || '').replace(/\\/g, '/')
  if (!p) return []
  const parts = p.split('/').filter(Boolean)
  const out = []
  let acc = ''
  parts.forEach((seg, i) => {
    acc = i === 0 ? seg : `${acc}/${seg}`
    out.push({ label: seg, path: acc, last: i === parts.length - 1 })
  })
  return out
})

const startEdit = async () => {
  draft.value = props.path || ''
  editing.value = true
  await nextTick()
  pathInput.value?.focus()
  pathInput.value?.select()
}
const commitEdit = () => {
  editing.value = false
  const v = draft.value.trim()
  if (v && v !== props.path) emit('scan', v)
}
const onFilterInput = (e) => emit('update:filter', e.target.value)

/** 目录选择器：使用 Tauri 的 dialog 插件（withGlobalTauri 注入） */
const browse = async () => {
  const dialog = window.__TAURI__?.dialog
  if (!dialog?.open) return
  try {
    const picked = await dialog.open({ directory: true, multiple: false, title: '选择要扫描的目录' })
    if (picked) emit('scan', typeof picked === 'string' ? picked : picked.path)
  } catch (e) {
    /* 用户取消或权限不足：静默 */
  }
}
</script>

<template>
  <div class="toolbar">
    <button class="btn ghost" :disabled="!canBack" title="后退" @click="emit('back')"><Icon name="left" /></button>
    <button class="btn ghost" :disabled="!canForward" title="前进" @click="emit('forward')"><Icon name="right" /></button>
    <button class="btn ghost" :disabled="!canUp" title="上一级（Backspace）" @click="emit('up')"><Icon name="up" /></button>
    <span class="sep" />

    <button class="btn primary" :disabled="loading" title="扫描当前路径（Enter）" @click="emit('scan', path)">
      <Icon name="scan" />扫描
    </button>
    <button v-if="loading" class="btn" title="取消扫描（Esc）" @click="emit('cancel-scan')">
      <Icon name="xc" />取消
    </button>
    <button v-else class="btn" :disabled="!path" title="走 USN 快路径刷新（F5）" @click="emit('refresh')">
      <Icon name="refresh" />刷新
    </button>
    <button class="btn ghost" :disabled="loading || !path" title="忽略缓存强制全量重扫（Ctrl+Shift+R）" @click="emit('force-scan')">
      <Icon name="zap" />强制
    </button>
    <button class="btn ghost" title="选择要扫描的目录…" @click="browse">
      <Icon name="folder-open" />浏览
    </button>
    <span class="sep" />

    <div class="pathbar" title="双击可直接编辑路径">
      <template v-if="!editing">
        <template v-for="(s, i) in segments" :key="s.path">
          <span class="caret" v-if="i">›</span>
          <span class="crumb" :class="{ cur: s.last }" @click="emit('navigate', s.path)">{{ s.label }}</span>
        </template>
        <span v-if="!segments.length" class="crumb">未选择目录</span>
        <span class="meta" @dblclick.stop="startEdit">
          {{ totalItems.toLocaleString() }} 项 · {{ formatSizeCompact(totalSize) }}
        </span>
      </template>
      <input
        v-else
        ref="pathInput"
        v-model="draft"
        class="mono"
        style="flex:1;background:none;border:0;outline:none;color:var(--tx-0);font:12px var(--font-mono)"
        @keydown.enter="commitEdit"
        @keydown.esc="editing = false"
        @blur="commitEdit"
      />
      <span v-if="!editing" class="crumb" title="编辑路径" style="padding:1px 3px" @click="startEdit">
        <Icon name="pencil" :size="12" />
      </span>
    </div>

    <label
      class="filter-box"
      title="输入即过滤当前目录（匹配名称与相对路径）；按 Enter 用全局索引搜索整个磁盘。语法：ext:zip size:>100MB dir:node_modules !tmp type:file mtime:>7d"
    >
      <Icon name="filter" :size="13" />
      <input
        :value="filter"
        placeholder="过滤当前目录；回车全局搜索"
        spellcheck="false"
        @input="onFilterInput"
        @keydown.enter.prevent="emit('search-global', filter)"
      />
      <span class="hits">
        <template v-if="filter">{{ hits }} 命中 · <kbd>Enter</kbd> 全局</template>
      </span>
    </label>

    <div class="seg">
      <button :class="{ on: view === 'list' }" title="列表视图" @click="emit('update:view', 'list')">列表</button>
      <button :class="{ on: view === 'map' }" title="热图视图" @click="emit('update:view', 'map')">热图</button>
    </div>

    <button class="btn ghost" title="导出当前页为 CSV" @click="emit('export')"><Icon name="tray" />导出</button>
  </div>
</template>

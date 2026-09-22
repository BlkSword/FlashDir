<script setup>
import { ref, computed, watch, nextTick } from 'vue'
import { invoke } from '@tauri-apps/api/core'
import Icon from './Icon.vue'
import { formatSizeCompact } from '../utils/format.js'
import { formatError } from '../utils/format.js'

const props = defineProps({
  rootPath: { type: String, default: '' },
  selectedPath: { type: String, default: '' },
  volumes: { type: Array, default: () => [] },
  history: { type: Array, default: () => [] },
})
const emit = defineEmits(['navigate', 'error'])

// 节点：{ path, name, size, isDir, loaded, open, children }
const nodes = ref([])
const loading = ref(false)
const focusIndex = ref(-1)

/** 点击树区域时保证容器获得焦点（否则键盘事件收不到） */
const ensureFocus = (e) => {
  const el = e.currentTarget
  if (el && el.focus) el.focus({ preventScroll: true })
}

const scrollToRow = async (idx) => {
  await nextTick()
  const el = document.querySelector(`.tree-scroll .tnode[data-idx="${idx}"]`)
  el?.scrollIntoView({ block: 'nearest' })
}

const toNode = (item) => ({
  path: item.path,
  name: item.name,
  size: item.size || 0,
  isDir: item.isDir !== false,
  loaded: false,
  open: false,
  children: [],
})

const loadRoot = async () => {
  const root = props.rootPath
  nodes.value = []
  if (!root) return
  loading.value = true
  try {
    const children = await invoke('get_dir_children', { path: root })
    nodes.value = (children || []).map(toNode).sort((a, b) => b.size - a.size)
  } catch (e) {
    emit('error', '读取目录失败: ' + formatError(e))
    nodes.value = []
  } finally {
    loading.value = false
  }
}

const toggle = async (node) => {
  if (!node.isDir) return
  node.open = !node.open
  if (node.open && !node.loaded) {
    try {
      const children = await invoke('get_dir_children', { path: node.path })
      node.children = (children || []).map(toNode).sort((a, b) => b.size - a.size)
      node.loaded = true
    } catch (e) {
      emit('error', '展开失败: ' + formatError(e))
      node.open = false
    }
  }
}

const click = (node) => {
  const idx = visible.value.findIndex((r) => r.node.path === node.path)
  if (idx >= 0) focusIndex.value = idx
  emit('navigate', node.path)
}

// 选中项所在分支自动展开（最多 4 层，避免深路径时大量请求）
const expandTo = async (target) => {
  if (!target || !nodes.value.length) return
  const root = props.rootPath.replace(/\\/g, '/')
  const norm = target.replace(/\\/g, '/')
  if (!norm.toLowerCase().startsWith(root.toLowerCase())) return
  const rest = norm.slice(root.length).replace(/^\//, '')
  if (!rest) return
  const parts = rest.split('/')
  let level = nodes.value
  for (let i = 0; i < parts.length - 1 && i < 4; i++) {
    const seg = parts[i]
    let node = level.find((n) => n.name.toLowerCase() === seg.toLowerCase())
    if (!node) break
    if (!node.open) await toggle(node)
    level = node.children
  }
}

watch(() => props.rootPath, loadRoot, { immediate: true })
watch(() => props.selectedPath, (p) => expandTo(p))

const maxSize = computed(() => nodes.value.reduce((m, n) => Math.max(m, n.size), 0))
const fill = (node) => {
  if (!maxSize.value) return 0
  return Math.max(2, Math.round((node.size / maxSize.value) * 100))
}

const historyTop = computed(() => (props.history || []).slice(0, 6))

/** 扁平化的可见节点（支持任意层级），fill = 占父目录体积 */
const visible = computed(() => {
  const out = []
  const walk = (list, depth, parentSize) => {
    for (const n of list) {
      out.push({
        node: n,
        depth,
        fill: parentSize > 0 ? Math.max(2, Math.min(100, Math.round((n.size / parentSize) * 100))) : 100,
      })
      if (n.open && n.children && n.children.length) walk(n.children, depth + 1, n.size)
    }
  }
  walk(nodes.value, 0, maxSize.value)
  return out
})

/**
 * 树内键盘导航：
 * ↑↓ 移动焦点 · → 展开（已展开则进入首个子项）· ← 折叠（否则回到父项）
 * Enter 扫描该目录 · Home/End 首尾
 */
async function onKey(e) {
  const list = visible.value
  if (!list.length) return
  const cur = focusIndex.value
  const row = list[cur]
  if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
    e.preventDefault()
    const next = Math.min(list.length - 1, Math.max(0, (cur < 0 ? -1 : cur) + (e.key === 'ArrowDown' ? 1 : -1)))
    focusIndex.value = next
    scrollToRow(next)
  } else if (e.key === 'ArrowRight') {
    e.preventDefault()
    if (!row) return
    if (row.node.isDir && !row.node.open) {
      await toggle(row.node)
    } else if (cur + 1 < list.length && list[cur + 1].depth > row.depth) {
      focusIndex.value = cur + 1
      scrollToRow(cur + 1)
    }
  } else if (e.key === 'ArrowLeft') {
    e.preventDefault()
    if (!row) return
    if (row.node.open) {
      await toggle(row.node)
    } else {
      for (let i = cur - 1; i >= 0; i--) {
        if (list[i].depth < row.depth) {
          focusIndex.value = i
          scrollToRow(i)
          break
        }
      }
    }
  } else if (e.key === 'Enter') {
    e.preventDefault()
    if (row) emit('navigate', row.node.path)
  } else if (e.key === 'Home') {
    e.preventDefault()
    focusIndex.value = 0
    scrollToRow(0)
  } else if (e.key === 'End') {
    e.preventDefault()
    focusIndex.value = list.length - 1
    scrollToRow(list.length - 1)
  }
}

/** 路径末段（模板里避免使用正则字面量：Vue 表达式解析器不支持） */
const baseName = (p) => (p || '').split(/[\/]/).filter(Boolean).pop() || p || ''
</script>

<template>
  <div class="fd-col tree">
    <div class="tree-head">
      <Icon name="folder" :size="12" />
      <span>目录 · 按占用排序</span>
      <span class="section-note" style="font-size:10px;margin-left:4px">↑↓ ←→ Enter</span>
      <span class="spacer" style="flex:1" />
      <button class="theme-btn" style="padding:0 2px" title="刷新目录树" @click="loadRoot">
        <Icon name="refresh" :size="12" />
      </button>
    </div>
    <div class="tree-scroll" tabindex="0" @keydown="onKey" @click="ensureFocus">
      <div v-if="loading" class="tree-empty"><span class="spinner" style="display:inline-block;vertical-align:-3px" /> 正在读取…</div>
      <div v-else-if="!nodes.length" class="tree-empty">
        选择一个卷或目录后，这里会列出子目录占用。
      </div>
      <template v-else>
        <div
          v-for="(row, i) in visible"
          :key="row.node.path"
          class="tnode"
          :class="{ on: row.node.path === selectedPath, focus: focusIndex === i }"
          :data-idx="i"
          :style="{ paddingLeft: 6 + row.depth * 14 + 'px' }"
          :title="row.node.path"
          @click="click(row.node)"
        >
          <span class="fill" :style="{ width: row.fill + '%' }" />
          <span class="twist" @click.stop="toggle(row.node)">
            <Icon :name="row.node.open ? 'down' : 'right'" :size="9" />
          </span>
          <Icon class="ico" :name="row.node.open ? 'folder-open' : 'folder'" :size="13" />
          <span class="lbl">{{ row.node.name }}</span>
          <span class="sz">{{ formatSizeCompact(row.node.size) }}</span>
        </div>
        <div v-if="!visible.length" class="tree-empty">该目录没有子目录。</div>
      </template>

      <template v-if="volumes.length">
        <div class="tree-sec">驱动器</div>
        <div
          v-for="v in volumes"
          :key="'vol-' + v.letter"
          class="tnode"
          :class="{ on: (selectedPath || '').toUpperCase().startsWith(v.letter + ':') && selectedPath.length <= 3 }"
          @click="emit('navigate', v.letter + ':/')"
        >
          <span class="twist" />
          <Icon class="ico" name="drive" :size="13" />
          <span class="lbl">{{ v.letter }}: {{ v.label || (v.fs || '') }}</span>
          <span class="sz">{{ formatSizeCompact(v.freeBytes) }} 可用</span>
        </div>
      </template>

      <template v-if="historyTop.length">
        <div class="tree-sec">最近扫描</div>
        <div
          v-for="(h, i) in historyTop"
          :key="'h' + i"
          class="tnode"
          :class="{ on: h.path === selectedPath }"
          :title="h.path"
          @click="emit('navigate', h.path)"
        >
          <span class="twist" />
          <Icon class="ico" name="clock" :size="13" />
          <span class="lbl">{{ baseName(h.path) }}</span>
          <span class="sz">{{ formatSizeCompact(h.totalSize || 0) }}</span>
        </div>
      </template>
    </div>
  </div>
</template>

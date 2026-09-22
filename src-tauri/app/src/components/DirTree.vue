<script setup>
import { ref, computed, watch } from 'vue'
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

const click = (node) => emit('navigate', node.path)

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

/** 路径末段（模板里避免使用正则字面量：Vue 表达式解析器不支持） */
const baseName = (p) => (p || '').split(/[\/]/).filter(Boolean).pop() || p || ''
</script>

<template>
  <div class="fd-col tree">
    <div class="tree-head">
      <Icon name="folder" :size="12" />
      <span>目录 · 按占用排序</span>
      <span class="spacer" style="flex:1" />
      <button class="theme-btn" style="padding:0 2px" title="刷新目录树" @click="loadRoot">
        <Icon name="refresh" :size="12" />
      </button>
    </div>
    <div class="tree-scroll">
      <div v-if="loading" class="tree-empty"><span class="spinner" style="display:inline-block;vertical-align:-3px" /> 正在读取…</div>
      <div v-else-if="!nodes.length" class="tree-empty">
        选择一个卷或目录后，这里会列出子目录占用。
      </div>
      <template v-else>
        <div
          v-for="row in visible"
          :key="row.node.path"
          class="tnode"
          :class="{ on: row.node.path === selectedPath }"
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

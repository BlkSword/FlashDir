<template>
  <aside
    class="shrink-0 flex flex-col border-r transition-all duration-200 overflow-hidden"
    :class="[
      isDark ? 'bg-slate-900 border-slate-700' : 'bg-white border-slate-300',
      collapsed ? 'w-0 opacity-0' : 'w-64 opacity-100'
    ]"
  >
    <div class="flex-1 overflow-y-auto">
      <!-- Quick access -->
      <div class="px-3 py-2">
        <div
          class="text-2xs font-semibold uppercase tracking-wider mb-1.5"
          :class="isDark ? 'text-slate-500' : 'text-slate-500'"
        >
          快速访问
        </div>
        <nav class="space-y-0.5">
          <button
            v-for="item in quickAccess"
            :key="item.action"
            class="w-full flex items-center gap-2 px-2.5 py-1.5 rounded text-xs text-left transition-colors"
            :class="isDark ? 'hover:bg-slate-800 text-slate-300' : 'hover:bg-slate-100 text-slate-700'"
            @click="$emit('quick-access', item.action)"
          >
            <component :is="item.icon" class="w-4 h-4" :class="isDark ? 'text-slate-400' : 'text-slate-500'" />
            <span class="truncate">{{ item.name }}</span>
          </button>
        </nav>
      </div>

      <!-- Directory tree -->
      <div class="px-3 py-2 border-t" :class="isDark ? 'border-slate-700' : 'border-slate-200'">
        <div
          class="text-2xs font-semibold uppercase tracking-wider mb-1.5"
          :class="isDark ? 'text-slate-500' : 'text-slate-500'"
        >
          目录树
        </div>
        <div v-if="!treeData.length" class="text-xs text-slate-400 px-2.5 py-2">
          扫描后显示目录结构
        </div>
        <TreeNode
          v-for="node in treeData"
          :key="node.key"
          :node="node"
          :selected-path="selectedPath"
          :is-dark="isDark"
          @select="$emit('select', $event)"
        />
      </div>

      <!-- History -->
      <div class="px-3 py-2 border-t" :class="isDark ? 'border-slate-700' : 'border-slate-200'">
        <div
          class="text-2xs font-semibold uppercase tracking-wider mb-1.5"
          :class="isDark ? 'text-slate-500' : 'text-slate-500'"
        >
          历史
        </div>
        <div v-if="!history.length" class="text-xs text-slate-400 px-2.5 py-2">
          暂无历史记录
        </div>
        <div v-else class="space-y-0.5">
          <button
            v-for="(item, index) in history.slice(0, 10)"
            :key="index"
            class="w-full px-2.5 py-1.5 rounded text-xs text-left truncate transition-colors"
            :class="isDark ? 'hover:bg-slate-800 text-slate-400' : 'hover:bg-slate-100 text-slate-600'"
            :title="item.path"
            @click="$emit('select', item.path)"
          >
            {{ item.path }}
          </button>
        </div>
      </div>
    </div>
  </aside>
</template>

<script setup>
import { h } from 'vue'
import TreeNode from './TreeNode.vue'

const props = defineProps({
  treeData: { type: Array, default: () => [] },
  selectedPath: { type: String, default: '' },
  history: { type: Array, default: () => [] },
  collapsed: { type: Boolean, default: false },
  isDark: { type: Boolean, default: false },
})

const ComputerIcon = {
  render: () => h('svg', { class: 'w-4 h-4', fill: 'none', stroke: 'currentColor', viewBox: '0 0 24 24' }, [
    h('path', { 'stroke-linecap': 'round', 'stroke-linejoin': 'round', 'stroke-width': '2', d: '9.75 17L9 20l-1 1h8l-1-1-.75-3M3 13h18M5 17h14a2 2 0 002-2V5a2 2 0 00-2-2H5a2 2 0 00-2 2v10a2 2 0 002 2z' })
  ])
}

const HomeIcon = {
  render: () => h('svg', { class: 'w-4 h-4', fill: 'none', stroke: 'currentColor', viewBox: '0 0 24 24' }, [
    h('path', { 'stroke-linecap': 'round', 'stroke-linejoin': 'round', 'stroke-width': '2', d: 'M3 12l2-2m0 0l7-7 7 7M5 10v10a1 1 0 001 1h3m10-11l2 2m-2-2v10a1 1 0 01-1 1h-3m-6 0a1 1 0 001-1v-4a1 1 0 011-1h2a1 1 0 011 1v4a1 1 0 001 1m-6 0h6' })
  ])
}

const DownloadIcon = {
  render: () => h('svg', { class: 'w-4 h-4', fill: 'none', stroke: 'currentColor', viewBox: '0 0 24 24' }, [
    h('path', { 'stroke-linecap': 'round', 'stroke-linejoin': 'round', 'stroke-width': '2', d: '4 16v1a3 3 0 003 3h10a3 3 0 003-3v-1m-4-4l-4 4m0 0l-4-4m4 4V4' })
  ])
}

const DesktopIcon = {
  render: () => h('svg', { class: 'w-4 h-4', fill: 'none', stroke: 'currentColor', viewBox: '0 0 24 24' }, [
    h('path', { 'stroke-linecap': 'round', 'stroke-linejoin': 'round', 'stroke-width': '2', d: '9.75 17L9 20l-1 1h8l-1-1-.75-3M3 13h18M5 17h14a2 2 0 002-2V5a2 2 0 00-2-2H5a2 2 0 00-2 2v10a2 2 0 002 2z' })
  ])
}

const quickAccess = [
  { name: '此电脑', action: 'computer', icon: ComputerIcon },
  { name: '用户目录', action: 'home', icon: HomeIcon },
  { name: '下载', action: 'downloads', icon: DownloadIcon },
  { name: '桌面', action: 'desktop', icon: DesktopIcon },
]

defineEmits(['select', 'quick-access'])
</script>

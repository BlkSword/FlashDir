<template>
  <aside
    class="fd-sidebar"
    :class="collapsed ? 'fd-sidebar-collapsed' : ''"
  >
    <div class="fd-sidebar-inner">
      <div class="fd-side-section">
        <div class="fd-side-title">快速访问</div>
        <nav class="fd-side-list">
          <button
            v-for="item in quickAccess"
            :key="item.action"
            class="fd-side-item"
            @click="$emit('quick-access', item.action)"
          >
            <component :is="item.icon" />
            <span class="truncate">{{ item.name }}</span>
          </button>
        </nav>
      </div>

      <div class="fd-side-section">
        <div class="fd-side-title">目录树</div>
        <div v-if="!treeData.length" class="fd-side-empty">
          扫描后显示目录结构
        </div>
        <TreeNode
          v-for="node in treeData"
          :key="node.key"
          :node="node"
          :selected-path="selectedPath"
          @select="$emit('select', $event)"
        />
      </div>

      <div class="fd-side-section">
        <div class="fd-side-title">历史</div>
        <div v-if="!history.length" class="fd-side-empty">
          暂无历史记录
        </div>
        <div v-else class="fd-side-list">
          <button
            v-for="(item, index) in history.slice(0, 10)"
            :key="index"
            class="fd-side-item fd-side-history"
            :title="item.path"
            @click="$emit('select', item.path)"
          >
            <span class="truncate">{{ item.path }}</span>
          </button>
        </div>
      </div>
    </div>
  </aside>
</template>

<script setup>
import { h } from 'vue'
import TreeNode from './TreeNode.vue'

defineProps({
  treeData: { type: Array, default: () => [] },
  selectedPath: { type: String, default: '' },
  history: { type: Array, default: () => [] },
  collapsed: { type: Boolean, default: false },
})

const ComputerIcon = {
  render: () => h('svg', { fill: 'none', stroke: 'currentColor', viewBox: '0 0 24 24' }, [
    h('path', { 'stroke-linecap': 'round', 'stroke-linejoin': 'round', 'stroke-width': '2', d: 'M9.75 17L9 20l-1 1h8l-1-1-.75-3M3 13h18M5 17h14a2 2 0 002-2V5a2 2 0 00-2-2H5a2 2 0 00-2 2v10a2 2 0 002 2z' })
  ])
}

const HomeIcon = {
  render: () => h('svg', { fill: 'none', stroke: 'currentColor', viewBox: '0 0 24 24' }, [
    h('path', { 'stroke-linecap': 'round', 'stroke-linejoin': 'round', 'stroke-width': '2', d: 'M3 12l2-2m0 0l7-7 7 7M5 10v10a1 1 0 001 1h3m10-11l2 2m-2-2v10a1 1 0 01-1 1h-3m-6 0a1 1 0 001-1v-4a1 1 0 011-1h2a1 1 0 011 1v4a1 1 0 001 1m-6 0h6' })
  ])
}

const DownloadIcon = {
  render: () => h('svg', { fill: 'none', stroke: 'currentColor', viewBox: '0 0 24 24' }, [
    h('path', { 'stroke-linecap': 'round', 'stroke-linejoin': 'round', 'stroke-width': '2', d: 'M4 16v1a3 3 0 003 3h10a3 3 0 003-3v-1m-4-4l-4 4m0 0l-4-4m4 4V4' })
  ])
}

const DesktopIcon = {
  render: () => h('svg', { fill: 'none', stroke: 'currentColor', viewBox: '0 0 24 24' }, [
    h('path', { 'stroke-linecap': 'round', 'stroke-linejoin': 'round', 'stroke-width': '2', d: 'M9.75 17L9 20l-1 1h8l-1-1-.75-3M3 13h18M5 17h14a2 2 0 002-2V5a2 2 0 00-2-2H5a2 2 0 00-2 2v10a2 2 0 002 2z' })
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

<style scoped>
.fd-sidebar {
  grid-row: 2 / 3;
  grid-column: 1 / 2;
  background: var(--fd-bg-1);
  border-right: 1px solid var(--fd-border);
  overflow: hidden;
  transition: width 0.15s ease;
  width: 220px;
}
.fd-sidebar-collapsed { width: 0; }
.fd-sidebar-inner {
  width: 220px;
  height: 100%;
  overflow-y: auto;
  padding: 8px 0;
}
.fd-side-section {
  padding: 8px 0;
  border-bottom: 1px solid var(--fd-border);
}
.fd-side-title {
  padding: 0 12px 6px;
  font-size: 11px;
  color: var(--fd-text-2);
  font-weight: 700;
  text-transform: uppercase;
  letter-spacing: 0.5px;
}
.fd-side-list {
  display: flex;
  flex-direction: column;
}
.fd-side-item {
  display: flex;
  align-items: center;
  gap: 7px;
  padding: 5px 12px;
  border: none;
  background: transparent;
  color: var(--fd-text-1);
  font-size: 12px;
  text-align: left;
  cursor: pointer;
  width: 100%;
}
.fd-side-item:hover { background: var(--fd-bg-2); color: var(--fd-text-0); }
.fd-side-item svg { width: 14px; height: 14px; color: var(--fd-text-2); flex-shrink: 0; }
.fd-side-history { padding-left: 12px; color: var(--fd-text-2); }
.fd-side-empty {
  padding: 6px 12px;
  font-size: 12px;
  color: var(--fd-text-2);
}
</style>

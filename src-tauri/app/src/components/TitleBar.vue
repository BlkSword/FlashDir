<script setup>
import { computed } from 'vue'
import Icon from './Icon.vue'
import { formatSizeCompact, usageClass } from '../utils/format.js'

const props = defineProps({
  volumes: { type: Array, default: () => [] },
  currentDrive: { type: String, default: '' },
  theme: { type: String, default: 'auto' },
  treeVisible: { type: Boolean, default: true },
  inspVisible: { type: Boolean, default: true },
  dockVisible: { type: Boolean, default: true },
})
const emit = defineEmits(['pick-volume', 'command', 'cycle-theme', 'toggle-tree', 'toggle-insp', 'toggle-dock', 'settings'])

const themeLabel = computed(() => ({ auto: '跟随系统', dark: '深色', light: '浅色' }[props.theme] || '跟随系统'))

const usedRatio = (v) => (v.totalBytes ? (v.totalBytes - v.freeBytes) / v.totalBytes : 0)
const themeIcon = computed(() => (props.theme === 'light' ? 'sun' : props.theme === 'dark' ? 'moon' : 'auto'))
</script>

<template>
  <div class="titlebar" data-tauri-drag-region>
    <div class="brand"><Icon name="drive" :size="15" />FlashDir</div>

    <div class="volumes">
      <button
        v-for="v in volumes"
        :key="v.letter"
        class="vol"
        :class="{ on: v.letter === currentDrive }"
        :title="`${v.letter}: ${v.fs || '未知'}${v.label ? ' · ' + v.label : ''}\n已用 ${formatSizeCompact(v.totalBytes - v.freeBytes)} / ${formatSizeCompact(v.totalBytes)}`"
        @click="emit('pick-volume', v)"
      >
        <span class="mono">{{ v.letter }}:</span>
        <span class="bar">
          <i :class="usageClass(usedRatio(v))" :style="{ width: Math.min(100, usedRatio(v) * 100) + '%' }" />
        </span>
        <span class="mono">{{ formatSizeCompact(v.freeBytes) }} 可用</span>
      </button>
      <span v-if="!volumes.length" class="section-note">正在读取卷信息…</span>
    </div>

    <div class="drag" data-tauri-drag-region />

    <button class="cmd-trigger" title="文件搜索与命令（Ctrl+K）" @click="emit('command')">
      <Icon name="search" :size="13" />
      <span>搜索文件 / 命令</span>
      <span class="sp" />
      <kbd>Ctrl</kbd><kbd>K</kbd>
    </button>

    <button class="theme-btn" title="设置（MCP 端点、端口）" @click="emit('settings')">
      <Icon name="cog" :size="15" />
    </button>
    <button class="theme-btn" :title="`主题：${themeLabel}（点击切换）`" @click="emit('cycle-theme')">
      <Icon :name="themeIcon" :size="15" />
    </button>
    <button class="theme-btn" :title="treeVisible ? '隐藏目录树' : '显示目录树'" @click="emit('toggle-tree')">
      <Icon name="tree" :size="15" />
    </button>
    <button class="theme-btn" :title="inspVisible ? '隐藏检查器' : '显示检查器'" @click="emit('toggle-insp')">
      <Icon name="panel" :size="15" />
    </button>
    <button class="theme-btn" :title="dockVisible ? '隐藏洞察坞' : '显示洞察坞'" @click="emit('toggle-dock')">
      <Icon name="dock" :size="15" />
    </button>
  </div>
</template>

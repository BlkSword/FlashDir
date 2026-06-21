<template>
  <div>
    <button
      class="w-full flex items-center gap-1 px-2 py-1 rounded text-xs text-left transition-colors"
      :class="[
        isSelected
          ? (isDark ? 'bg-blue-900/40 text-blue-300' : 'bg-blue-50 text-blue-700')
          : (isDark ? 'hover:bg-slate-800 text-slate-300' : 'hover:bg-slate-100 text-slate-700')
      ]"
      :style="{ paddingLeft: `${level * 12 + 8}px` }"
      @click="handleClick"
    >
      <span
        v-if="hasChildren"
        class="w-3 h-3 flex items-center justify-center text-[10px] transition-transform"
        :class="expanded ? 'rotate-90' : ''"
        @click.stop="toggleExpand"
      >
        <svg class="w-3 h-3" fill="none" stroke="currentColor" viewBox="0 0 24 24">
          <path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M9 5l7 7-7 7" />
        </svg>
      </span>
      <span v-else class="w-3"></span>
      <svg
        class="w-4 h-4 shrink-0"
        :class="node.isLeaf ? (isDark ? 'text-slate-500' : 'text-slate-400') : 'text-blue-500'"
        fill="none"
        stroke="currentColor"
        viewBox="0 0 24 24"
      >
        <path
          v-if="node.isLeaf"
          stroke-linecap="round"
          stroke-linejoin="round"
          stroke-width="2"
          d="M9 12h6m-6 4h6m2 5H7a2 2 0 01-2-2V5a2 2 0 012-2h5.586a1 1 0 01.707.293l5.414 5.414a1 1 0 01.293.707V19a2 2 0 01-2 2z"
        />
        <path
          v-else
          stroke-linecap="round"
          stroke-linejoin="round"
          stroke-width="2"
          d="M3 7v10a2 2 0 002 2h14a2 2 0 002-2V9a2 2 0 00-2-2h-6l-2-2H5a2 2 0 00-2 2z"
        />
      </svg>
      <span class="truncate">{{ node.title }}</span>
      <span
        v-if="node.sizeFormatted"
        class="ml-auto text-2xs shrink-0"
        :class="isDark ? 'text-slate-500' : 'text-slate-400'"
      >
        {{ node.sizeFormatted }}
      </span>
    </button>

    <div v-if="expanded && hasChildren" class="mt-0.5">
      <TreeNode
        v-for="child in node.children"
        :key="child.key"
        :node="child"
        :selected-path="selectedPath"
        :is-dark="isDark"
        :level="level + 1"
        @select="$emit('select', $event)"
      />
    </div>
  </div>
</template>

<script setup>
import { ref, computed } from 'vue'

const props = defineProps({
  node: { type: Object, required: true },
  selectedPath: { type: String, default: '' },
  isDark: { type: Boolean, default: false },
  level: { type: Number, default: 0 },
})

const expanded = ref(props.level < 1)

const hasChildren = computed(() => props.node.children && props.node.children.length > 0)
const isSelected = computed(() => props.selectedPath && props.node.key && props.selectedPath.includes(props.node.key))

const toggleExpand = () => {
  expanded.value = !expanded.value
}

const handleClick = () => {
  if (hasChildren.value) {
    toggleExpand()
  }
  const fullPath = props.node.key
  if (fullPath) {
    emit('select', fullPath)
  }
}

const emit = defineEmits(['select'])
</script>

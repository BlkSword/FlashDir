<template>
  <div>
    <button
      class="fd-tree-item"
      :class="{ 'fd-tree-selected': isSelected }"
      :style="{ paddingLeft: `${level * 12 + 8}px` }"
      @click="handleClick"
    >
      <span
        v-if="hasChildren"
        class="fd-tree-toggle"
        :class="{ 'fd-tree-expanded': expanded }"
        @click.stop="toggleExpand"
      >
        <svg fill="none" stroke="currentColor" viewBox="0 0 24 24">
          <path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M9 5l7 7-7 7" />
        </svg>
      </span>
      <span v-else class="fd-tree-spacer"></span>
      <svg
        class="fd-tree-icon"
        :class="node.isLeaf ? 'fd-tree-file' : 'fd-tree-folder'"
        fill="currentColor"
        viewBox="0 0 24 24"
      >
        <path
          v-if="node.isLeaf"
          d="M9 12h6m-6 4h6m2 5H7a2 2 0 01-2-2V5a2 2 0 012-2h5.586a1 1 0 01.707.293l5.414 5.414a1 1 0 01.293.707V19a2 2 0 01-2 2z"
        />
        <path
          v-else
          d="M3 7v10a2 2 0 002 2h14a2 2 0 002-2V9a2 2 0 00-2-2h-6l-2-2H5a2 2 0 00-2 2z"
        />
      </svg>
      <span class="truncate">{{ node.title }}</span>
      <span v-if="node.sizeFormatted" class="fd-tree-size">{{ node.sizeFormatted }}</span>
    </button>

    <div v-if="expanded && hasChildren">
      <TreeNode
        v-for="child in node.children"
        :key="child.key"
        :node="child"
        :selected-path="selectedPath"
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

<style scoped>
.fd-tree-item {
  width: 100%;
  display: flex;
  align-items: center;
  gap: 4px;
  padding: 4px 8px;
  border: none;
  background: transparent;
  color: var(--fd-text-1);
  font-size: 12px;
  text-align: left;
  cursor: pointer;
}
.fd-tree-item:hover { background: var(--fd-bg-2); }
.fd-tree-selected { background: var(--fd-selected) !important; color: #fff; }
.fd-tree-toggle {
  width: 12px;
  height: 12px;
  display: inline-flex;
  align-items: center;
  justify-content: center;
  color: var(--fd-text-2);
  transition: transform 0.1s;
}
.fd-tree-toggle svg { width: 12px; height: 12px; }
.fd-tree-expanded { transform: rotate(90deg); }
.fd-tree-spacer { width: 12px; }
.fd-tree-icon { width: 14px; height: 14px; flex-shrink: 0; }
.fd-tree-folder { color: var(--fd-folder); }
.fd-tree-file { color: var(--fd-file); }
.fd-tree-size {
  margin-left: auto;
  font-size: 11px;
  color: var(--fd-text-2);
  flex-shrink: 0;
}
.fd-tree-selected .fd-tree-size { color: rgba(255,255,255,0.7); }
</style>

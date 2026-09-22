<script setup>
import { computed } from 'vue'
import { formatSizeCompact, heatLevel, heatVar } from '../utils/format.js'

const props = defineProps({
  items: { type: Array, default: () => [] },
  max: { type: Number, default: 12 },
  total: { type: Number, default: 0 },
})
const emit = defineEmits(['navigate'])

const cells = computed(() => {
  const list = [...props.items].filter((i) => i.size > 0).sort((a, b) => b.size - a.size)
  const top = list.slice(0, props.max)
  const restSum = list.slice(props.max).reduce((s, i) => s + i.size, 0)
  const out = top.map((i) => ({ ...i, rest: false }))
  if (restSum > 0) out.push({ path: '', name: `其他 ${list.length - props.max} 项`, size: restSum, rest: true })
  return out
})
const maxSize = computed(() => cells.value.reduce((m, c) => Math.max(m, c.size), 0))

// 用 flex 权重做近似 treemap：面积 ∝ 体积，行内换行
const rows = computed(() => {
  const list = cells.value
  if (!list.length) return []
  const total = list.reduce((s, c) => s + c.size, 0) || 1
  const rows = []
  let cur = []
  let curWeight = 0
  const target = 3 // 每行约 3 个块
  for (const c of list) {
    cur.push({ ...c, flex: Math.max(1, Math.round((c.size / total) * 12)) })
    curWeight++
    if (curWeight >= target) {
      rows.push(cur)
      cur = []
      curWeight = 0
    }
  }
  if (cur.length) rows.push(cur)
  return rows
})
</script>

<template>
  <div v-if="!cells.length" class="section-note">当前目录没有可展示的条目。</div>
  <div v-else style="display:flex;flex-direction:column;gap:2px;height:100%">
    <div v-for="(row, ri) in rows" :key="ri" style="display:flex;gap:2px;flex:1;min-height:0">
      <div
        v-for="c in row"
        :key="c.path || c.name"
        :style="{
          flex: c.flex,
          background: heatVar(heatLevel(c.size, maxSize)),
          cursor: c.rest ? 'default' : 'pointer',
        }"
        class="cell"
        :title="`${c.name} · ${formatSizeCompact(c.size)}`"
        @click="!c.rest && emit('navigate', c.path)"
      >
        <div class="n">{{ c.name }}</div>
        <b>{{ formatSizeCompact(c.size) }}</b>
      </div>
    </div>
  </div>
</template>

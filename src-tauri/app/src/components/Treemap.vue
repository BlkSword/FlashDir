<script setup>
import { computed, ref, onMounted, onUnmounted } from 'vue'
import { formatSizeCompact, heatLevel, heatVar } from '../utils/format.js'

const props = defineProps({
  items: { type: Array, default: () => [] },
  total: { type: Number, default: 0 },
  max: { type: Number, default: 14 },
})
const emit = defineEmits(['navigate'])

const box = ref(null)
const size = ref({ w: 800, h: 200 })
let ro = null

onMounted(() => {
  const measure = () => {
    const el = box.value
    if (el) size.value = { w: el.clientWidth || 800, h: el.clientHeight || 200 }
  }
  measure()
  if (typeof ResizeObserver !== 'undefined') {
    ro = new ResizeObserver(measure)
    if (box.value) ro.observe(box.value)
  }
  window.addEventListener('resize', measure)
  onUnmounted(() => {
    window.removeEventListener('resize', measure)
    ro?.disconnect()
  })
})

/** 参与布局的条目：前 max 项 + "其他" */
const entries = computed(() => {
  const list = props.items.filter((i) => i.size > 0).sort((a, b) => b.size - a.size)
  const top = list.slice(0, props.max)
  const rest = list.slice(props.max)
  const out = top.map((i) => ({ ...i, rest: false }))
  if (rest.length) {
    out.push({
      path: '',
      name: `其他 ${rest.length} 项`,
      size: rest.reduce((s, i) => s + i.size, 0),
      rest: true,
    })
  }
  return out
})

const totalSize = computed(() => entries.value.reduce((s, i) => s + i.size, 0) || props.total || 0)
const topShare = computed(() => {
  const top = entries.value.filter((e) => !e.rest)
  const t = totalSize.value || 1
  return Math.round((top.reduce((s, e) => s + e.size, 0) / t) * 100)
})

/**
 * squarified treemap（Bruls/Huizing/van Wijk）：
 * 沿矩形短边成行排列，使每个格子的长宽比尽量接近 1；面积严格 ∝ 体积。
 * 返回百分比坐标，便于用绝对定位渲染。
 */
function squarify(list) {
  const out = []
  const total = list.reduce((s, i) => s + i.size, 0)
  if (!list.length || total <= 0) return out
  const W = 100
  const H = 100
  const scale = (W * H) / total

  const rest = list.slice()
  let row = []
  let rx = 0
  let ry = 0
  let rw = W
  let rh = H

  const area = (r) => r.reduce((s, i) => s + i.size, 0) * scale
  const worst = (r, side) => {
    const sum = area(r)
    if (sum <= 0 || side <= 0) return Infinity
    const maxA = Math.max(...r.map((i) => i.size * scale))
    const minA = Math.min(...r.map((i) => i.size * scale))
    if (minA <= 0) return Infinity
    return Math.max((side * side * maxA) / (sum * sum), (sum * sum) / (side * side * minA))
  }
  const place = (r) => {
    const sum = area(r)
    if (sum <= 0) return
    if (rw >= rh) {
      const colW = sum / rh
      let oy = ry
      for (const it of r) {
        const cellH = (it.size * scale) / colW
        out.push({ ...it, x: rx, y: oy, w: colW, h: cellH })
        oy += cellH
      }
      rx += colW
      rw -= colW
    } else {
      const rowH = sum / rw
      let ox = rx
      for (const it of r) {
        const cellW = (it.size * scale) / rowH
        out.push({ ...it, x: ox, y: ry, w: cellW, h: rowH })
        ox += cellW
      }
      ry += rowH
      rh -= rowH
    }
  }

  while (rest.length) {
    const side = Math.min(rw, rh)
    const cand = row.concat(rest[0])
    if (!row.length || worst(cand, side) <= worst(row, side)) {
      row.push(rest.shift())
    } else {
      place(row)
      row = []
    }
  }
  if (row.length) place(row)
  return out
}

const cells = computed(() => squarify(entries.value))
const maxCell = computed(() => cells.value.reduce((m, c) => Math.max(m, c.size), 0) || 1)

const labelMode = (c) => {
  const pxW = (c.w / 100) * size.value.w
  const pxH = (c.h / 100) * size.value.h
  if (pxW >= 74 && pxH >= 34) return 'full'
  if (pxW >= 46 && pxH >= 20) return 'size'
  return 'none'
}
</script>

<template>
  <div ref="box" class="tm-box">
    <div v-if="!cells.length" class="section-note">当前目录没有可展示的条目。</div>
    <template v-else>
      <div
        v-for="c in cells"
        :key="c.path || c.name"
        class="cell"
        :style="{
          left: c.x + '%',
          top: c.y + '%',
          width: c.w + '%',
          height: c.h + '%',
          background: heatVar(heatLevel(c.size, maxCell)),
          cursor: c.rest ? 'default' : 'pointer',
        }"
        :title="`${c.name}\n${formatSizeCompact(c.size)}${c.rest ? '' : '\n点击进入'}`"
        @click="!c.rest && emit('navigate', c.path)"
      >
        <template v-if="labelMode(c) === 'full'">
          <div class="n">{{ c.name }}</div>
          <b>{{ formatSizeCompact(c.size) }}</b>
        </template>
        <template v-else-if="labelMode(c) === 'size'">
          <b>{{ formatSizeCompact(c.size) }}</b>
        </template>
      </div>
      <div class="tm-legend">
        面积 ∝ 体积 · 前 {{ cells.filter((c) => !c.rest).length }} 项占 {{ topShare }}%
      </div>
    </template>
  </div>
</template>

<style scoped>
.tm-box {
  position: relative;
  width: 100%;
  height: 100%;
  min-height: 90px;
  background: var(--bg-0);
  border: 1px solid var(--bd-0);
  border-radius: var(--r);
  overflow: hidden;
}
.cell {
  position: absolute;
  padding: 3px 5px;
  overflow: hidden;
  color: var(--tx-0);
  border: 1px solid color-mix(in srgb, var(--bg-0) 70%, transparent);
  box-sizing: border-box;
}
.cell:hover { outline: 1px solid var(--accent); outline-offset: -1px; }
.cell .n {
  font-size: 11px;
  line-height: 1.25;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.cell b { display: block; font: 10px var(--font-mono); opacity: 0.85; font-weight: 400; }
.tm-legend {
  position: absolute;
  right: 5px;
  bottom: 3px;
  font-size: 10px;
  color: var(--tx-2);
  background: color-mix(in srgb, var(--bg-0) 75%, transparent);
  padding: 0 4px;
  border-radius: 2px;
  pointer-events: none;
}
</style>

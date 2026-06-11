<template>
  <div class="treemap-container" ref="containerRef">
    <div class="treemap-toolbar">
      <div class="treemap-breadcrumb">
        <a-button
          type="link"
          size="small"
          :disabled="!canGoUp"
          @click="goUp"
        >⬆ 上级</a-button>
        <span class="treemap-current-dir">{{ currentDir || '(根目录)' }}</span>
        <span class="treemap-dir-stats" v-if="currentItems.length > 0">
          {{ currentItems.length }} 项 · {{ formatSize(currentTotalSize) }}
        </span>
      </div>
      <div class="treemap-legend">
        <span
          v-for="ext in topExtensions"
          :key="ext.name"
          class="treemap-legend-item"
        >
          <span class="legend-color" :style="{ background: ext.color }"></span>
          {{ ext.name }}
        </span>
      </div>
    </div>
    <div class="treemap-canvas-wrapper" ref="canvasWrapperRef">
      <canvas
        ref="canvasRef"
        @mousemove="handleMouseMove"
        @mouseleave="handleMouseLeave"
        @click="handleClick"
      ></canvas>
      <div
        class="treemap-tooltip"
        v-if="tooltip.visible"
        :style="{ left: tooltip.x + 'px', top: tooltip.y + 'px' }"
      >
        <div class="tooltip-name">{{ tooltip.name }}</div>
        <div class="tooltip-size">{{ tooltip.size }}</div>
      </div>
    </div>
    <div class="treemap-empty" v-if="currentItems.length === 0">
      选择要扫描的目录以查看 Treemap
    </div>
  </div>
</template>

<script setup>
import { ref, computed, watch, onMounted, onUnmounted, nextTick } from 'vue'
import { formatSize } from '../utils/format.js'

const props = defineProps({
  items: { type: Array, default: () => [] },
  totalSize: { type: Number, default: 0 }
})

const emit = defineEmits(['drilldown'])

const containerRef = ref(null)
const canvasWrapperRef = ref(null)
const canvasRef = ref(null)

// 导航状态
const navStack = ref([]) // [{ dirPath, items, totalSize }]
const currentDir = ref('')
const currentItems = ref([])
const currentTotalSize = ref(0)

const canGoUp = computed(() => navStack.value.length > 0)

// 鼠标提示
const tooltip = ref({ visible: false, x: 0, y: 0, name: '', size: '' })

// 缓存的布局数据 (用于 hover/click 检测)
let layoutCache = []

// 文件扩展名颜色映射
const extColors = {
  // 文档
  pdf: '#f5222d', doc: '#2f54eb', docx: '#2f54eb', xls: '#52c41a',
  xlsx: '#52c41a', ppt: '#fa8c16', pptx: '#fa8c16', txt: '#8c8c8c',
  md: '#1890ff',
  // 图片
  jpg: '#eb2f96', jpeg: '#eb2f96', png: '#722ed1', gif: '#13c2c2',
  svg: '#faad14', webp: '#a0d911', bmp: '#595959',
  // 视频
  mp4: '#cf1322', mkv: '#d4380d', avi: '#d46b08', mov: '#d48806',
  webm: '#d4b106', flv: '#5b8c00',
  // 音频
  mp3: '#08979c', flac: '#006d75', wav: '#00474f', aac: '#237804',
  ogg: '#3f8600',
  // 代码
  js: '#f0db4f', ts: '#3178c6', jsx: '#61dafb', tsx: '#3178c6',
  vue: '#42b883', py: '#3776ab', rs: '#dea584', go: '#00add8',
  java: '#b07219', rb: '#cc342d', php: '#777bb4', cpp: '#f34b7d',
  c: '#555555', h: '#555555', cs: '#178600', swift: '#f05138',
  kt: '#7f52ff',
  // 配置/数据
  json: '#f5a623', yaml: '#f5a623', yml: '#f5a623', xml: '#f5a623',
  toml: '#f5a623', ini: '#8c8c8c', cfg: '#8c8c8c', env: '#8b4513',
  lock: '#999999',
  // 压缩
  zip: '#8b7355', rar: '#8b7355', '7z': '#8b7355', gz: '#8b7355',
  tar: '#8b7355', bz2: '#8b7355', xz: '#8b7355',
  // 可执行文件
  exe: '#434343', dll: '#595959', so: '#262626', dylib: '#262626',
  app: '#1a1a1a', msi: '#434343',
  // 磁盘映像
  iso: '#fa8c16', vhdx: '#d4380d', vmdk: '#d4380d', img: '#d4380d',
  // 数据库
  db: '#003a8c', sqlite: '#003a8c', sqlite3: '#003a8c', mdb: '#003a8c',
}

const extColorFallbacks = [
  '#1890ff', '#13c2c2', '#52c41a', '#faad14', '#f5222d', '#722ed1',
  '#eb2f96', '#fa8c16', '#a0d911', '#2f54eb'
]

const getExtColor = (name) => {
  const ext = name.split('.').pop()?.toLowerCase() || ''
  return extColors[ext] || extColorFallbacks[Math.abs(hashCode(ext)) % extColorFallbacks.length]
}

const hashCode = (str) => {
  let hash = 0
  for (let i = 0; i < str.length; i++) {
    hash = ((hash << 5) - hash) + str.charCodeAt(i)
    hash |= 0
  }
  return hash
}

const topExtensions = computed(() => {
  const counts = {}
  for (const item of currentItems.value) {
    if (item.isDir) continue
    const ext = item.name.split('.').pop()?.toLowerCase() || '无扩展名'
    counts[ext] = (counts[ext] || 0) + 1
  }
  return Object.entries(counts)
    .sort((a, b) => b[1] - a[1])
    .slice(0, 8)
    .map(([name]) => ({ name, color: extColors[name] || extColorFallbacks[Math.abs(hashCode(name)) % extColorFallbacks.length] }))
})

// 当 items 变化时，重置到根
watch(() => props.items, (newItems) => {
  if (newItems && newItems.length > 0) {
    navStack.value = []
    currentDir.value = ''
    currentItems.value = newItems
    currentTotalSize.value = props.totalSize
    nextTick(() => render())
  } else {
    currentItems.value = []
    layoutCache = []
    nextTick(() => clearCanvas())
  }
}, { immediate: true })

// ─── Squarified Treemap 算法 ──────────────────────────────

const render = () => {
  if (!canvasRef.value || !canvasWrapperRef.value) return

  const canvas = canvasRef.value
  const wrapper = canvasWrapperRef.value
  const dpr = window.devicePixelRatio || 1
  const rect = wrapper.getBoundingClientRect()

  canvas.width = rect.width * dpr
  canvas.height = rect.height * dpr
  canvas.style.width = rect.width + 'px'
  canvas.style.height = rect.height + 'px'

  const ctx = canvas.getContext('2d')
  ctx.scale(dpr, dpr)
  ctx.clearRect(0, 0, rect.width, rect.height)

  const items = currentItems.value
  if (items.length === 0) return

  // 构建布局数据
  const layoutItems = items.map((item, idx) => ({
    idx,
    name: item.name,
    path: item.path,
    size: item.size || 1, // 最小 1 避免除零
    isDir: item.isDir,
    color: item.isDir ? '#d9d9d9' : getExtColor(item.name)
  }))

  // 按 size 降序
  layoutItems.sort((a, b) => b.size - a.size)

  // Recursive squarified treemap
  layoutCache = []
  squarify(layoutItems, 0, 0, rect.width, rect.height, ctx)

  // 绘制标签
  for (const cell of layoutCache) {
    drawCellLabel(ctx, cell)
  }
}

const squarify = (items, x, y, w, h, ctx) => {
  if (items.length === 0) return
  if (items.length === 1) {
    layoutAndDraw(items, x, y, w, h, ctx)
    return
  }

  const total = items.reduce((s, i) => s + i.size, 0)
  if (total === 0) return

  // 用最短边策略：保持较好的长宽比
  const area = w * h
  let row = []
  let rowArea = 0
  let bestAspect = Infinity
  let bestSplit = 0

  for (let i = 0; i < items.length; i++) {
    const itemArea = (items[i].size / total) * area
    const testRow = [...row, items[i]]
    const testRowArea = rowArea + itemArea
    const aspect = worstAspectRatio(testRow, testRowArea, w, h)
    if (aspect < bestAspect) {
      bestAspect = aspect
      row = testRow
      rowArea = testRowArea
      bestSplit = i
    }
  }

  // 如果加起来还没到合理比例，继续加
  if (bestSplit < items.length - 1 && bestAspect > 4) {
    row = items
    rowArea = area
  }

  // 布局当前行
  const remaining = items.slice(row.length)
  if (w >= h) {
    const rowWidth = rowArea / h
    layoutAndDrawSlice(row, x, y, rowWidth, h, true, ctx)
    if (remaining.length > 0) {
      squarify(remaining, x + rowWidth, y, w - rowWidth, h, ctx)
    }
  } else {
    const rowHeight = rowArea / w
    layoutAndDrawSlice(row, x, y, w, rowHeight, false, ctx)
    if (remaining.length > 0) {
      squarify(remaining, x, y + rowHeight, w, h - rowHeight, ctx)
    }
  }
}

const worstAspectRatio = (items, totalArea, w, h) => {
  const side = totalArea / Math.min(w, h)
  let worst = 0
  for (const item of items) {
    const area = (item.size / items.reduce((s, i) => s + i.size, 0)) * totalArea
    const aspect = Math.max(side * side / area, area / (side * side))
    worst = Math.max(worst, aspect)
  }
  return worst
}

const layoutAndDraw = (items, x, y, w, h, ctx) => {
  const total = items.reduce((s, i) => s + i.size, 0)
  if (total === 0) return

  const isVertical = w >= h
  let pos = isVertical ? x : y

  for (const item of items) {
    const frac = item.size / total
    let cellW, cellH
    if (isVertical) {
      cellW = w * frac
      cellH = h
    } else {
      cellW = w
      cellH = h * frac
    }

    const cell = {
      x: isVertical ? pos : x,
      y: isVertical ? y : pos,
      w: cellW,
      h: cellH,
      item
    }
    layoutCache.push(cell)
    drawCell(ctx, cell)
    pos += isVertical ? cellW : cellH
  }
}

const layoutAndDrawSlice = (items, x, y, w, h, isVertical, ctx) => {
  layoutAndDraw(items, x, y, w, h, ctx)
}

const drawCell = (ctx, cell) => {
  const { x, y, w, h, item } = cell
  const padding = 1

  ctx.fillStyle = item.color
  ctx.fillRect(x + padding, y + padding, Math.max(0, w - padding * 2), Math.max(0, h - padding * 2))

  // 目录用更浅的颜色和虚线边框
  if (item.isDir) {
    ctx.strokeStyle = '#bfbfbf'
    ctx.lineWidth = 0.5
    ctx.setLineDash([2, 2])
    ctx.strokeRect(x + padding, y + padding, Math.max(0, w - padding * 2), Math.max(0, h - padding * 2))
    ctx.setLineDash([])
  }
}

const drawCellLabel = (ctx, cell) => {
  const { x, y, w, h, item } = cell
  if (w < 30 || h < 16) return // 太小不画文字

  const maxTextWidth = w - 6
  if (maxTextWidth < 20) return

  ctx.save()
  ctx.fillStyle = item.isDir ? '#595959' : getContrastColor(item.color)
  ctx.font = `${Math.min(11, Math.max(8, h / 5))}px -apple-system, sans-serif`

  // 截断文本
  let text = item.name
  while (ctx.measureText(text).width > maxTextWidth && text.length > 3) {
    text = text.substring(0, text.length - 4) + '...'
  }

  ctx.fillText(text, x + 4, y + h / 2 + 4)
  ctx.restore()
}

const getContrastColor = (hex) => {
  // 计算亮度，深色背景用白字，浅色背景用黑字
  const r = parseInt(hex.slice(1, 3), 16)
  const g = parseInt(hex.slice(3, 5), 16)
  const b = parseInt(hex.slice(5, 7), 16)
  const luminance = (0.299 * r + 0.587 * g + 0.114 * b) / 255
  return luminance > 0.6 ? '#262626' : '#ffffff'
}

const clearCanvas = () => {
  if (!canvasRef.value) return
  const ctx = canvasRef.value.getContext('2d')
  ctx.clearRect(0, 0, canvasRef.value.width, canvasRef.value.height)
  layoutCache = []
}

// ─── 交互 ─────────────────────────────────────────────────

const findCellAtPos = (x, y) => {
  for (const cell of layoutCache) {
    if (x >= cell.x && x < cell.x + cell.w && y >= cell.y && y < cell.y + cell.h) {
      return cell
    }
  }
  return null
}

const handleMouseMove = (e) => {
  if (!canvasRef.value) return
  const rect = canvasRef.value.getBoundingClientRect()
  const mx = e.clientX - rect.left
  const my = e.clientY - rect.top

  const cell = findCellAtPos(mx, my)
  if (cell) {
    const item = cell.item
    tooltip.value = {
      visible: true,
      x: e.clientX - rect.left + 12,
      y: e.clientY - rect.top + 12,
      name: item.isDir ? `📁 ${item.name}` : `📄 ${item.name}`,
      size: formatSize(item.size)
    }
  } else {
    tooltip.value.visible = false
  }
}

const handleMouseLeave = () => {
  tooltip.value.visible = false
}

const handleClick = (e) => {
  if (!canvasRef.value) return
  const rect = canvasRef.value.getBoundingClientRect()
  const mx = e.clientX - rect.left
  const my = e.clientY - rect.top

  const cell = findCellAtPos(mx, my)
  if (!cell || !cell.item.isDir) return

  // 钻入目录
  const dirPath = cell.item.path
  const dirItems = currentItems.value.filter(item =>
    item.path.startsWith(dirPath + '/') && item.path !== dirPath
  )

  if (dirItems.length === 0) {
    // 空目录，不钻入
    return
  }

  // 保存当前状态到栈
  navStack.value.push({
    dirPath: currentDir.value,
    items: currentItems.value,
    totalSize: currentTotalSize.value
  })

  currentDir.value = dirPath
  currentItems.value = dirItems
  currentTotalSize.value = dirItems.reduce((s, i) => s + (i.isDir ? 0 : i.size), 0)

  nextTick(() => render())
}

const goUp = () => {
  if (navStack.value.length === 0) return
  const prev = navStack.value.pop()
  currentDir.value = prev.dirPath
  currentItems.value = prev.items
  currentTotalSize.value = prev.totalSize
  nextTick(() => render())
}

// ─── Resize 处理 ──────────────────────────────────────────

let resizeObserver = null

onMounted(() => {
  if (canvasWrapperRef.value) {
    resizeObserver = new ResizeObserver(() => {
      if (currentItems.value.length > 0) {
        render()
      }
    })
    resizeObserver.observe(canvasWrapperRef.value)
  }
})

onUnmounted(() => {
  if (resizeObserver) {
    resizeObserver.disconnect()
  }
})

// 监听窗口 resize
watch(() => props.items, () => {
  if (currentItems.value.length === 0 && props.items.length > 0) {
    currentItems.value = props.items
    currentTotalSize.value = props.totalSize
    nextTick(() => render())
  }
}, { immediate: true })
</script>

<style scoped>
.treemap-container {
  display: flex;
  flex-direction: column;
  height: 100%;
  overflow: hidden;
}

.treemap-toolbar {
  padding: 8px 12px;
  background: white;
  border-bottom: 1px solid #f0f0f0;
}

.treemap-breadcrumb {
  display: flex;
  align-items: center;
  gap: 4px;
  margin-bottom: 6px;
}

.treemap-current-dir {
  font-size: 12px;
  font-weight: 600;
  color: #262626;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.treemap-dir-stats {
  font-size: 11px;
  color: #8c8c8c;
  margin-left: 8px;
}

.treemap-legend {
  display: flex;
  flex-wrap: wrap;
  gap: 6px;
}

.treemap-legend-item {
  font-size: 10px;
  color: #595959;
  display: flex;
  align-items: center;
  gap: 3px;
}

.legend-color {
  width: 10px;
  height: 10px;
  border-radius: 2px;
  display: inline-block;
}

.treemap-canvas-wrapper {
  flex: 1;
  overflow: hidden;
  position: relative;
  cursor: crosshair;
}

.treemap-canvas-wrapper canvas {
  display: block;
}

.treemap-tooltip {
  position: absolute;
  background: rgba(0, 0, 0, 0.8);
  color: white;
  padding: 6px 10px;
  border-radius: 4px;
  font-size: 12px;
  pointer-events: none;
  z-index: 10;
  max-width: 250px;
  white-space: nowrap;
}

.tooltip-name {
  font-weight: 500;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.tooltip-size {
  color: #bfbfbf;
  font-size: 11px;
  margin-top: 2px;
}

.treemap-empty {
  flex: 1;
  display: flex;
  align-items: center;
  justify-content: center;
  color: #bfbfbf;
  font-size: 13px;
}
</style>

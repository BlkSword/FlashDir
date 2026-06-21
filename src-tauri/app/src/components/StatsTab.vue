<template>
  <div class="space-y-4">
    <div>
      <div
        class="text-2xs font-semibold uppercase tracking-wider mb-2"
        :class="isDark ? 'text-slate-500' : 'text-slate-500'"
      >
        总览
      </div>
      <div class="grid grid-cols-2 gap-2">
        <div
          class="border rounded p-2"
          :class="isDark ? 'bg-slate-800 border-slate-700' : 'bg-slate-50 border-slate-200'"
        >
          <div class="text-2xs" :class="isDark ? 'text-slate-400' : 'text-slate-500'">总大小</div>
          <div class="text-base font-semibold mono" :class="isDark ? 'text-slate-200' : 'text-slate-800'">{{ formatSize(totalSize) }}</div>
        </div>
        <div
          class="border rounded p-2"
          :class="isDark ? 'bg-slate-800 border-slate-700' : 'bg-slate-50 border-slate-200'"
        >
          <div class="text-2xs" :class="isDark ? 'text-slate-400' : 'text-slate-500'">文件数</div>
          <div class="text-base font-semibold mono" :class="isDark ? 'text-slate-200' : 'text-slate-800'">{{ fileCount }}</div>
        </div>
        <div
          class="border rounded p-2"
          :class="isDark ? 'bg-slate-800 border-slate-700' : 'bg-slate-50 border-slate-200'"
        >
          <div class="text-2xs" :class="isDark ? 'text-slate-400' : 'text-slate-500'">目录数</div>
          <div class="text-base font-semibold mono" :class="isDark ? 'text-slate-200' : 'text-slate-800'">{{ dirCount }}</div>
        </div>
        <div
          class="border rounded p-2"
          :class="isDark ? 'bg-slate-800 border-slate-700' : 'bg-slate-50 border-slate-200'"
        >
          <div class="text-2xs" :class="isDark ? 'text-slate-400' : 'text-slate-500'">扫描耗时</div>
          <div class="text-base font-semibold mono" :class="isDark ? 'text-slate-200' : 'text-slate-800'">{{ scanTime.toFixed(2) }}s</div>
        </div>
      </div>
    </div>

    <div>
      <div
        class="text-2xs font-semibold uppercase tracking-wider mb-2"
        :class="isDark ? 'text-slate-500' : 'text-slate-500'"
      >
        扩展名分布
      </div>
      <div class="space-y-1.5">
        <div v-for="(ext, index) in extStats" :key="index">
          <div class="flex items-center justify-between text-xs">
            <span :class="isDark ? 'text-slate-300' : 'text-slate-600'">{{ ext.name }}</span>
            <span class="mono" :class="isDark ? 'text-slate-400' : 'text-slate-500'">{{ ext.sizeFormatted }}</span>
          </div>
          <div class="w-full h-1.5 rounded-full overflow-hidden" :class="isDark ? 'bg-slate-700' : 'bg-slate-200'">
            <div
              class="h-full rounded-full"
              :class="ext.color"
              :style="{ width: ext.percent + '%' }"
            ></div>
          </div>
        </div>
      </div>
    </div>

    <div>
      <div
        class="text-2xs font-semibold uppercase tracking-wider mb-2"
        :class="isDark ? 'text-slate-500' : 'text-slate-500'"
      >
        Top 5 大文件
      </div>
      <div class="space-y-1">
        <div v-for="(file, index) in topFiles" :key="index" class="flex justify-between text-xs">
          <span class="truncate w-32" :class="isDark ? 'text-slate-300' : 'text-slate-700'">{{ file.name }}</span>
          <span class="mono" :class="isDark ? 'text-slate-400' : 'text-slate-500'">{{ file.sizeFormatted }}</span>
        </div>
      </div>
    </div>
  </div>
</template>

<script setup>
import { computed } from 'vue'

const props = defineProps({
  items: { type: Array, default: () => [] },
  totalSize: { type: Number, default: 0 },
  scanTime: { type: Number, default: 0 },
  isDark: { type: Boolean, default: false },
})

const fileCount = computed(() => props.items.filter(i => !i.isDir).length)
const dirCount = computed(() => props.items.filter(i => i.isDir).length)

const topFiles = computed(() => {
  return props.items
    .filter(i => !i.isDir)
    .sort((a, b) => b.size - a.size)
    .slice(0, 5)
    .map(i => ({ ...i, sizeFormatted: formatSize(i.size) }))
})

const extStats = computed(() => {
  const map = new Map()
  for (const item of props.items) {
    if (item.isDir) continue
    const ext = getExt(item.name)
    const key = ext || '无扩展名'
    const cur = map.get(key) || { size: 0, count: 0 }
    cur.size += item.size
    cur.count++
    map.set(key, cur)
  }

  const colors = ['bg-blue-500', 'bg-yellow-500', 'bg-green-500', 'bg-purple-500', 'bg-slate-400']
  return Array.from(map.entries())
    .sort((a, b) => b[1].size - a[1].size)
    .slice(0, 5)
    .map(([name, data], idx) => ({
      name,
      sizeFormatted: formatSize(data.size),
      percent: props.totalSize ? Math.max(1, (data.size / props.totalSize) * 100) : 0,
      color: colors[idx % colors.length],
    }))
})

const getExt = (name) => {
  const dot = name.lastIndexOf('.')
  return dot > 0 ? name.slice(dot + 1).toLowerCase() : ''
}

const formatSize = (bytes) => {
  if (bytes === 0) return '0 B'
  const units = ['B', 'KB', 'MB', 'GB', 'TB']
  const i = Math.floor(Math.log(bytes) / Math.log(1024))
  return (bytes / Math.pow(1024, i)).toFixed(i === 0 ? 0 : 2) + ' ' + units[i]
}
</script>

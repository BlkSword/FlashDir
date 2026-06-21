<template>
  <div class="flex-1 flex flex-col min-h-0">
    <!-- Header info -->
    <div
      class="flex items-center justify-between px-3 py-2 border-b"
      :class="isDark ? 'bg-slate-900/50 border-slate-700' : 'bg-slate-50 border-slate-200'"
    >
      <div class="text-xs" :class="isDark ? 'text-slate-400' : 'text-slate-500'">
        {{ currentPath || '未选择目录' }}
      </div>
      <div class="flex items-center gap-2">
        <select
          :value="sortKey"
          class="text-xs rounded border px-2 py-1 outline-none transition-colors"
          :class="isDark ? 'bg-slate-800 border-slate-600 text-slate-200' : 'bg-white border-slate-300 text-slate-700'"
          @change="handleSortChange"
        >
          <option value="size-desc">大小降序</option>
          <option value="size-asc">大小升序</option>
          <option value="name-asc">名称升序</option>
          <option value="name-desc">名称降序</option>
          <option value="mtime-desc">修改时间降序</option>
        </select>
      </div>
    </div>

    <!-- Table -->
    <div class="flex-1 overflow-auto">
      <table class="w-full text-left border-collapse">
        <thead
          class="sticky top-0 z-10"
          :class="isDark ? 'bg-slate-800 text-slate-400' : 'bg-slate-50 text-slate-500'"
        >
          <tr class="text-xs border-b" :class="isDark ? 'border-slate-700' : 'border-slate-200'">
            <th class="px-3 py-2 font-medium w-10 cursor-pointer" @click="handleSort('name')">
              <div class="flex items-center gap-1">
                名称
                <SortIcon :active="sortConfig.column === 'name'" :direction="sortConfig.direction" />
              </div>
            </th>
            <th class="px-3 py-2 font-medium w-20 cursor-pointer" @click="handleSort('size')">
              <div class="flex items-center gap-1">
                大小
                <SortIcon :active="sortConfig.column === 'size'" :direction="sortConfig.direction" />
              </div>
            </th>
            <th class="px-3 py-2 font-medium w-24 text-right">占比</th>
            <th class="px-3 py-2 font-medium w-36">修改时间</th>
          </tr>
        </thead>
        <tbody class="text-xs">
          <tr v-if="loading && items.length === 0">
            <td colspan="4" class="px-3 py-8 text-center" :class="isDark ? 'text-slate-500' : 'text-slate-400'">
              <div class="flex items-center justify-center gap-2">
                <svg class="w-4 h-4 animate-spin" fill="none" viewBox="0 0 24 24">
                  <circle class="opacity-25" cx="12" cy="12" r="10" stroke="currentColor" stroke-width="4"></circle>
                  <path class="opacity-75" fill="currentColor" d="M4 12a8 8 0 018-8V0C5.373 0 0 5.373 0 12h4zm2 5.291A7.962 7.962 0 014 12H0c0 3.042 1.135 5.824 3 7.938l3-2.647z"></path>
                </svg>
                扫描中...
              </div>
            </td>
          </tr>
          <tr v-else-if="items.length === 0">
            <td colspan="4" class="px-3 py-8 text-center" :class="isDark ? 'text-slate-500' : 'text-slate-400'">
              选择目录并开始扫描
            </td>
          </tr>
          <tr
            v-for="(item, index) in items"
            :key="index"
            class="border-b cursor-pointer transition-colors"
            :class="[
              isDark ? 'border-slate-800 hover:bg-slate-800/50' : 'border-slate-100 hover:bg-slate-50',
              item.isDir ? '' : ''
            ]"
            @click="$emit('select', item)"
          >
            <td class="px-3 py-1.5">
              <div class="flex items-center gap-2">
                <svg
                  class="w-4 h-4 shrink-0"
                  :class="item.isDir ? 'text-blue-500' : (isDark ? 'text-slate-500' : 'text-slate-400')"
                  fill="none"
                  stroke="currentColor"
                  viewBox="0 0 24 24"
                >
                  <path
                    v-if="item.isDir"
                    stroke-linecap="round"
                    stroke-linejoin="round"
                    stroke-width="2"
                    d="M3 7v10a2 2 0 002 2h14a2 2 0 002-2V9a2 2 0 00-2-2h-6l-2-2H5a2 2 0 00-2 2z"
                  />
                  <path
                    v-else
                    stroke-linecap="round"
                    stroke-linejoin="round"
                    stroke-width="2"
                    d="M9 12h6m-6 4h6m2 5H7a2 2 0 01-2-2V5a2 2 0 012-2h5.586a1 1 0 01.707.293l5.414 5.414a1 1 0 01.293.707V19a2 2 0 01-2 2z"
                  />
                </svg>
                <span
                  class="font-medium truncate"
                  :class="item.isDir ? (isDark ? 'text-slate-300' : 'text-slate-700') : (isDark ? 'text-slate-400' : 'text-slate-600')"
                >
                  {{ item.name }}
                </span>
              </div>
            </td>
            <td
              class="px-3 py-1.5 text-right mono font-medium"
              :class="isDark ? 'text-slate-300' : 'text-slate-700'"
            >
              {{ item.sizeFormatted || formatSize(item.size) }}
            </td>
            <td class="px-3 py-1.5 text-right">
              <div class="flex items-center justify-end gap-2">
                <span class="text-2xs" :class="isDark ? 'text-slate-500' : 'text-slate-500'">{{ getPercent(item.size) }}</span>
                <div class="w-16 h-1.5 rounded-full overflow-hidden" :class="isDark ? 'bg-slate-700' : 'bg-slate-200'">
                  <div
                    class="h-full rounded-full"
                    :class="item.isDir ? 'bg-blue-500' : 'bg-slate-400'"
                    :style="{ width: getBarWidth(item.size) }"
                  ></div>
                </div>
              </div>
            </td>
            <td class="px-3 py-1.5 text-2xs" :class="isDark ? 'text-slate-500' : 'text-slate-500'">
              {{ item.mtime ? formatTime(item.mtime * 1000) : '-' }}
            </td>
          </tr>
        </tbody>
      </table>
    </div>

    <!-- Pagination -->
    <div
      v-if="totalItems > pageSize"
      class="px-3 py-2 border-t flex items-center justify-between shrink-0"
      :class="isDark ? 'bg-slate-900/50 border-slate-700' : 'bg-slate-50 border-slate-200'"
    >
      <span class="text-xs" :class="isDark ? 'text-slate-400' : 'text-slate-500'">
        共 {{ totalItems.toLocaleString() }} 项
      </span>
      <div class="flex items-center gap-2">
        <a-pagination
          :current="currentPage"
          :page-size="pageSize"
          :total="totalItems"
          :page-size-options="['50', '100', '200', '500', '1000']"
          show-size-changer
          size="small"
          @change="$emit('page-change', $event)"
          @showSizeChange="(current, size) => $emit('size-change', current, size)"
        />
      </div>
    </div>
  </div>
</template>

<script setup>
import { computed } from 'vue'
import SortIcon from './SortIcon.vue'

const props = defineProps({
  items: { type: Array, default: () => [] },
  loading: { type: Boolean, default: false },
  totalSize: { type: Number, default: 0 },
  currentPath: { type: String, default: '' },
  sortConfig: { type: Object, default: () => ({ column: 'size', direction: 'desc' }) },
  currentPage: { type: Number, default: 1 },
  pageSize: { type: Number, default: 100 },
  totalItems: { type: Number, default: 0 },
  isDark: { type: Boolean, default: false },
})

const emit = defineEmits(['sort', 'select', 'page-change', 'size-change'])

const sortKey = computed(() => `${props.sortConfig.column}-${props.sortConfig.direction}`)

const handleSort = (column) => {
  emit('sort', column)
}

const handleSortChange = (e) => {
  const [column, direction] = e.target.value.split('-')
  emit('sort', column, direction)
}

const getPercent = (size) => {
  if (!props.totalSize || !size) return '0%'
  const p = (size / props.totalSize) * 100
  if (p < 0.1) return '<0.1%'
  return p.toFixed(1) + '%'
}

const getBarWidth = (size) => {
  if (!props.totalSize || !size) return '0%'
  return Math.min(100, (size / props.totalSize) * 100) + '%'
}

const formatSize = (bytes) => {
  if (bytes === 0) return '0 B'
  const units = ['B', 'KB', 'MB', 'GB', 'TB']
  const i = Math.floor(Math.log(bytes) / Math.log(1024))
  return (bytes / Math.pow(1024, i)).toFixed(i === 0 ? 0 : 2) + ' ' + units[i]
}

const formatTime = (ts) => {
  const d = new Date(ts)
  return d.toLocaleString('zh-CN', {
    year: 'numeric',
    month: '2-digit',
    day: '2-digit',
    hour: '2-digit',
    minute: '2-digit',
  })
}
</script>

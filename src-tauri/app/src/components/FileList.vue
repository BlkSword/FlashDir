<template>
  <div class="fd-filelist">
    <div class="fd-filter-bar">
      <div class="fd-filter-input-wrap">
        <svg fill="none" stroke="currentColor" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M21 21l-6-6m2-5a7 7 0 11-14 0 7 7 0 0114 0z" /></svg>
        <input
          v-model="localFilter"
          type="text"
          placeholder="过滤当前目录…  支持 ext:zip  size:>1MB  dir:node_modules"
          @input="$emit('filter', localFilter)"
        />
      </div>
      <span class="fd-filter-hint" @click="applyHint('ext:zip')">ext:zip</span>
      <span class="fd-filter-hint" @click="applyHint('size:>100MB')">size:>100MB</span>
      <span class="fd-filter-hint" @click="applyHint('type:dir')">type:dir</span>
    </div>

    <div class="fd-table-wrap">
      <table class="fd-table">
        <thead>
          <tr>
            <th
              class="fd-col-name"
              :class="{ sort: sortConfig.column === 'name', asc: sortConfig.column === 'name' && sortConfig.direction === 'asc' }"
              @click="handleSort('name')"
            >名称</th>
            <th
              class="fd-col-size"
              :class="{ sort: sortConfig.column === 'size', asc: sortConfig.column === 'size' && sortConfig.direction === 'asc' }"
              @click="handleSort('size')"
            >大小</th>
            <th class="fd-col-pct">占比</th>
            <th
              class="fd-col-date"
              :class="{ sort: sortConfig.column === 'mtime', asc: sortConfig.column === 'mtime' && sortConfig.direction === 'asc' }"
              @click="handleSort('mtime')"
            >修改时间</th>
          </tr>
        </thead>
        <tbody>
          <tr v-if="loading && items.length === 0">
            <td colspan="4" class="fd-empty-cell">
              <div class="fd-loading">
                <svg class="animate-spin" width="14" height="14" fill="none" viewBox="0 0 24 24">
                  <circle class="opacity-25" cx="12" cy="12" r="10" stroke="currentColor" stroke-width="4"></circle>
                  <path class="opacity-75" fill="currentColor" d="M4 12a8 8 0 018-8V0C5.373 0 0 5.373 0 12h4zm2 5.291A7.962 7.962 0 014 12H0c0 3.042 1.135 5.824 3 7.938l3-2.647z"></path>
                </svg>
                扫描中…
              </div>
            </td>
          </tr>
          <tr v-else-if="items.length === 0">
            <td colspan="4" class="fd-empty-cell">选择目录并开始扫描</td>
          </tr>
          <tr
            v-for="(item, index) in items"
            :key="index"
            :class="{ selected: selectedIndex === index }"
            @click="selectItem(index)"
            @dblclick="$emit('select', item)"
          >
            <td>
              <div class="fd-cell-name">
                <svg
                  class="fd-cell-icon"
                  :class="item.isDir ? 'fd-folder' : 'fd-file'"
                  fill="currentColor"
                  viewBox="0 0 24 24"
                >
                  <path
                    v-if="item.isDir"
                    d="M3 7v10a2 2 0 002 2h14a2 2 0 002-2V9a2 2 0 00-2-2h-6l-2-2H5a2 2 0 00-2 2z"
                  />
                  <path
                    v-else
                    d="M9 12h6m-6 4h6m2 5H7a2 2 0 01-2-2V5a2 2 0 012-2h5.586a1 1 0 01.707.293l5.414 5.414a1 1 0 01.293.707V19a2 2 0 01-2 2z"
                  />
                </svg>
                <span class="truncate">{{ item.name }}</span>
              </div>
            </td>
            <td class="fd-cell-size">{{ item.sizeFormatted || formatSize(item.size) }}</td>
            <td class="fd-cell-pct">
              <span>{{ getPercent(item.size) }}</span>
              <span class="fd-pct-bar"><span class="fd-pct-fill" :style="{ width: getBarWidth(item.size) }"></span></span>
            </td>
            <td class="fd-cell-date">{{ item.mtime ? formatTime(item.mtime * 1000) : '-' }}</td>
          </tr>
        </tbody>
      </table>
    </div>

    <div v-if="totalItems > pageSize" class="fd-pagination">
      <span class="fd-paging-info">共 {{ totalItems.toLocaleString() }} 项</span>
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
</template>

<script setup>
import { ref, watch } from 'vue'

const props = defineProps({
  items: { type: Array, default: () => [] },
  loading: { type: Boolean, default: false },
  totalSize: { type: Number, default: 0 },
  currentPath: { type: String, default: '' },
  sortConfig: { type: Object, default: () => ({ column: 'size', direction: 'desc' }) },
  currentPage: { type: Number, default: 1 },
  pageSize: { type: Number, default: 100 },
  totalItems: { type: Number, default: 0 },
  filterKeyword: { type: String, default: '' },
})

const emit = defineEmits(['sort', 'select', 'page-change', 'size-change', 'filter'])

const localFilter = ref(props.filterKeyword)
watch(() => props.filterKeyword, (v) => { localFilter.value = v })

const selectedIndex = ref(-1)

const selectItem = (index) => {
  selectedIndex.value = index
}

const handleSort = (column) => {
  emit('sort', column)
}

const applyHint = (hint) => {
  localFilter.value = hint
  emit('filter', hint)
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

<style scoped>
.fd-filelist {
  flex: 1;
  display: flex;
  flex-direction: column;
  min-height: 0;
}
.fd-filter-bar {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 6px 10px;
  background: var(--fd-bg-1);
  border-bottom: 1px solid var(--fd-border);
  flex-shrink: 0;
}
.fd-filter-input-wrap {
  flex: 1;
  display: flex;
  align-items: center;
  gap: 6px;
  padding: 4px 8px;
  background: var(--fd-bg-0);
  border: 1px solid var(--fd-border);
  border-radius: 3px;
}
.fd-filter-input-wrap svg { width: 13px; height: 13px; color: var(--fd-text-2); }
.fd-filter-input-wrap input {
  flex: 1;
  background: transparent;
  border: none;
  outline: none;
  color: var(--fd-text-0);
  font-size: 12px;
}
.fd-filter-input-wrap input::placeholder { color: var(--fd-text-3); }
.fd-filter-hint {
  font-size: 11px;
  color: var(--fd-text-2);
  padding: 2px 6px;
  border-radius: 3px;
  background: var(--fd-bg-2);
  cursor: pointer;
}
.fd-filter-hint:hover { color: var(--fd-text-1); }
.fd-table-wrap { flex: 1; overflow: auto; min-height: 0; }
.fd-table { width: 100%; border-collapse: collapse; font-size: 12px; }
.fd-table thead th {
  position: sticky;
  top: 0;
  background: var(--fd-bg-1);
  color: var(--fd-text-2);
  font-weight: 600;
  text-align: left;
  padding: 6px 10px;
  border-bottom: 1px solid var(--fd-border);
  cursor: pointer;
  user-select: none;
  white-space: nowrap;
}
.fd-table thead th.sort::after { content: " ▼"; font-size: 9px; }
.fd-table thead th.sort.asc::after { content: " ▲"; }
.fd-table tbody td {
  padding: 4px 10px;
  border-bottom: 1px solid transparent;
  color: var(--fd-text-1);
  white-space: nowrap;
}
.fd-table tbody tr:hover td { background: var(--fd-bg-2); }
.fd-table tbody tr.selected td { background: var(--fd-selected); color: #fff; }
.fd-cell-name { display: flex; align-items: center; gap: 6px; min-width: 0; }
.fd-cell-icon { width: 16px; height: 16px; flex-shrink: 0; }
.fd-cell-icon.fd-folder { color: var(--fd-folder); }
.fd-cell-icon.fd-file { color: var(--fd-file); }
.fd-cell-size { text-align: right; font-family: Consolas, 'JetBrains Mono', monospace; }
.fd-cell-pct { text-align: right; }
.fd-cell-date { text-align: right; color: var(--fd-text-2); }
.fd-table tbody tr.selected .fd-cell-date { color: rgba(255,255,255,0.7); }
.fd-pct-bar {
  display: inline-block;
  width: 50px;
  height: 3px;
  background: var(--fd-bg-3);
  border-radius: 2px;
  overflow: hidden;
  margin-left: 6px;
  vertical-align: middle;
}
.fd-pct-fill { display: block; height: 100%; background: var(--fd-accent); border-radius: 2px; }
.fd-empty-cell {
  text-align: center;
  padding: 40px 0;
  color: var(--fd-text-2);
}
.fd-loading {
  display: inline-flex;
  align-items: center;
  gap: 6px;
}
.fd-pagination {
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: 5px 10px;
  background: var(--fd-bg-1);
  border-top: 1px solid var(--fd-border);
  flex-shrink: 0;
}
.fd-paging-info { font-size: 12px; color: var(--fd-text-2); }
</style>

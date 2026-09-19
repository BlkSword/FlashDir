<template>
  <div ref="rootRef" class="fd-global-search-dropdown" :class="{ open: isOpen }">
    <div class="fd-search-input-wrap">
      <svg class="fd-search-icon" fill="none" stroke="currentColor" viewBox="0 0 24 24">
        <path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M21 21l-6-6m2-5a7 7 0 11-14 0 7 7 0 0114 0z" />
      </svg>
      <input
        ref="inputRef"
        v-model="query"
        type="text"
        :placeholder="ready ? '全局搜索 (Ctrl+K)' : '全局搜索（索引未就绪，点击展开）'"
        autocomplete="off"
        spellcheck="false"
        @focus="onFocus"
        @blur="onBlur"
        @keydown="onKeydown"
      />
      <button
        v-if="ready"
        class="fd-index-btn"
        title="刷新全局索引"
        :disabled="indexBuilding"
        @mousedown.prevent
        @click="refreshIndex"
      >
        <svg :class="{ 'animate-spin': indexBuilding }" fill="none" stroke="currentColor" viewBox="0 0 24 24">
          <path v-if="!indexBuilding" stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M4 4v5h.582m15.356 2A8.001 8.001 0 004.582 9m0 0H9m11 11v-5h-.581m0 0a8.003 8.003 0 01-15.357-2m15.357 2H15" />
          <path v-else stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M12 6v6m0 0v6m0-6h6m-6 0H6" />
        </svg>
      </button>
      <svg v-if="searching" class="fd-loading-icon" fill="none" viewBox="0 0 24 24">
        <circle class="opacity-25" cx="12" cy="12" r="10" stroke="currentColor" stroke-width="4"></circle>
        <path class="opacity-75" fill="currentColor" d="M4 12a8 8 0 018-8V0C5.373 0 0 5.373 0 12h4zm2 5.291A7.962 7.962 0 014 12H0c0 3.042 1.135 5.824 3 7.938l3-2.647z"></path>
      </svg>
    </div>

    <div
      v-if="isOpen"
      ref="panelRef"
      class="fd-search-dropdown"
      :class="{ expanded: expanded }"
      @mousedown.prevent
    >
      <div v-if="!ready" class="fd-index-state">
        <div v-if="kind === 'notLoaded'" class="fd-state-box">
          <p>首次使用需建立全盘文件索引（扫描所有 NTFS 盘的 MFT，约几秒至几十秒）。</p>
          <button class="fd-btn fd-btn-primary" :disabled="indexBuilding" @click="ensureIndex">建立索引</button>
        </div>
        <div v-else-if="kind === 'loading'" class="fd-state-box">
          <span class="fd-spinner"></span>
          <span class="fd-state-text">{{ statusText || '正在建立索引…' }}</span>
        </div>
        <div v-else-if="kind === 'failed'" class="fd-state-box failed">
          <p>{{ failedReason }}</p>
          <button class="fd-btn" :disabled="indexBuilding" @click="ensureIndex">重试</button>
        </div>
      </div>

      <template v-else>
        <div v-if="results.length > 0" class="fd-results-meta">
          找到 {{ results.length.toLocaleString() }} 项
          <span v-if="!expanded && results.length >= compactLimit" class="fd-results-meta-hint">· 仅显示前 {{ compactLimit }} 条</span>
        </div>

        <template v-if="results.length > 0">
          <!-- 小框模式：紧凑列表 -->
          <div v-if="!expanded" ref="listRef" class="fd-result-list">
            <div
              v-for="(item, index) in displayResults"
              :key="item.path"
              class="fd-result-item"
              :class="{ active: index === activeIndex }"
              :title="item.path"
              @click="openItem(item)"
              @contextmenu.prevent="showContextMenu($event, item)"
              @mousemove="activeIndex = index"
            >
              <div class="fd-result-icon">
                <svg v-if="item.isDir" fill="currentColor" viewBox="0 0 24 24"><path d="M3 7v10a2 2 0 002 2h14a2 2 0 002-2V9a2 2 0 00-2-2h-6l-2-2H5a2 2 0 00-2 2z" /></svg>
                <svg v-else fill="currentColor" viewBox="0 0 24 24"><path d="M9 12h6m-6 4h6m2 5H7a2 2 0 01-2-2V5a2 2 0 012-2h5.586a1 1 0 01.707.293l5.414 5.414a1 1 0 01.293.707V19a2 2 0 01-2 2z" /></svg>
              </div>
              <div class="fd-result-main">
                <div class="fd-result-name" v-html="highlight(item.name)"></div>
                <div class="fd-result-meta">
                  <span class="fd-result-path">{{ item.path }}</span>
                  <span v-if="!item.isDir"> · {{ formatSize(item.size) }}</span>
                  <span v-if="item.mtime"> · {{ formatTime(item.mtime * 1000) }}</span>
                </div>
              </div>
              <div class="fd-inline-actions" @click.stop>
                <button class="fd-action-btn" title="在主界面打开所在目录" @click="scanItemDir(item)">
                  <svg fill="none" stroke="currentColor" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M3 7v10a2 2 0 002 2h14a2 2 0 002-2V9a2 2 0 00-2-2h-6l-2-2H5a2 2 0 00-2 2z" /><path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M9 14l6-6m0 0h-4m4 0v4" /></svg>
                </button>
                <button v-if="!item.isDir" class="fd-action-btn" title="打开所在文件夹" @click="openParentFolder(item)">
                  <svg fill="none" stroke="currentColor" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M5 19a2 2 0 01-2-2V7a2 2 0 012-2h4l2 2h6a2 2 0 012 2v1M8 13h13l-2 6H10l-2-6z" /></svg>
                </button>
                <button class="fd-action-btn" title="复制完整路径" @click="copyToClipboard(item.path, '路径')">
                  <svg fill="none" stroke="currentColor" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M8 16H6a2 2 0 01-2-2V6a2 2 0 012-2h8a2 2 0 012 2v2m-6 12h8a2 2 0 002-2v-8a2 2 0 00-2-2h-8a2 2 0 00-2 2v8a2 2 0 002 2z" /></svg>
                </button>
              </div>
            </div>
          </div>

          <!-- 半屏模式：可排序表格 -->
          <div v-else ref="listRef" class="fd-result-table-wrap">
            <table class="fd-result-table">
              <thead>
                <tr>
                  <th class="col-name" :class="{ sort: sortKey === 'name', asc: sortKey === 'name' && sortDir === 'asc' }" @click="toggleSort('name')">名称</th>
                  <th class="col-path" :class="{ sort: sortKey === 'path', asc: sortKey === 'path' && sortDir === 'asc' }" @click="toggleSort('path')">路径</th>
                  <th class="col-size" :class="{ sort: sortKey === 'size', asc: sortKey === 'size' && sortDir === 'asc' }" @click="toggleSort('size')">大小</th>
                  <th class="col-mtime" :class="{ sort: sortKey === 'mtime', asc: sortKey === 'mtime' && sortDir === 'asc' }" @click="toggleSort('mtime')">修改时间</th>
                </tr>
              </thead>
              <tbody>
                <tr
                  v-for="(item, index) in displayResults"
                  :key="item.path"
                  :class="{ active: index === activeIndex }"
                  :title="item.path"
                  @click="openItem(item)"
                  @contextmenu.prevent="showContextMenu($event, item)"
                  @mousemove="activeIndex = index"
                >
                  <td class="col-name"><span v-html="highlight(item.name)"></span></td>
                  <td class="col-path mono">{{ item.path }}</td>
                  <td class="col-size mono">{{ item.isDir ? '-' : formatSize(item.size) }}</td>
                  <td class="col-mtime mono">{{ item.mtime ? formatTime(item.mtime * 1000) : '-' }}</td>
                </tr>
              </tbody>
            </table>
          </div>
        </template>

        <div v-else class="fd-result-list empty">
          <div v-if="ready && query && !searching" class="fd-empty">
            <div class="fd-empty-title">{{ lastNoResultMsg }}</div>
            <div v-if="lastIndexSize > 0" class="fd-empty-sub">
              索引共 {{ lastIndexSize.toLocaleString() }} 项
            </div>
          </div>
          <div v-else-if="ready && !query" class="fd-empty">
            <div class="fd-empty-title">输入关键字开始搜索</div>
            <div class="fd-empty-sub">支持 *.pdf · ext:zip · size:&gt;100MB · mtime:&lt;7d · NOT 等语法</div>
          </div>
        </div>

        <div
          v-if="contextMenu.visible"
          class="fd-context-menu"
          :style="{ left: `${contextMenu.x}px`, top: `${contextMenu.y}px` }"
          @click.stop
        >
          <div class="fd-context-item" @click="onContextMenuClick('open')">打开</div>
          <div class="fd-context-item" @click="onContextMenuClick('open-folder')">打开所在文件夹</div>
          <div class="fd-context-item" @click="onContextMenuClick('scan-dir')">在主界面打开所在目录</div>
          <div class="fd-context-divider"></div>
          <div class="fd-context-item" @click="onContextMenuClick('copy-path')">复制完整路径</div>
          <div class="fd-context-item" @click="onContextMenuClick('copy-name')">复制文件名</div>
        </div>

        <div v-if="expanded && results.length > pageSize" class="fd-results-pagination">
          <a-pagination
            v-model:current="currentPage"
            v-model:page-size="pageSize"
            :total="results.length"
            :page-size-options="['50', '100', '200', '500']"
            show-size-changer
            size="small"
          />
        </div>

        <div v-if="ready" class="fd-dropdown-footer">
          <span v-if="indexMeta" class="fd-footer-meta">
            已索引 {{ ((indexMeta.fileCount || 0) + (indexMeta.dirCount || 0)).toLocaleString() }} 项 · {{ indexMeta.driveCount || 0 }} 个盘
            <span v-if="indexMeta.failedDrives?.length" class="fd-footer-warn">
              （{{ indexMeta.failedDrives.join(', ') }} 跳过）
            </span>
            <span v-if="indexMeta.partial" class="fd-footer-warn">
              （仅部分目录，点右侧刷新可重建全盘索引）
            </span>
          </span>
          <span v-else class="fd-footer-meta">↑↓ 选择 · Enter 打开 · Ctrl+Enter 所在文件夹 · Esc 关闭</span>
          <span class="fd-footer-spacer"></span>
          <button
            v-if="(!expanded && results.length >= compactLimit) || expanded"
            class="fd-btn fd-btn-link"
            @click="toggleExpanded"
          >
            {{ expanded ? '收起' : `查看全部 ${results.length.toLocaleString()} 项` }}
          </button>
        </div>
      </template>
    </div>
  </div>
</template>

<script setup>
import { ref, computed, watch, onMounted, onUnmounted, nextTick } from 'vue'
import { message } from 'ant-design-vue'
import { useTauri } from '../composables/useTauri'
import { useGlobalSearch } from '../composables/useGlobalSearch'
import { formatSize, formatTime, formatError, debounce, getParentPath } from '../utils/format.js'

const { invoke } = useTauri()
const gs = useGlobalSearch()
const { kind, ready, failedReason, indexMeta, statusText } = gs

const emit = defineEmits(['open-dir'])

const rootRef = ref(null)
const inputRef = ref(null)
const listRef = ref(null)

const query = ref('')
const results = ref([])
const searching = ref(false)
const indexBuilding = ref(false)
const isOpen = ref(false)
const expanded = ref(false)
const currentPage = ref(1)
const pageSize = ref(100)
const activeIndex = ref(-1)
const contextMenu = ref({ visible: false, x: 0, y: 0, item: null })
const lastIndexSize = ref(0)
const lastNoResultMsg = ref('无匹配文件')

const compactLimit = 50
const expandedLimit = 1000

let blurTimer = null

// 半屏表格排序（空串 = 保持后端相关性排序）
const sortKey = ref('')
const sortDir = ref('asc')

const sortedResults = computed(() => {
  if (!sortKey.value) return results.value
  const k = sortKey.value
  return [...results.value].sort((a, b) => {
    let r
    if (k === 'name') r = a.name.localeCompare(b.name, 'zh-CN')
    else if (k === 'path') r = a.path.localeCompare(b.path, 'zh-CN')
    else if (k === 'size') r = (a.size || 0) - (b.size || 0)
    else r = (a.mtime || 0) - (b.mtime || 0)
    return sortDir.value === 'asc' ? r : -r
  })
})

const displayResults = computed(() => {
  if (expanded.value) {
    const start = (currentPage.value - 1) * pageSize.value
    return sortedResults.value.slice(start, start + pageSize.value)
  }
  return results.value.slice(0, compactLimit)
})

const toggleSort = (key) => {
  if (sortKey.value === key) {
    sortDir.value = sortDir.value === 'asc' ? 'desc' : 'asc'
  } else {
    sortKey.value = key
    sortDir.value = (key === 'name' || key === 'path') ? 'asc' : 'desc'
  }
  currentPage.value = 1
}

const open = () => {
  isOpen.value = true
}

const close = () => {
  isOpen.value = false
  activeIndex.value = -1
  // 注意：不重置 expanded —— 记住用户上次选择的视图模式（本次会话内有效）
}

const focusSearch = () => {
  inputRef.value?.focus?.()
  open()
}

const toggleExpanded = () => {
  expanded.value = !expanded.value
  currentPage.value = 1
  if (expanded.value && results.value.length >= compactLimit) {
    doSearch(expandedLimit)
  }
  nextTick(() => inputRef.value?.focus?.())
}

const onFocus = () => {
  open()
  gs.fetchStatus()
}

const onBlur = () => {
  blurTimer = setTimeout(() => {
    close()
  }, 200)
}

const scrollActiveIntoView = () => {
  nextTick(() => {
    listRef.value
      ?.querySelector('.fd-result-item.active')
      ?.scrollIntoView({ block: 'nearest' })
  })
}

const onKeydown = (e) => {
  const count = displayResults.value.length
  if (e.key === 'ArrowDown') {
    e.preventDefault()
    if (count > 0) {
      activeIndex.value = activeIndex.value + 1 >= count ? 0 : activeIndex.value + 1
      scrollActiveIntoView()
    }
  } else if (e.key === 'ArrowUp') {
    e.preventDefault()
    if (count > 0) {
      activeIndex.value = activeIndex.value - 1 < 0 ? count - 1 : activeIndex.value - 1
      scrollActiveIntoView()
    }
  } else if (e.key === 'Enter') {
    e.preventDefault()
    const item = displayResults.value[activeIndex.value] || displayResults.value[0]
    if (!item) return
    // Ctrl+Enter：打开所在文件夹；Enter：打开文件/目录本身
    if (e.ctrlKey || e.metaKey) {
      openParentFolder(item)
    } else {
      openItem(item)
    }
  } else if (e.key === 'Escape') {
    if (expanded.value) {
      expanded.value = false
    } else {
      close()
      inputRef.value?.blur?.()
    }
  }
}

const doSearch = async (limit) => {
  if (!ready.value || !query.value.trim()) {
    results.value = []
    return
  }
  searching.value = true
  try {
    const res = await gs.search(query.value, limit)
    results.value = res.results || []
    lastIndexSize.value = res.indexSize || 0
    lastNoResultMsg.value = '无匹配文件'
    activeIndex.value = results.value.length > 0 ? 0 : -1
  } catch (e) {
    console.error('搜索失败', e)
    results.value = []
    lastIndexSize.value = 0
    lastNoResultMsg.value = '搜索失败，请重试'
  } finally {
    searching.value = false
  }
}

const onQueryChange = debounce(async () => {
  currentPage.value = 1
  if (!query.value.trim()) {
    results.value = []
    lastIndexSize.value = 0
    activeIndex.value = -1
    return
  }
  await doSearch(expanded.value ? expandedLimit : compactLimit)
}, 200)

watch(query, () => {
  onQueryChange()
})

const ensureIndex = async () => {
  indexBuilding.value = true
  try {
    const s = await gs.ensureIndex()
    if (s?.kind === 'failed') {
      // 建索引失败通常是权限不足，尝试以管理员身份重启。
      // 提权成功后旧进程会在 Rust 侧自行退出（避免双实例 + 双托盘）。
      try {
        await gs.restartAsAdmin()
      } catch {}
    }
  } catch (e) {
    console.error(e)
  } finally {
    indexBuilding.value = false
  }
}

const refreshIndex = async () => {
  if (!ready.value) return
  indexBuilding.value = true
  results.value = []
  query.value = ''
  try {
    await gs.refreshIndex()
  } catch (e) {
    console.error(e)
  } finally {
    indexBuilding.value = false
  }
}

const openItem = async (item) => {
  try {
    await invoke('open_path', { path: item.path })
  } catch (e) {
    console.error('打开失败', e)
    message.error('打开失败: ' + formatError(e))
  }
}

const openParentFolder = async (item) => {
  const target = item.isDir ? item.path : getParentPath(item.path)
  try {
    await invoke('open_path', { path: target })
  } catch (e) {
    console.error('打开失败', e)
    message.error('打开失败: ' + formatError(e))
  }
}

const scanItemDir = (item) => {
  const target = item.isDir ? item.path : getParentPath(item.path)
  emit('open-dir', target)
  close()
  inputRef.value?.blur?.()
}

const copyToClipboard = async (text, label) => {
  try {
    await navigator.clipboard.writeText(text)
    message.success(`${label}已复制`)
  } catch (e) {
    console.error('复制失败', e)
    message.error('复制失败: ' + formatError(e))
  }
}

const showContextMenu = (e, item) => {
  e.stopPropagation()
  contextMenu.value = {
    visible: true,
    x: e.clientX,
    y: e.clientY,
    item
  }
}

const closeContextMenu = () => {
  contextMenu.value.visible = false
}

const onContextMenuClick = (key) => {
  const item = contextMenu.value.item
  if (!item) return
  closeContextMenu()
  switch (key) {
    case 'open':
      openItem(item)
      break
    case 'open-folder':
      openParentFolder(item)
      break
    case 'scan-dir':
      scanItemDir(item)
      break
    case 'copy-path':
      copyToClipboard(item.path, '路径')
      break
    case 'copy-name':
      copyToClipboard(item.name, '文件名')
      break
    default:
      break
  }
}

const escapeHtml = (s) =>
  String(s).replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;')

const highlight = (name) => {
  const q = query.value.trim()
  if (!q) return escapeHtml(name)

  const tokens = q
    .split(/\s+/)
    .filter((t) => t && !t.includes(':') && !['AND', 'OR', 'NOT'].includes(t.toUpperCase()))
    .map((t) => {
      let s = t.toLowerCase()
      if (s.startsWith('*.')) return s.slice(2)
      if (s.startsWith('.')) return s.slice(1)
      if (s.startsWith('*')) return s.slice(1)
      if (s.endsWith('*')) return s.slice(0, -1)
      return s
    })
    .filter(Boolean)
  if (tokens.length === 0) return escapeHtml(name)

  const intervals = []
  const lower = name.toLowerCase()
  for (const token of tokens) {
    let pos = 0
    while ((pos = lower.indexOf(token, pos)) !== -1) {
      intervals.push([pos, pos + token.length])
      pos += token.length
    }
  }
  if (intervals.length === 0) return escapeHtml(name)

  intervals.sort((a, b) => a[0] - b[0])
  const merged = [intervals[0]]
  for (let i = 1; i < intervals.length; i++) {
    const last = merged[merged.length - 1]
    const cur = intervals[i]
    if (cur[0] <= last[1]) {
      last[1] = Math.max(last[1], cur[1])
    } else {
      merged.push(cur)
    }
  }

  let html = ''
  let last = 0
  for (const [start, end] of merged) {
    html += escapeHtml(name.slice(last, start))
    html += '<mark>' + escapeHtml(name.slice(start, end)) + '</mark>'
    last = end
  }
  html += escapeHtml(name.slice(last))
  return html
}

const onWindowClick = () => {
  closeContextMenu()
}

onMounted(() => {
  window.addEventListener('click', onWindowClick)
  window.addEventListener('resize', closeContextMenu)
})

onUnmounted(() => {
  if (blurTimer) clearTimeout(blurTimer)
  window.removeEventListener('click', onWindowClick)
  window.removeEventListener('resize', closeContextMenu)
})

defineExpose({ focusSearch })
</script>

<style>
/* 自定义右键菜单 */
.fd-context-menu {
  position: fixed;
  min-width: 160px;
  background: var(--fd-bg-2);
  border: 1px solid var(--fd-border);
  border-radius: 4px;
  box-shadow: 0 6px 20px rgba(0, 0, 0, 0.4);
  z-index: 3000;
  padding: 4px 0;
}
.fd-context-item {
  padding: 6px 12px;
  color: var(--fd-text-1);
  font-size: 13px;
  cursor: pointer;
  white-space: nowrap;
}
.fd-context-item:hover {
  background: var(--fd-bg-3);
}
.fd-context-divider {
  height: 1px;
  background-color: var(--fd-border);
  margin: 4px 0;
}
</style>

<style scoped>
.fd-global-search-dropdown {
  position: relative;
  width: 300px;
}
.fd-search-input-wrap {
  position: relative;
  display: flex;
  align-items: center;
  gap: 6px;
  padding: 4px 6px;
  background: var(--fd-bg-0);
  border: 1px solid var(--fd-border);
  border-radius: 3px;
  transition: border-color 0.15s;
}
.fd-search-input-wrap:focus-within {
  border-color: var(--fd-accent);
}
.fd-search-input-wrap input {
  flex: 1;
  min-width: 0;
  background: transparent;
  border: none;
  outline: none;
  color: var(--fd-text-1);
  font-size: 12px;
  line-height: 18px;
}
.fd-search-input-wrap input::placeholder {
  color: var(--fd-text-3);
}
.fd-search-icon,
.fd-loading-icon,
.fd-index-btn svg {
  width: 13px;
  height: 13px;
  color: var(--fd-text-2);
  flex-shrink: 0;
}
.fd-loading-icon {
  animation: spin 1s linear infinite;
}
.fd-index-btn {
  width: 18px;
  height: 18px;
  padding: 0;
  display: inline-grid;
  place-items: center;
  border: none;
  background: transparent;
  color: var(--fd-text-2);
  border-radius: 2px;
  cursor: pointer;
  flex-shrink: 0;
}
.fd-index-btn:hover:not(:disabled) {
  color: var(--fd-text-0);
  background: var(--fd-bg-3);
}
.fd-index-btn:disabled {
  opacity: 0.5;
  cursor: default;
}
.fd-index-btn .animate-spin {
  animation: spin 1s linear infinite;
}

@keyframes spin {
  from { transform: rotate(0deg); }
  to { transform: rotate(360deg); }
}

.fd-search-dropdown {
  position: absolute;
  top: calc(100% + 4px);
  right: 0;
  width: 520px;
  max-height: 400px;
  overflow-y: auto;
  background: var(--fd-bg-1);
  border: 1px solid var(--fd-border);
  border-radius: 4px;
  box-shadow: 0 12px 40px rgba(0, 0, 0, 0.5);
  z-index: 2000;
}
.fd-search-dropdown.expanded {
  /* 半屏完整视图：脱离工具栏锚定，居中定宽 */
  position: fixed;
  top: 46px;
  left: 50%;
  transform: translateX(-50%);
  width: 50vw;
  min-width: 520px;
  max-height: 60vh;
}

.fd-index-state {
  padding: 20px 16px;
  text-align: center;
}
.fd-state-box {
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: 12px;
  color: var(--fd-text-2);
  font-size: 13px;
}
.fd-state-box p { margin: 0; max-width: 440px; }
.fd-state-box.failed { color: var(--fd-danger); }
.fd-state-text { margin-left: 8px; color: var(--fd-text-2); }
.fd-spinner {
  width: 16px;
  height: 16px;
  border: 2px solid var(--fd-border);
  border-top-color: var(--fd-accent);
  border-radius: 50%;
  animation: spin 0.8s linear infinite;
}

.fd-results-meta {
  padding: 8px 12px 4px;
  font-size: 12px;
  color: var(--fd-text-2);
}
.fd-results-meta-hint {
  color: var(--fd-text-3);
  margin-left: 4px;
}
.fd-result-list {
  padding: 0 6px 6px;
}
.fd-result-list.empty {
  min-height: 80px;
  display: flex;
  align-items: center;
  justify-content: center;
}

/* 半屏模式：可排序结果表格 */
.fd-result-table-wrap {
  overflow: auto;
  max-height: calc(60vh - 90px);
}
.fd-result-table {
  width: 100%;
  border-collapse: collapse;
  font-size: 12px;
  table-layout: fixed;
}
.fd-result-table thead th {
  position: sticky;
  top: 0;
  z-index: 1;
  background: var(--fd-bg-2);
  color: var(--fd-text-2);
  font-weight: 600;
  text-align: left;
  padding: 6px 10px;
  border-bottom: 1px solid var(--fd-border);
  cursor: pointer;
  user-select: none;
  white-space: nowrap;
}
.fd-result-table thead th.sort::after { content: " ▼"; font-size: 9px; }
.fd-result-table thead th.sort.asc::after { content: " ▲"; }
.fd-result-table tbody td {
  padding: 4px 10px;
  color: var(--fd-text-1);
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
  border-bottom: 1px solid transparent;
}
.fd-result-table tbody tr { cursor: pointer; }
.fd-result-table tbody tr:hover td,
.fd-result-table tbody tr.active td { background: var(--fd-bg-2); }
.fd-result-table .col-name { width: 32%; }
.fd-result-table .col-path { width: 40%; }
.fd-result-table .col-size { width: 12%; text-align: right; }
.fd-result-table .col-mtime { width: 16%; text-align: right; }
.fd-result-table .mono { font-family: Consolas, 'JetBrains Mono', monospace; }
.fd-result-table td.col-name :deep(mark),
.fd-result-name :deep(mark) {
  background: rgba(0, 122, 204, 0.35);
  color: var(--fd-text-0);
  border-radius: 2px;
  padding: 0 1px;
}
.fd-result-item {
  display: flex;
  align-items: flex-start;
  gap: 8px;
  padding: 5px 8px;
  border-radius: 4px;
  cursor: pointer;
  transition: background 0.1s;
}
.fd-result-item:hover,
.fd-result-item.active {
  background: var(--fd-bg-2);
}
.fd-result-item.active {
  outline: 1px solid var(--fd-accent);
  outline-offset: -1px;
}
.fd-result-icon {
  width: 16px;
  height: 16px;
  color: var(--fd-folder);
  flex-shrink: 0;
  margin-top: 1px;
}
.fd-result-main { flex: 1; min-width: 0; }
.fd-result-name {
  font-size: 13px;
  color: var(--fd-text-0);
  word-break: break-all;
}
.fd-result-name :deep(mark) {
  background: rgba(0, 122, 204, 0.35);
  color: var(--fd-text-0);
  border-radius: 2px;
  padding: 0 1px;
}
.fd-result-meta {
  font-family: Consolas, 'JetBrains Mono', monospace;
  font-size: 11px;
  color: var(--fd-text-2);
  word-break: break-all;
  margin-top: 2px;
}

/* 悬停/选中时显示的内联操作按钮 */
.fd-inline-actions {
  display: none;
  align-items: center;
  gap: 2px;
  flex-shrink: 0;
  margin-top: 1px;
}
.fd-result-item:hover .fd-inline-actions,
.fd-result-item.active .fd-inline-actions {
  display: inline-flex;
}
.fd-action-btn {
  width: 22px;
  height: 22px;
  padding: 0;
  display: inline-grid;
  place-items: center;
  border: none;
  background: transparent;
  color: var(--fd-text-2);
  border-radius: 3px;
  cursor: pointer;
}
.fd-action-btn:hover {
  color: var(--fd-text-0);
  background: var(--fd-bg-3);
}
.fd-action-btn svg { width: 14px; height: 14px; }

.fd-empty {
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  gap: 4px;
  padding: 28px 16px;
  text-align: center;
}
.fd-empty-title {
  color: var(--fd-text-2);
  font-size: 13px;
}
.fd-empty-sub {
  color: var(--fd-text-3);
  font-size: 12px;
}

.fd-results-pagination {
  padding: 8px 12px;
  border-top: 1px solid var(--fd-border);
}

.fd-dropdown-footer {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 8px 12px;
  border-top: 1px solid var(--fd-border);
  background: var(--fd-bg-1);
}
.fd-footer-meta {
  font-size: 11px;
  color: var(--fd-text-2);
}
.fd-footer-spacer { flex: 1; }
.fd-footer-warn { color: var(--fd-danger); }

.fd-btn {
  display: inline-flex;
  align-items: center;
  gap: 4px;
  padding: 4px 10px;
  border: 1px solid var(--fd-border);
  background: var(--fd-bg-2);
  color: var(--fd-text-1);
  border-radius: 3px;
  font-size: 12px;
  cursor: pointer;
}
.fd-btn:hover:not(:disabled) { background: var(--fd-bg-3); }
.fd-btn:disabled { opacity: 0.5; cursor: default; }
.fd-btn-primary {
  background: var(--fd-accent);
  border-color: var(--fd-accent);
  color: #fff;
}
.fd-btn-primary:hover:not(:disabled) { background: var(--fd-accent-hover); border-color: var(--fd-accent-hover); }
.fd-btn-link {
  border: none;
  background: transparent;
  color: var(--fd-accent);
  padding: 2px 6px;
}
.fd-btn-link:hover:not(:disabled) { background: rgba(0, 122, 204, 0.12); }
</style>

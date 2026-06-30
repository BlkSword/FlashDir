<template>
  <a-modal
    :open="visible"
    :title="expanded ? null : '全局搜索'"
    :width="expanded ? '80vw' : 660"
    :footer="null"
    :destroy-on-close="false"
    :closable="!expanded"
    :class="['fd-global-search', { 'fd-global-search-expanded': expanded }]"
    @cancel="close"
  >
    <div class="fd-search-input-wrap" :class="{ expanded: expanded }">
      <svg class="fd-search-icon" fill="none" stroke="currentColor" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M21 21l-6-6m2-5a7 7 0 11-14 0 7 7 0 0114 0z" /></svg>
      <a-input-search
        ref="inputRef"
        v-model:value="query"
        :placeholder="ready ? '输入文件名，跨所有盘搜索…' : '索引未就绪'"
        :disabled="!ready"
        :loading="searching"
        allow-clear
        @change="onQueryChange"
        @search="onQueryChange"
      />
      <button
        class="fd-expand-btn"
        :title="expanded ? '收缩窗口' : '展开为半屏（Everything 风格）'"
        @click="toggleExpanded"
      >
        <svg v-if="expanded" fill="none" stroke="currentColor" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M9 4H5v4M5 4l4 4m6-4h4v4M19 4l-4 4M9 20H5v-4M5 20l4-4m6 4h4v-4M19 20l-4-4" /></svg>
        <svg v-else fill="none" stroke="currentColor" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M4 8V4m0 0h4M4 4l5 5m11-1V4m0 0h-4m4 0l-5 5M4 16v4m0 0h4m-4 0l5-5m11 5l-5-5m5 5v-4m0 4h-4" /></svg>
      </button>
    </div>

    <div v-if="!ready" class="fd-index-state">
      <div v-if="stateKind === 'notLoaded'" class="fd-state-box">
        <p>首次使用需建立全盘文件索引（扫描所有 NTFS 盘的 MFT，约几秒至几十秒）。</p>
        <a-button type="primary" :loading="loading" @click="ensureIndex">建立索引</a-button>
      </div>
      <div v-else-if="stateKind === 'loading'" class="fd-state-box">
        <a-spin />
        <span class="fd-state-text">
          正在扫描 {{ progress?.drive || stateData?.drive || '…' }}
          · 已索引 {{ (progress?.scanned ?? stateData?.scanned ?? 0) }} 项
        </span>
      </div>
      <div v-else-if="stateKind === 'failed'" class="fd-state-box">
        <p>{{ failedReason }}</p>
        <a-button :loading="loading" @click="ensureIndex">重试</a-button>
      </div>
    </div>

    <div v-else class="fd-results">
      <div v-if="query && results.length > 0" class="fd-results-meta">
        找到 {{ results.length }} 项
      </div>
      <div class="fd-result-list">
        <a-dropdown
          v-for="(item, index) in displayResults"
          :key="index"
          :trigger="['contextmenu']"
          overlay-class-name="fd-result-context-menu"
        >
          <div
            class="fd-result-item"
            :title="item.path"
            @click="openTarget(item)"
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
          </div>
          <template #overlay>
            <a-menu @click="({ key }) => onContextMenuClick(key, item)">
              <a-menu-item key="open">打开</a-menu-item>
              <a-menu-item key="open-folder">打开所在文件夹</a-menu-item>
              <a-menu-item key="scan-dir">在主界面打开所在目录</a-menu-item>
              <a-menu-divider />
              <a-menu-item key="copy-path">复制完整路径</a-menu-item>
              <a-menu-item key="copy-name">复制文件名</a-menu-item>
            </a-menu>
          </template>
        </a-dropdown>
      </div>

      <a-pagination
        v-if="query && results.length > pageSize"
        v-model:current="currentPage"
        v-model:page-size="pageSize"
        :total="results.length"
        :page-size-options="['50', '100', '200', '500']"
        show-size-changer
        size="small"
        class="fd-results-pagination"
      />

      <a-empty
        v-if="ready && query && results.length === 0 && !searching"
        :description="lastNoResultMsg"
        :image="emptyImage"
      />
    </div>

    <div v-if="ready" class="fd-modal-footer" :class="{ 'fd-modal-footer-inline': expanded }">
      <span v-if="indexMeta" class="fd-footer-meta">
        已索引 {{ (indexMeta.fileCount || 0) + (indexMeta.dirCount || 0) }} 项 · {{ indexMeta.driveCount || 0 }} 个盘
        <span v-if="indexMeta.failedDrives?.length" class="fd-footer-warn">
          （{{ indexMeta.failedDrives.join(', ') }} 跳过）
        </span>
      </span>
      <span class="fd-footer-spacer"></span>
      <a-button size="small" :loading="loading" @click="refreshIndex">刷新索引</a-button>
    </div>
  </a-modal>
</template>

<script setup>
import { ref, computed, watch, onMounted, onUnmounted, nextTick } from 'vue'
import { Empty, message } from 'ant-design-vue'
import { listen } from '@tauri-apps/api/event'
import { useTauri } from '../composables/useTauri'
import { formatSize, debounce, getParentPath } from '../utils/format.js'

const { invoke } = useTauri()
const emptyImage = Empty.PRESENTED_IMAGE_SIMPLE
const lastNoResultMsg = ref('无匹配文件')

const props = defineProps({
  visible: { type: Boolean, default: false },
})
const emit = defineEmits(['update:visible', 'open-dir'])

const query = ref('')
const results = ref([])
const searching = ref(false)
const loading = ref(false)
const state = ref({ kind: 'notLoaded' })
const progress = ref(null)
const inputRef = ref(null)
const currentPage = ref(1)
const pageSize = ref(100)
const expanded = ref(false)

let unlistenProgress = null

const stateKind = computed(() => state.value?.kind)
const stateData = computed(() => state.value?.data)
const ready = computed(() => stateKind.value === 'ready')
const indexMeta = computed(() => (ready.value ? state.value.data : null))
const failedReason = computed(() => (stateKind.value === 'failed' ? state.value?.data?.reason : ''))

const displayResults = computed(() => {
  const start = (currentPage.value - 1) * pageSize.value
  return results.value.slice(start, start + pageSize.value)
})

const toggleExpanded = () => {
  expanded.value = !expanded.value
  nextTick(() => inputRef.value?.focus?.())
}

const fetchStatus = async () => {
  try {
    state.value = await invoke('global_search_status')
  } catch (e) {
    console.error('获取索引状态失败', e)
  }
}

const onQueryChange = debounce(async () => {
  if (!ready.value || !query.value.trim()) {
    results.value = []
    return
  }
  searching.value = true
  currentPage.value = 1
  try {
    const res = await invoke('global_search', { query: query.value, limit: 1000 })
    state.value = res.state
    results.value = res.results || []
    if (!results.value.length && res.indexSize != null) {
      let msg = `无匹配文件（索引共 ${res.indexSize} 项）`
      if (res.sampleNames?.length) {
        msg += `，示例文件名: ${res.sampleNames.slice(0, 3).join(', ')}`
      }
      lastNoResultMsg.value = msg
    } else if (!results.value.length) {
      lastNoResultMsg.value = '无匹配文件'
    }
  } catch (e) {
    console.error('搜索失败', e)
    results.value = []
    lastNoResultMsg.value = '无匹配文件'
  } finally {
    searching.value = false
  }
}, 200)

const ensureIndex = async () => {
  loading.value = true
  try {
    await invoke('global_search_ensure_index')
    await fetchStatus()
    const s = state.value
    const needUac = s?.kind === 'failed'
    if (needUac) {
      try {
        await invoke('restart_as_admin')
        setTimeout(() => window.close(), 500)
        return
      } catch {}
    }
  } catch (e) {
    console.error(e)
  } finally {
    loading.value = false
  }
}

const refreshIndex = async () => {
  loading.value = true
  results.value = []
  query.value = ''
  try {
    await invoke('global_search_refresh')
    await fetchStatus()
  } catch (e) {
    console.error(e)
  } finally {
    loading.value = false
  }
}

const openTarget = (item) => {
  const target = item.isDir ? item.path : getParentPath(item.path)
  emit('open-dir', target)
  close()
}

const openItem = async (item) => {
  try {
    await invoke('open_path', { path: item.path })
  } catch (e) {
    console.error('打开失败', e)
    message.error('打开失败: ' + e)
  }
}

const openParentFolder = async (item) => {
  const target = item.isDir ? item.path : getParentPath(item.path)
  try {
    await invoke('open_path', { path: target })
  } catch (e) {
    console.error('打开失败', e)
    message.error('打开失败: ' + e)
  }
}

const scanItemDir = (item) => {
  const target = item.isDir ? item.path : getParentPath(item.path)
  emit('open-dir', target)
  close()
}

const copyToClipboard = async (text, label) => {
  try {
    await navigator.clipboard.writeText(text)
    message.success(`${label}已复制`)
  } catch (e) {
    console.error('复制失败', e)
    message.error('复制失败')
  }
}

const onContextMenuClick = (key, item) => {
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

const close = () => emit('update:visible', false)

const escapeHtml = (s) =>
  String(s).replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;')

const highlight = (name) => {
  const q = query.value.trim()
  if (!q) return escapeHtml(name)

  const tokens = q
    .split(/\s+/)
    .filter((t) => t && !t.includes(':') && !['AND', 'OR', 'NOT'].includes(t.toUpperCase()))
    .map((t) => t.toLowerCase())
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

const formatTime = (ts) => {
  const d = new Date(ts)
  return d.toLocaleString('zh-CN', { year: 'numeric', month: '2-digit', day: '2-digit', hour: '2-digit', minute: '2-digit' })
}

onMounted(async () => {
  unlistenProgress = await listen('global-search-progress', (event) => {
    progress.value = event.payload
    if (event.payload?.phase === 'done') {
      fetchStatus()
    } else {
      state.value = {
        kind: 'loading',
        data: { drive: event.payload?.drive || '', scanned: event.payload?.scanned || 0 }
      }
    }
  })
  fetchStatus()
})

onUnmounted(() => {
  if (unlistenProgress) unlistenProgress()
})

watch(
  () => props.visible,
  (v) => {
    if (v) {
      fetchStatus()
      nextTick(() => inputRef.value?.focus?.())
    } else {
      expanded.value = false
    }
  }
)
</script>

<style>
.fd-global-search .ant-modal-content {
  background: var(--fd-bg-1) !important;
  border: 1px solid var(--fd-border);
}
.fd-global-search .ant-modal-header {
  background: var(--fd-bg-1) !important;
  border-bottom: 1px solid var(--fd-border);
}
.fd-global-search .ant-modal-title { color: var(--fd-text-0); }
.fd-global-search .ant-modal-close { color: var(--fd-text-2); }
.fd-global-search .ant-input {
  background: var(--fd-bg-0) !important;
  border-color: var(--fd-border) !important;
  color: var(--fd-text-0) !important;
  padding: 8px 12px !important;
}
.fd-global-search .ant-input::placeholder { color: var(--fd-text-3) !important; }

/* 展开模式：顶部居中、更宽、更像 Everything */
.fd-global-search-expanded .ant-modal {
  top: 8vh !important;
  padding-bottom: 0 !important;
}
.fd-global-search-expanded .ant-modal-content {
  border-radius: 8px !important;
  box-shadow: 0 16px 48px rgba(0, 0, 0, 0.5) !important;
}
.fd-global-search-expanded .ant-modal-header {
  display: none;
}
.fd-global-search-expanded .ant-modal-body {
  padding: 16px 20px 12px !important;
}

/* 右键菜单暗色主题 */
.fd-result-context-menu .ant-dropdown-menu {
  background: var(--fd-bg-2) !important;
  border: 1px solid var(--fd-border) !important;
  border-radius: 4px !important;
  box-shadow: 0 6px 20px rgba(0, 0, 0, 0.4) !important;
}
.fd-result-context-menu .ant-dropdown-menu-item {
  color: var(--fd-text-1) !important;
  font-size: 13px !important;
}
.fd-result-context-menu .ant-dropdown-menu-item:hover {
  background: var(--fd-bg-3) !important;
}
.fd-result-context-menu .ant-dropdown-menu-item-divider {
  background-color: var(--fd-border) !important;
}
</style>

<style scoped>
.fd-search-input-wrap {
  display: flex;
  align-items: center;
  gap: 8px;
}
.fd-search-input-wrap.expanded {
  gap: 12px;
  padding: 4px 0 8px;
}
.fd-search-icon {
  width: 16px;
  height: 16px;
  color: var(--fd-text-2);
  flex-shrink: 0;
}
.fd-search-input-wrap.expanded .fd-search-icon {
  width: 22px;
  height: 22px;
}
.fd-search-input-wrap :deep(.ant-input) {
  flex: 1;
  font-size: 14px;
}
.fd-search-input-wrap.expanded :deep(.ant-input) {
  font-size: 18px;
  padding: 10px 14px !important;
}
.fd-expand-btn {
  width: 26px;
  height: 26px;
  padding: 0;
  display: inline-grid;
  place-items: center;
  border: 1px solid var(--fd-border);
  background: var(--fd-bg-2);
  color: var(--fd-text-2);
  border-radius: 4px;
  cursor: pointer;
  flex-shrink: 0;
}
.fd-expand-btn:hover {
  background: var(--fd-bg-3);
  color: var(--fd-text-0);
}
.fd-expand-btn svg {
  width: 14px;
  height: 14px;
}
.fd-search-input-wrap.expanded .fd-expand-btn {
  width: 32px;
  height: 32px;
}
.fd-search-input-wrap.expanded .fd-expand-btn svg {
  width: 18px;
  height: 18px;
}
.fd-index-state { padding: 24px 0; text-align: center; }
.fd-state-box {
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: 12px;
  color: var(--fd-text-2);
  font-size: 13px;
}
.fd-state-box p { margin: 0; max-width: 440px; }
.fd-state-text { margin-left: 8px; color: var(--fd-text-2); }
.fd-state-box.failed { color: var(--fd-danger); }

.fd-results { margin-top: 12px; max-height: 52vh; overflow-y: auto; }
.fd-global-search-expanded .fd-results {
  max-height: 58vh;
  margin-top: 16px;
}
.fd-results-meta {
  font-size: 12px;
  color: var(--fd-text-2);
  margin-bottom: 6px;
}
.fd-result-list { display: flex; flex-direction: column; gap: 2px; }
.fd-result-item {
  display: flex;
  align-items: flex-start;
  gap: 10px;
  padding: 6px 8px;
  border-radius: 4px;
  cursor: pointer;
  transition: background 0.1s;
}
.fd-result-item:hover { background: var(--fd-bg-2); }
.fd-result-icon {
  width: 18px;
  height: 18px;
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
  background: rgba(0,122,204,0.35);
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
.fd-results-pagination { margin-top: 12px; }

.fd-modal-footer {
  display: flex;
  align-items: center;
  margin-top: 12px;
  padding-top: 10px;
  border-top: 1px solid var(--fd-border);
}
.fd-modal-footer-inline {
  margin-top: 10px;
  padding-top: 8px;
}
.fd-footer-meta { font-size: 11px; color: var(--fd-text-2); }
.fd-footer-spacer { flex: 1; }
.fd-footer-warn { color: var(--fd-danger); }
</style>

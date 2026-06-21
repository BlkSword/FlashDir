<template>
  <a-modal
    :open="visible"
    title="全局搜索"
    :width="660"
    :footer="null"
    :destroy-on-close="false"
    :class="['global-search-modal', isDark ? 'dark-modal' : '']"
    @cancel="close"
  >
    <!-- 搜索框 -->
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

    <!-- 索引未就绪 / 加载中 / 失败 -->
    <div v-if="!ready" class="index-state">
      <div v-if="stateKind === 'notLoaded'" class="state-box">
        <p>首次使用需建立全盘文件索引（扫描所有 NTFS 盘的 MFT，约几秒至几十秒）。</p>
        <a-button type="primary" :loading="loading" @click="ensureIndex">建立索引</a-button>
      </div>
      <div v-else-if="stateKind === 'loading'" class="state-box">
        <a-spin />
        <span class="state-text">
          正在扫描 {{ progress?.drive || stateData?.drive || '…' }}
          · 已索引 {{ (progress?.scanned ?? stateData?.scanned ?? 0) }} 项
        </span>
      </div>
      <div v-else-if="stateKind === 'failed'" class="state-box failed">
        <p>{{ failedReason }}</p>
        <a-button :loading="loading" @click="ensureIndex">重试</a-button>
      </div>
    </div>

    <!-- 结果 -->
    <div v-else class="results">
      <div class="results-meta" v-if="query && results.length > 0">
        找到 {{ results.length }} 项
      </div>
      <a-list :data-source="displayResults" size="small" :split="false">
        <template #renderItem="{ item }">
          <a-list-item class="result-item" @click="openTarget(item)">
            <a-list-item-meta>
              <template #avatar>
                <FolderOutlined v-if="item.isDir" class="ic-folder" />
                <FileOutlined v-else class="ic-file" />
              </template>
              <template #title>
                <span class="result-name" v-html="highlight(item.name)"></span>
              </template>
              <template #description>
                <span class="result-path">{{ item.path }}</span>
                <span class="result-size" v-if="!item.isDir"> · {{ formatSize(item.size) }}</span>
                <span class="result-mtime" v-if="item.mtime"> · {{ formatTime(item.mtime * 1000) }}</span>
              </template>
            </a-list-item-meta>
          </a-list-item>
        </template>
      </a-list>
      <a-pagination
        v-if="query && results.length > pageSize"
        v-model:current="currentPage"
        v-model:page-size="pageSize"
        :total="results.length"
        :page-size-options="['50', '100', '200', '500']"
        show-size-changer
        size="small"
        class="results-pagination"
      />
      <a-empty
        v-if="ready && query && results.length === 0 && !searching"
        :description="lastNoResultMsg"
        :image="emptyImage"
      />
    </div>

    <!-- 底部 -->
    <div class="modal-footer" v-if="ready">
      <span class="footer-meta" v-if="indexMeta">
        已索引 {{ (indexMeta.fileCount || 0) + (indexMeta.dirCount || 0) }} 项 · {{ indexMeta.driveCount || 0 }} 个盘
        <span v-if="indexMeta.failedDrives?.length" class="footer-warn">
          （{{ indexMeta.failedDrives.join(', ') }} 跳过）
        </span>
      </span>
      <span class="footer-spacer"></span>
      <a-button size="small" :loading="loading" @click="refreshIndex">刷新索引</a-button>
    </div>
  </a-modal>
</template>

<script setup>
import { ref, computed, watch, onMounted, onUnmounted, nextTick } from 'vue'
import { Empty } from 'ant-design-vue'
import { FolderOutlined, FileOutlined } from '@ant-design/icons-vue'
import { listen } from '@tauri-apps/api/event'
import { useTauri } from '../composables/useTauri'
import { formatSize, debounce, getParentPath } from '../utils/format.js'

const { invoke } = useTauri()
const emptyImage = Empty.PRESENTED_IMAGE_SIMPLE
const lastNoResultMsg = ref('无匹配文件')

const props = defineProps({
  visible: { type: Boolean, default: false },
  isDark: { type: Boolean, default: false }
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
    // 触发自动提权：仅索引完全失败时(scan_directory 会保证正常数据)
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

const close = () => emit('update:visible', false)

const escapeHtml = (s) =>
  String(s).replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;')

const highlight = (name) => {
  const q = query.value.trim()
  if (!q) return escapeHtml(name)

  // 提取纯文本 token（忽略 ext:zip 这类过滤关键字），用于高亮
  const tokens = q
    .split(/\s+/)
    .filter((t) => t && !t.includes(':') && !['AND', 'OR', 'NOT'].includes(t.toUpperCase()))
    .map((t) => t.toLowerCase())
  if (tokens.length === 0) return escapeHtml(name)

  // 收集所有匹配区间
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

  // 合并重叠区间
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

  // 构建 HTML
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
    }
  }
)
</script>

<style scoped>
.index-state {
  padding: 24px 0;
  text-align: center;
}
.state-box {
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: 12px;
  color: #595959;
  font-size: 13px;
}
.state-box p {
  margin: 0;
  max-width: 440px;
}
.state-text {
  margin-left: 8px;
  color: #595959;
}
.failed {
  color: #cf1322;
}

.results {
  margin-top: 12px;
  max-height: 52vh;
  overflow-y: auto;
}
.results-meta {
  font-size: 12px;
  color: #8c8c8c;
  margin-bottom: 6px;
}
.result-item {
  cursor: pointer;
  padding: 6px 8px !important;
  border-radius: 4px;
}
.result-item:hover {
  background: #f5f5f5;
}
.ic-folder {
  color: #faad14;
  font-size: 18px;
}
.ic-file {
  color: #8c8c8c;
  font-size: 18px;
}
.result-name {
  font-size: 13px;
  color: #262626;
}
.result-name :deep(mark) {
  background: #fff7be;
  color: #d4380d;
  border-radius: 2px;
  padding: 0 1px;
}
.result-path {
  font-family: 'Consolas', 'Monaco', monospace;
  font-size: 11px;
  color: #8c8c8c;
  word-break: break-all;
}
.result-size {
  font-size: 11px;
  color: #bfbfbf;
}
.result-mtime {
  font-size: 11px;
  color: #bfbfbf;
}

.results-pagination {
  margin-top: 12px;
}

.modal-footer {
  display: flex;
  align-items: center;
  margin-top: 12px;
  padding-top: 10px;
  border-top: 1px solid #f0f0f0;
}
.footer-meta {
  font-size: 11px;
  color: #8c8c8c;
}
.footer-spacer {
  flex: 1;
}

/* Dark mode overrides */
.global-search-modal.dark-modal .ant-modal-content,
.global-search-modal.dark-modal .ant-modal-header {
  background-color: #0f172a;
  color: #e2e8f0;
}
.global-search-modal.dark-modal .ant-modal-title {
  color: #e2e8f0;
}
.global-search-modal.dark-modal .ant-modal-close {
  color: #94a3b8;
}
.global-search-modal.dark-modal .ant-input {
  background-color: #1e293b;
  border-color: #334155;
  color: #e2e8f0;
}
.global-search-modal.dark-modal .ant-input::placeholder {
  color: #64748b;
}
.global-search-modal.dark-modal .result-item {
  border-bottom-color: #1e293b;
}
.global-search-modal.dark-modal .result-item:hover {
  background-color: #1e293b;
}
.global-search-modal.dark-modal .result-path {
  color: #64748b;
}
.global-search-modal.dark-modal .result-size,
.global-search-modal.dark-modal .result-mtime {
  color: #475569;
}
.global-search-modal.dark-modal .modal-footer {
  border-top-color: #1e293b;
}
.global-search-modal.dark-modal .footer-meta {
  color: #64748b;
}
</style>

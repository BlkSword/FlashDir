<script setup>
import { ref, reactive, computed, watch, onMounted, onUnmounted, nextTick } from 'vue'
import { invoke } from '@tauri-apps/api/core'
import { listen } from '@tauri-apps/api/event'

import TitleBar from './components/TitleBar.vue'
import Toolbar from './components/Toolbar.vue'
import DirTree from './components/DirTree.vue'
import FileTable from './components/FileTable.vue'
import Inspector from './components/Inspector.vue'
import InsightDock from './components/InsightDock.vue'
import StatusBar from './components/StatusBar.vue'
import Treemap from './components/Treemap.vue'
import SearchResults from './components/SearchResults.vue'
import CommandPalette from './components/CommandPalette.vue'
import ContextMenu from './components/ContextMenu.vue'
import UiModal from './components/UiModal.vue'
import Toasts from './components/Toasts.vue'
import Icon from './components/Icon.vue'

import { useTheme } from './composables/useTheme.js'
import { useToasts } from './composables/useToasts.js'
import { useGlobalSearch } from './composables/useGlobalSearch.js'
import { formatSize, formatDateTime, formatError, getParentPath, normalizePath } from './utils/format.js'
import { searchGlobal } from './utils/globalSearchApi.js'

/* ── 状态 ───────────────────────────────────────────────── */
const currentPath = ref('')
const items = ref([])
const totalItems = ref(0)
const totalSize = ref(0)
const fileCount = ref(0)
const dirCount = ref(0)
const topFiles = ref([])
const scanTime = ref(0)
const cacheSource = ref('')
const mftAvailable = ref(false)
const isAdmin = ref(false)
const loading = ref(false)
const filter = ref('')
const sortConfig = ref({ column: 'size', direction: 'desc' })
const page = ref(1)
const pageSize = ref(200)
const view = ref('list')
const dockTab = ref('big')
const selectedItem = ref(null)
const treeVisible = ref(true)
const inspVisible = ref(true)
const dockVisible = ref(true)
const paletteOpen = ref(false)
const diagnosticsOpen = ref(false)
const diagnosticsText = ref('')
const aboutOpen = ref(false)
const snapshotCount = ref(0)
const scanPhase = ref({ phase: '', message: '' })
const history = ref([])
const navStack = ref([])
const navIndex = ref(-1)

const ctx = reactive({ open: false, x: 0, y: 0, items: [], target: null })

/* ── 全局搜索（工具栏回车 / 命令面板 / 右键"全局搜索同名文件"） ── */
const searchQuery = ref('')
const searchResults = ref([])
const searching = ref(false)
const searchElapsed = ref(0)
const searchTotal = ref(0)
const searchTruncated = ref(false)
const searchLoadingMore = ref(false)
const searchExporting = ref(false)
/* MCP（AI 客户端接入）状态与配置 */
const mcpStatus = ref(null)
const mcpConfigOpen = ref(false)
const mcpConfigHttp = ref('')
const mcpConfigStdio = ref('')
const mcpUrl = ref('')
const paletteSeed = ref('')

/** 每页加载条数（"加载更多"步长） */
const SEARCH_PAGE = 1000
/** 导出上限：避免一次性把几十万条塞进内存/CSV */
const SEARCH_EXPORT_MAX = 50000

async function runGlobalSearch(q, { append = false } = {}) {
  const query = (q || '').trim()
  if (!query) {
    toasts.warn('请输入搜索关键字')
    return
  }
  if (!append) {
    view.value = 'search'
    searchQuery.value = query
    searchResults.value = []
    searchTotal.value = 0
    searchTruncated.value = false
  }
  const offset = append ? searchResults.value.length : 0
  if (append) searchLoadingMore.value = true
  else searching.value = true

  const t0 = performance.now()
  try {
    const res = await searchGlobal(query, { limit: SEARCH_PAGE, offset })
    if (append) searchResults.value = searchResults.value.concat(res.results)
    else searchResults.value = res.results
    searchTotal.value = res.total
    searchTruncated.value = res.truncated
    searchElapsed.value = Math.round(performance.now() - t0)

    if (!res.ready) {
      toasts.warn('全局索引尚未就绪，正在后台构建，稍后重试')
      gs.ensureIndex().catch(() => {})
    } else if (!append && !res.results.length && res.indexSize) {
      toasts.info('索引中有 ' + res.indexSize.toLocaleString() + ' 项，但没有匹配“' + query + '”')
    }
  } catch (e) {
    const msg = formatError(e)
    toasts.err('搜索失败：' + msg)
    if (/索引|index/i.test(msg)) gs.ensureIndex().catch(() => {})
  } finally {
    searching.value = false
    searchLoadingMore.value = false
  }
}

/** 加载下一页搜索结果 */
function loadMoreSearch() {
  if (searchLoadingMore.value || !searchTruncated.value) return
  runGlobalSearch(searchQuery.value, { append: true })
}

/** 导出全部命中（上限 5 万条）为 CSV */
async function exportSearchCsv() {
  const query = searchQuery.value
  if (!query) return
  searchExporting.value = true
  try {
    const want = Math.min(searchTotal.value || SEARCH_EXPORT_MAX, SEARCH_EXPORT_MAX)
    const res = await searchGlobal(query, { limit: Math.max(1, want), offset: 0 })
    const rows = res.results
    if (!rows.length) {
      toasts.warn('没有可导出的结果')
      return
    }
    const esc = (v) => `"${String(v ?? '').replace(/"/g, '""')}"`
    const lines = ['名称,大小(字节),类型,修改时间,完整路径']
    for (const r of rows) {
      lines.push([esc(r.name), r.size, r.isDir ? '目录' : '文件', esc(formatDateTime(r.mtime)), esc(r.path)].join(','))
    }
    const blob = new Blob(['\ufeff' + lines.join('\r\n')], { type: 'text/csv;charset=utf-8' })

    const a = document.createElement('a')
    a.href = URL.createObjectURL(blob)
    a.download = `flashdir-search-${Date.now()}.csv`
    a.click()
    URL.revokeObjectURL(a.href)
    toasts.ok(
      '已导出 ' + rows.length.toLocaleString() + ' 条' +
      (res.total > rows.length ? '（共 ' + res.total.toLocaleString() + ' 条，已达上限 ' + SEARCH_EXPORT_MAX.toLocaleString() + '）' : '')
    )
  } catch (e) {
    toasts.err('导出失败：' + formatError(e))
  } finally {
    searchExporting.value = false
  }
}

function openSearchResult(path) {
  invoke('open_path', { path }).catch((e) => toasts.err('打开失败：' + formatError(e)))
}

const { mode: themeMode, cycle: cycleTheme } = useTheme()
const toasts = useToasts()
const gs = useGlobalSearch()

const volumes = ref([])
const currentDrive = computed(() => {
  const m = (currentPath.value || '').match(/^([A-Za-z]):/)
  return m ? m[1].toUpperCase() : ''
})
const usnVerified = computed(() =>
  mftAvailable.value && ['usn', 'usn-disk', 'scan'].includes(cacheSource.value)
)
const parentTotal = computed(() => totalSize.value)
const scopeLabel = computed(() => (currentDrive.value ? currentDrive.value + ': ' : ''))

async function loadMcpStatus() {
  try {
    mcpStatus.value = await invoke('get_mcp_status')
  } catch (e) {
    mcpStatus.value = null // 未编译 MCP 特性时静默隐藏
  }
}

async function openMcpConfig() {
  mcpConfigOpen.value = true
  mcpConfigHttp.value = '正在获取…'
  try {
    const cfg = await invoke('get_mcp_config')
    mcpConfigHttp.value = cfg.configHttp || ''
    mcpConfigStdio.value = cfg.configStdio || ''
    mcpUrl.value = cfg.url || ''
  } catch (e) {
    mcpConfigHttp.value = '获取失败：' + formatError(e)
  }
}

/* ── 卷信息 ─────────────────────────────────────────────── */
async function refreshVolumes() {
  try {
    volumes.value = (await invoke('get_volumes')) || []
  } catch (e) {
    volumes.value = []
  }
}

/* ── 扫描 / 分页 ─────────────────────────────────────────── */
async function loadPage({ record = false, force = false } = {}) {
  if (!currentPath.value) return
  loading.value = true
  const t0 = performance.now()
  try {
    const res = await invoke('scan_directory_paged', {
      path: currentPath.value,
      forceRefresh: force,
      page: page.value,
      pageSize: pageSize.value,
      sortColumn: sortConfig.value.column,
      sortDirection: sortConfig.value.direction,
      filter: filter.value.trim(),
      recordHistory: record,
    })
    items.value = res.items || []
    totalItems.value = res.totalItems || 0
    totalSize.value = res.totalSize || 0
    fileCount.value = res.fileCount || 0
    dirCount.value = res.dirCount || 0
    topFiles.value = res.topFiles || []
    cacheSource.value = res.cacheSource || 'scan'
    mftAvailable.value = !!res.mftAvailable
    scanTime.value = parseFloat(((performance.now() - t0) / 1000).toFixed(2))
    if (!items.value.some((i) => i.path === selectedItem.value?.path)) selectedItem.value = null
  } catch (e) {
    toasts.err('扫描失败：' + formatError(e))
    items.value = []
    totalItems.value = 0
  } finally {
    loading.value = false
  }
}

async function scanPath(path, { force = false, record = true } = {}) {
  const p = normalizePath((path || '').trim())
  if (!p) {
    toasts.warn('请输入有效路径')
    return
  }
  if (record) {
    navStack.value = navStack.value.slice(0, navIndex.value + 1)
    navStack.value.push(p)
    navIndex.value = navStack.value.length - 1
  }
  currentPath.value = p
  page.value = 1
  selectedItem.value = null
  scanPhase.value = { phase: '', message: '' }
  await loadPage({ record, force })
  refreshVolumes()
  loadSnapshotCount()
  // 全局索引增量追加放后台
  invoke('global_search_add_scan_from_cache', { path: p }).catch(() => {})
}

function navigate(path) {
  if (!path) return
  if (normalizePath(path) === currentPath.value && !loading.value) return
  scanPath(path)
}
function goUp() {
  const parent = getParentPath(currentPath.value)
  if (parent && parent !== currentPath.value) navigate(parent)
}
function goBack() {
  if (navIndex.value > 0) {
    navIndex.value -= 1
    scanPath(navStack.value[navIndex.value], { record: false })
  }
}
function goForward() {
  if (navIndex.value < navStack.value.length - 1) {
    navIndex.value += 1
    scanPath(navStack.value[navIndex.value], { record: false })
  }
}
function refresh() {
  loadPage({ force: false })
}
function forceRescan() {
  loadPage({ force: true })
}
async function cancelScan() {
  try {
    await invoke('cancel_scan')
    toasts.info('正在取消扫描…')
  } catch (e) {
    toasts.err('取消失败：' + formatError(e))
  }
}

/* ── 排序 / 过滤 ─────────────────────────────────────────── */
function onSort(column) {
  if (sortConfig.value.column === column) {
    sortConfig.value.direction = sortConfig.value.direction === 'desc' ? 'asc' : 'desc'
  } else {
    sortConfig.value.column = column
    sortConfig.value.direction = column === 'name' ? 'asc' : 'desc'
  }
  page.value = 1
  loadPage()
}
let filterTimer = null
function onFilterInput(v) {
  filter.value = v
  clearTimeout(filterTimer)
  filterTimer = setTimeout(() => {
    page.value = 1
    loadPage()
  }, 220)
}
function changePage(p) {
  page.value = p
  loadPage()
}
function changePageSize(n) {
  pageSize.value = n
  page.value = 1
  loadPage()
}
function searchHere(path) {
  const name = normalizePath(path || '').split('/').filter(Boolean).pop()
  if (!name) return
  filter.value = `dir:${name}`
  page.value = 1
  loadPage()
}

/* ── 条目操作（只读：跳转 / 复制 / 打开） ─────────────────── */
async function openItem(item) {
  try {
    await invoke('open_path', { path: item.path })
  } catch (e) {
    toasts.err('打开失败：' + formatError(e))
  }
}
async function copyText(text) {
  try {
    await navigator.clipboard.writeText(text)
    toasts.ok('已复制：' + (text.length > 60 ? text.slice(0, 60) + '…' : text))
  } catch (e) {
    toasts.err('复制失败：' + formatError(e))
  }
}
function exportCsv() {
  if (!items.value.length) {
    toasts.warn('当前页没有可导出的数据')
    return
  }
  const esc = (s) => `"${String(s ?? '').replace(/"/g, '""')}"`
  const lines = ['名称,大小(字节),类型,修改时间,访问时间,完整路径']
  for (const it of items.value) {
    lines.push([
      esc(it.name), it.size, it.isDir ? '目录' : '文件',
      esc(formatDateTime(it.mtime)), esc(formatDateTime(it.atime)), esc(it.path),
    ].join(','))
  }
  const blob = new Blob(['\ufeff' + lines.join('\r\n')], { type: 'text/csv;charset=utf-8' })
  const a = document.createElement('a')
  a.href = URL.createObjectURL(blob)
  a.download = `flashdir-${(currentPath.value.split('/').pop() || 'export')}-p${page.value}.csv`
  a.click()
  URL.revokeObjectURL(a.href)
  toasts.ok(`已导出当前页 ${items.value.length} 行`)
}

/* ── 右键菜单 ───────────────────────────────────────────── */
function openContext({ item, event }) {
  ctx.target = item
  ctx.items = [
    { id: 'open', label: '打开所在位置', icon: 'folder-open' },
    { id: 'copy', label: '复制完整路径', icon: 'copy' },
    { id: 'copyName', label: '复制文件名', icon: 'copy' },
    { sep: true },
    { id: 'searchHere', label: '在此目录内过滤', icon: 'filter', disabled: !item.isDir },
    { id: 'globalSearch', label: '全局搜索同名文件', icon: 'search' },
    { id: 'dupes', label: '检测重复文件', icon: 'dupes', disabled: item.isDir },
    { sep: true },
    { id: 'snapshot', label: '保存目录快照', icon: 'clock', disabled: !item.isDir },
  ]
  ctx.x = event.clientX
  ctx.y = event.clientY
  ctx.open = true
}
function onContextPick(id) {
  const item = ctx.target
  if (!item) return
  if (id === 'open') openItem(item)
  else if (id === 'copy') copyText(item.path)
  else if (id === 'copyName') copyText(item.name)
  else if (id === 'searchHere') searchHere(item.path)
  else if (id === 'globalSearch') { paletteSeed.value = item.name; paletteOpen.value = true }
  else if (id === 'dupes') { dockTab.value = 'dupes'; dockVisible.value = true }
  else if (id === 'snapshot') saveSnapshot(currentPath.value)
}

/* ── 快照 / 诊断 / 管理员 ───────────────────────────────── */
async function loadSnapshotCount() {
  try {
    const list = await invoke('list_snapshots', { path: currentPath.value })
    snapshotCount.value = (list || []).length
  } catch (e) {
    snapshotCount.value = 0
  }
}
async function saveSnapshot(path) {
  try {
    await invoke('save_snapshot_from_cache', { path })
    toasts.ok('已保存快照')
    loadSnapshotCount()
  } catch (e) {
    toasts.err('保存快照失败：' + formatError(e))
  }
}
async function showDiagnostics() {
  diagnosticsOpen.value = true
  diagnosticsText.value = '正在获取诊断信息…'
  try {
    diagnosticsText.value = JSON.stringify(await invoke('get_diagnostics'), null, 2)
  } catch (e) {
    diagnosticsText.value = '获取失败：' + formatError(e)
  }
}
async function restartAsAdmin() {
  try {
    await invoke('restart_as_admin')
  } catch (e) {
    toasts.err('重启失败：' + formatError(e))
  }
}
async function indexAction() {
  try {
    if (gs.ready.value) await gs.refreshIndex()
    else await gs.ensureIndex()
    toasts.ok('已触发索引构建/刷新')
  } catch (e) {
    toasts.err('索引操作失败：' + formatError(e))
  }
}

/* ── 命令面板 ───────────────────────────────────────────── */
const commands = computed(() => [
  { id: 'scan', label: '扫描当前目录（强制刷新）', icon: 'scan', keywords: 'scan rescan', shortcut: 'Enter' },
  { id: 'refresh', label: '刷新（USN 快路径）', icon: 'refresh', keywords: 'refresh usn', shortcut: 'F5' },
  { id: 'force', label: '强制全量重扫（忽略缓存）', icon: 'zap', keywords: 'force full', shortcut: 'Ctrl+Shift+R' },
  { id: 'cancel', label: '取消当前扫描', icon: 'xc', keywords: 'cancel stop', shortcut: 'Esc' },
  { id: 'up', label: '上一级目录', icon: 'up', keywords: 'up parent', shortcut: 'Backspace' },
  { id: 'index', label: gs.ready.value ? '刷新全局索引' : '建立全局索引', icon: 'search', keywords: 'index global' },
  { id: 'search-global', label: '用当前关键字做全局搜索', icon: 'search', keywords: 'search global find', shortcut: 'Enter' },
  { id: 'snapshot', label: '保存当前目录快照', icon: 'clock', keywords: 'snapshot save' },
  { id: 'dupes', label: '重复文件检测', icon: 'dupes', keywords: 'duplicate same' },
  { id: 'dev', label: '开发缓存分析', icon: 'dev', keywords: 'node_modules target cache' },
  { id: 'map', label: '切换主视图：热图', icon: 'grid', keywords: 'treemap map' },
  { id: 'export', label: '导出当前页 CSV', icon: 'tray', keywords: 'export csv' },
  { id: 'theme', label: '切换主题（跟随系统 / 深色 / 浅色）', icon: 'sun', keywords: 'theme dark light' },
  { id: 'diagnostics', label: '运行诊断', icon: 'info', keywords: 'diagnostics debug' },
  { id: 'mcp-config', label: 'MCP：复制 AI 客户端配置（Claude Desktop / Cursor）', icon: 'dev', keywords: 'mcp ai claude cursor config' },
  { id: 'admin', label: '以管理员重启（启用 MFT / USN）', icon: 'shield', keywords: 'admin elevate mft' },
  { id: 'about', label: '关于 FlashDir', icon: 'info', keywords: 'about version' },
])
function runCommand(id) {
  const map = {
    scan: () => loadPage({ record: true, force: true }),
    refresh: () => refresh(),
    force: () => forceRescan(),
    cancel: () => cancelScan(),
    up: () => goUp(),
    index: () => indexAction(),
    'search-global': () => runGlobalSearch(filter.value),
    snapshot: () => saveSnapshot(currentPath.value),
    dupes: () => { dockTab.value = 'dupes'; dockVisible.value = true },
    dev: () => { dockTab.value = 'dev'; dockVisible.value = true },
    map: () => { view.value = view.value === 'map' ? 'list' : 'map' },
    export: () => exportCsv(),
    theme: () => cycleTheme(),
    diagnostics: () => showDiagnostics(),
    'mcp-config': () => openMcpConfig(),
    admin: () => restartAsAdmin(),
    about: () => { aboutOpen.value = true },
  }
  map[id]?.()
}

/* ── 键盘 ───────────────────────────────────────────────── */
const dockTabs = ['big', 'growth', 'dupes', 'snapshot', 'dev']
function onKeydown(e) {
  const mod = e.ctrlKey || e.metaKey
  const inInput = ['INPUT', 'TEXTAREA', 'SELECT'].includes(document.activeElement?.tagName)

  if (mod && (e.key === 'k' || e.key === 'K')) { e.preventDefault(); paletteOpen.value = true; return }
  if (e.key === 'Escape') {
    if (paletteOpen.value) { paletteOpen.value = false; return }
    if (diagnosticsOpen.value) { diagnosticsOpen.value = false; return }
    if (aboutOpen.value) { aboutOpen.value = false; return }
    if (ctx.open) { ctx.open = false; return }
    if (loading.value) { cancelScan(); return }
    return
  }
  if (inInput) return

  if (mod && e.shiftKey && (e.key === 'R' || e.key === 'r')) { e.preventDefault(); forceRescan(); return }
  if (e.key === 'F5') { e.preventDefault(); refresh(); return }
  if (mod && (e.key === 'f' || e.key === 'F')) {
    e.preventDefault()
    document.querySelector('.filter-box input')?.focus()
    return
  }
  if (mod && (e.key === 'b' || e.key === 'B')) { e.preventDefault(); treeVisible.value = !treeVisible.value; return }
  if (mod && (e.key === 'j' || e.key === 'J')) { e.preventDefault(); dockVisible.value = !dockVisible.value; return }
  if (mod && (e.key === 'i' || e.key === 'I')) { e.preventDefault(); inspVisible.value = !inspVisible.value; return }
  if (mod && /^[1-5]$/.test(e.key)) {
    e.preventDefault()
    dockVisible.value = true
    dockTab.value = dockTabs[Number(e.key) - 1]
  }
}

/* ── 生命周期 ───────────────────────────────────────────── */
let unlistenPhase = null
let mcpTimer = null
async function loadHistory() {
  try { history.value = (await invoke('get_history_summary')) || [] } catch (e) { history.value = [] }
}

onMounted(async () => {
  document.addEventListener('keydown', onKeydown)
  try {
    unlistenPhase = await listen('scan-phase', (ev) => {
      scanPhase.value = ev.payload || { phase: '', message: '' }
    })
  } catch (e) {
    // 非 Tauri 环境或事件系统异常：不影响其余初始化
    unlistenPhase = null
  }
  refreshVolumes()
  loadHistory()
  loadMcpStatus()
  mcpTimer = setInterval(loadMcpStatus, 3000)
  try { isAdmin.value = await invoke('is_admin') } catch (e) { isAdmin.value = false }
})

onUnmounted(() => {
  if (mcpTimer) clearInterval(mcpTimer)
  document.removeEventListener('keydown', onKeydown)
  if (unlistenPhase) unlistenPhase()
})

watch(loading, (v) => { if (!v) scanPhase.value = { phase: '', message: '' } })
</script>

<template>
  <svg style="display:none" aria-hidden="true">
    <symbol id="i-drive" viewBox="0 0 16 16"><rect x="1.5" y="3.5" width="13" height="9" rx="1"/><path d="M1.5 9.5h13"/><circle cx="12" cy="11.5" r=".7"/></symbol>
    <symbol id="i-folder" viewBox="0 0 16 16"><path d="M1.5 3.5h4l1.2 1.5h7.8v7.5h-13z"/></symbol>
    <symbol id="i-folder-open" viewBox="0 0 16 16"><path d="M1.5 3.5h4l1.2 1.5h6.8v2"/><path d="M1.5 5.5h13l-1.6 7h-11.4z"/></symbol>
    <symbol id="i-file" viewBox="0 0 16 16"><path d="M3.5 1.5h6l3 3v10h-9z"/><path d="M9.5 1.5v3h3"/></symbol>
    <symbol id="i-search" viewBox="0 0 16 16"><circle cx="7" cy="7" r="4.5"/><path d="M10.5 10.5L14 14"/></symbol>
    <symbol id="i-scan" viewBox="0 0 16 16"><path d="M2 8a6 6 0 0 1 12 0"/><path d="M8 8l3.5-3.5"/><circle cx="8" cy="8" r="1"/></symbol>
    <symbol id="i-zap" viewBox="0 0 16 16"><path d="M9 1.5L3.5 9h4L7 14.5 12.5 7h-4z"/></symbol>
    <symbol id="i-x" viewBox="0 0 16 16"><path d="M4 4l8 8M12 4l-8 8"/></symbol>
    <symbol id="i-xc" viewBox="0 0 16 16"><circle cx="8" cy="8" r="6"/><path d="M5.8 5.8l4.4 4.4M10.2 5.8l-4.4 4.4"/></symbol>
    <symbol id="i-refresh" viewBox="0 0 16 16"><path d="M13.5 8a5.5 5.5 0 1 1-1.8-4.1"/><path d="M13.5 2v3.5H10"/></symbol>
    <symbol id="i-filter" viewBox="0 0 16 16"><path d="M2 4h12M4.5 8h7M6.5 12h3"/></symbol>
    <symbol id="i-right" viewBox="0 0 16 16"><path d="M6 3.5L10.5 8 6 12.5"/></symbol>
    <symbol id="i-left" viewBox="0 0 16 16"><path d="M10 3.5L5.5 8 10 12.5"/></symbol>
    <symbol id="i-down" viewBox="0 0 16 16"><path d="M3.5 6L8 10.5 12.5 6"/></symbol>
    <symbol id="i-up" viewBox="0 0 16 16"><path d="M3.5 10L8 5.5 12.5 10"/></symbol>
    <symbol id="i-grid" viewBox="0 0 16 16"><rect x="2" y="2" width="5" height="5"/><rect x="9" y="2" width="5" height="3"/><rect x="2" y="9" width="5" height="5"/><rect x="9" y="7" width="5" height="7"/></symbol>
    <symbol id="i-diff" viewBox="0 0 16 16"><path d="M4 2.5v11M12 2.5v11M4 6h3M9 10h3"/></symbol>
    <symbol id="i-dupes" viewBox="0 0 16 16"><rect x="2.5" y="2.5" width="8" height="8"/><path d="M5.5 13.5h8v-8"/></symbol>
    <symbol id="i-dev" viewBox="0 0 16 16"><path d="M6 3.5L2.5 8 6 12.5M10 3.5L13.5 8 10 12.5"/></symbol>
    <symbol id="i-cog" viewBox="0 0 16 16"><circle cx="8" cy="8" r="2.2"/><path d="M8 1.8v1.6M8 12.6v1.6M1.8 8h1.6M12.6 8h1.6M3.6 3.6l1.1 1.1M11.3 11.3l1.1 1.1M12.4 3.6l-1.1 1.1M4.7 11.3l-1.1 1.1"/></symbol>
    <symbol id="i-tray" viewBox="0 0 16 16"><path d="M8 2v7.5M5 6.5L8 9.5l3-3M2.5 13.5h11"/></symbol>
    <symbol id="i-info" viewBox="0 0 16 16"><circle cx="8" cy="8" r="6"/><path d="M8 7v4M8 5.2v.6"/></symbol>
    <symbol id="i-clock" viewBox="0 0 16 16"><circle cx="8" cy="8" r="6"/><path d="M8 4.5V8l2.5 1.5"/></symbol>
    <symbol id="i-shield" viewBox="0 0 16 16"><path d="M8 1.8l5 2v4c0 3-2.2 5.4-5 6.4-2.8-1-5-3.4-5-6.4v-4z"/></symbol>
    <symbol id="i-copy" viewBox="0 0 16 16"><rect x="5.5" y="5.5" width="8" height="8"/><path d="M2.5 10.5v-8h8"/></symbol>
    <symbol id="i-pencil" viewBox="0 0 16 16"><path d="M11 2.5l2.5 2.5L6 12.5 2.5 13.5 3.5 10z"/></symbol>
    <symbol id="i-check" viewBox="0 0 16 16"><path d="M3 8.5l3.5 3.5L13 5"/></symbol>
    <symbol id="i-warn" viewBox="0 0 16 16"><path d="M8 2.5l6 11H2z"/><path d="M8 6.5v3.5M8 11.6v.4"/></symbol>
    <symbol id="i-tree" viewBox="0 0 16 16"><path d="M3 3h5M3 3v9h5M6 7.5h4M6 12h4"/></symbol>
    <symbol id="i-panel" viewBox="0 0 16 16"><rect x="2" y="3" width="12" height="10"/><path d="M10 3v10"/></symbol>
    <symbol id="i-dock" viewBox="0 0 16 16"><rect x="2" y="3" width="12" height="10"/><path d="M2 9.5h12"/></symbol>
    <symbol id="i-sun" viewBox="0 0 16 16"><circle cx="8" cy="8" r="3"/><path d="M8 1.5v1.6M8 12.9v1.6M1.5 8h1.6M12.9 8h1.6M3.5 3.5l1.1 1.1M11.4 11.4l1.1 1.1M12.5 3.5l-1.1 1.1M4.6 11.4l-1.1 1.1"/></symbol>
    <symbol id="i-moon" viewBox="0 0 16 16"><path d="M13 10.5A5.5 5.5 0 0 1 5.5 3a5.5 5.5 0 1 0 7.5 7.5z"/></symbol>
    <symbol id="i-auto" viewBox="0 0 16 16"><circle cx="8" cy="8" r="6"/><path d="M8 2v12"/><path d="M8 2a6 6 0 0 1 0 12z" fill="currentColor" stroke="none"/></symbol>
    <symbol id="i-min" viewBox="0 0 16 16"><path d="M3 8h10"/></symbol>
    <symbol id="i-max" viewBox="0 0 16 16"><rect x="3.5" y="3.5" width="9" height="9"/></symbol>
    <symbol id="i-grip" viewBox="0 0 16 16"><path d="M3 6h10M3 10h10"/></symbol>
  </svg>

  <div class="fd-app" :class="{ 'no-dock': !dockVisible }">
    <TitleBar
      :volumes="volumes"
      :current-drive="currentDrive"
      :theme="themeMode"
      :tree-visible="treeVisible"
      :insp-visible="inspVisible"
      :dock-visible="dockVisible"
      @pick-volume="navigate($event.letter + ':/')"
      @command="paletteOpen = true"
      @cycle-theme="cycleTheme"
      @toggle-tree="treeVisible = !treeVisible"
      @toggle-insp="inspVisible = !inspVisible"
      @toggle-dock="dockVisible = !dockVisible"
    />

    <Toolbar
      :path="currentPath"
      :loading="loading"
      :filter="filter"
      :hits="totalItems"
      :total-items="totalItems"
      :total-size="totalSize"
      :view="view"
      :can-back="navIndex > 0"
      :can-forward="navIndex < navStack.length - 1"
      :can-up="!!currentPath && getParentPath(currentPath) !== currentPath"
      @scan="scanPath($event)"
      @force-scan="forceRescan"
      @cancel-scan="cancelScan"
      @refresh="refresh"
      @navigate="navigate"
      @update:filter="onFilterInput"
      @update:view="view = $event === 'search' ? 'list' : $event"
      @export="exportCsv"
      @search-global="runGlobalSearch"
      @up="goUp"
      @back="goBack"
      @forward="goForward"
    />

    <div v-if="loading && (scanPhase.message || scanPhase.phase)" class="scan-strip">
      <span class="dot warn" />
      <span>{{ scanPhase.message || scanPhase.phase }}</span>
      <span class="track">
        <i :class="{ indet: typeof scanPhase.progress !== 'number' }" :style="typeof scanPhase.progress === 'number' ? { width: Math.min(100, Math.max(2, scanPhase.progress * 100)) + '%' } : {}" />
      </span>
      <button class="btn ghost" style="height:18px" @click="cancelScan">取消</button>
    </div>

    <div class="fd-body" :class="{ 'tree-collapsed': !treeVisible, 'insp-collapsed': !inspVisible }">
      <DirTree
        v-if="treeVisible"
        :root-path="currentPath"
        :selected-path="currentPath"
        :volumes="volumes"
        :history="history"
        @navigate="navigate"
        @error="toasts.err($event)"
      />

      <SearchResults
        v-if="view === 'search'"
        :query="searchQuery"
        :results="searchResults"
        :total="searchTotal"
        :truncated="searchTruncated"
        :loading="searching"
        :loading-more="searchLoadingMore"
        :exporting="searchExporting"
        :elapsed-ms="searchElapsed"
        :index-count="gs.indexMeta.value ? (gs.indexMeta.value.fileCount || 0) + (gs.indexMeta.value.dirCount || 0) : 0"
        :index-partial="!!gs.indexMeta.value?.partial"
        :scope="scopeLabel"
        @open="openSearchResult"
        @navigate="navigate"
        @back="view = 'list'"
        @retry="runGlobalSearch(searchQuery)"
        @copy="copyText"
        @more="loadMoreSearch"
        @export-all="exportSearchCsv"
      />

      <FileTable
        v-else-if="view === 'list'"
        :items="items"
        :total-size="totalSize"
        :parent-total="parentTotal"
        :total-items="totalItems"
        :loading="loading"
        :sort-config="sortConfig"
        :page="page"
        :page-size="pageSize"
        :filter="filter"
        :selected-path="selectedItem?.path || ''"
        @sort="onSort"
        @select="selectedItem = $event"
        @open="openItem"
        @context="openContext"
        @page-change="changePage"
        @page-size-change="changePageSize"
        @up="goUp"
      />

      <div v-else class="table-wrap" style="padding:10px;overflow:auto">
        <Treemap :items="items" :total="totalSize" @navigate="navigate" />
      </div>

      <Inspector
        v-if="inspVisible"
        :item="selectedItem"
        :parent-path="currentPath"
        :parent-total="parentTotal"
        :top-files="topFiles"
        :access-reliable="true"
        :cache-source="cacheSource"
        :mft-available="mftAvailable"
        :snapshot-count="snapshotCount"
        @open="openItem"
        @copy="copyText"
        @search-here="searchHere"
        @duplicates="dockTab = 'dupes'; dockVisible = true"
        @snapshot="saveSnapshot"
      />
    </div>

    <InsightDock
      v-if="dockVisible"
      :tab="dockTab"
      :items="items"
      :top-files="topFiles"
      :total-size="totalSize"
      :current-path="currentPath"
      @update:tab="dockTab = $event"
      @navigate="navigate"
      @open="openItem"
      @refresh="loadPage"
    />

    <StatusBar
      :path="currentPath"
      :total-items="totalItems"
      :total-size="totalSize"
      :file-count="fileCount"
      :dir-count="dirCount"
      :scan-time="scanTime"
      :cache-source="cacheSource"
      :mft-available="mftAvailable"
      :is-admin="isAdmin"
      :index-count="gs.indexMeta.value ? (gs.indexMeta.value.fileCount || 0) + (gs.indexMeta.value.dirCount || 0) : 0"
      :index-partial="!!gs.indexMeta.value?.partial"
      :usn-verified="usnVerified"
      :filter="filter"
      :selected="selectedItem"
      :mcp="mcpStatus"
      @mcp-config="openMcpConfig"
      @restart-admin="restartAsAdmin"
      @index-action="indexAction"
      @diagnostics="showDiagnostics"
    />

    <CommandPalette
      :open="paletteOpen"
      :seed="paletteSeed"
      :commands="commands"
      :index-count="gs.indexMeta.value ? (gs.indexMeta.value.fileCount || 0) + (gs.indexMeta.value.dirCount || 0) : 0"
      :index-partial="!!gs.indexMeta.value?.partial"
      :scope="scopeLabel"
      @close="paletteOpen = false"
      @run="runCommand"
      @open-full="runGlobalSearch($event)"
      @open-path="(p) => invoke('open_path', { path: p }).catch((e) => toasts.err('打开失败：' + formatError(e)))"
      @navigate="navigate"
    />

    <ContextMenu :open="ctx.open" :x="ctx.x" :y="ctx.y" :items="ctx.items" @close="ctx.open = false" @pick="onContextPick" />

    <UiModal :open="diagnosticsOpen" title="运行诊断" width="820px" @close="diagnosticsOpen = false">
      <pre class="mono-block">{{ diagnosticsText }}</pre>
    </UiModal>

    <UiModal :open="aboutOpen" title="关于 FlashDir" width="560px" @close="aboutOpen = false">
      <div style="display:flex;gap:12px;align-items:flex-start">
        <Icon name="drive" :size="28" style="color:var(--accent)" />
        <div>
          <div style="font-weight:600;margin-bottom:4px">FlashDir · 磁盘可观测性</div>
          <p class="section-note" style="margin:0 0 8px">
            MFT 直读扫描、USN 增量刷新、快照对比、重复文件检测、开发缓存分析与跨盘全局搜索。
            扫描全程只读，不修改、不删除任何文件。
          </p>
          <div class="section-note">
            快捷键：<kbd>Ctrl</kbd>+<kbd>K</kbd> 命令与文件搜索 ·
            <kbd>Ctrl</kbd>+<kbd>F</kbd> 过滤 ·
            <kbd>F5</kbd> 增量刷新 ·
            <kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>R</kbd> 强制全量 ·
            <kbd>Ctrl</kbd>+<kbd>B</kbd>/<kbd>J</kbd>/<kbd>I</kbd> 面板开关 ·
            <kbd>Ctrl</kbd>+<kbd>1..6</kbd> 洞察标签
          </div>
        </div>
      </div>
    </UiModal>

    <UiModal :open="mcpConfigOpen" title="MCP：让 AI 客户端接入 FlashDir" width="760px" @close="mcpConfigOpen = false">
      <div class="section-note" style="line-height:1.9">
        加到 <b>Claude Desktop</b>（<span class="mono">%APPDATA%\Claude\claude_desktop_config.json</span>）
        或 <b>Cursor</b>（<span class="mono">~/.cursor/mcp.json</span>）后重启客户端。
        两种方式都**复用桌面端的索引与扫描缓存**，并继承它的管理员权限（MFT 直读）。
      </div>

      <div class="insp-h">方式一：HTTP 地址（推荐 · 端口固定，配置跨机器一致）</div>
      <pre class="mono-block" style="max-height:150px">{{ mcpConfigHttp }}</pre>
      <div class="chips">
        <span class="chip" @click="copyText(mcpConfigHttp)"><Icon name="copy" />复制 HTTP 配置</span>
        <span class="chip" @click="copyText(mcpUrl)"><Icon name="copy" />只复制地址</span>
      </div>

      <div class="insp-h">方式二：stdio 命令（兼容只支持 command 的 Host）</div>
      <pre class="mono-block" style="max-height:170px">{{ mcpConfigStdio }}</pre>
      <div class="chips">
        <span class="chip" @click="copyText(mcpConfigStdio)"><Icon name="copy" />复制 stdio 配置</span>
        <span class="chip" @click="mcpConfigOpen = false">关闭</span>
      </div>

      <div class="section-note" style="margin-top:10px">
        工具：list_volumes · search_files · scan_directory · list_directory · cache_stats · diagnostics（全部只读）
        <template v-if="mcpStatus && mcpStatus.endpoint">
          <br />当前端点：<span class="mono">127.0.0.1:{{ mcpStatus.endpoint.port }}</span>
          （PID {{ mcpStatus.endpoint.pid }}）· 客户端：{{ mcpStatus.client || '未连接' }}
          · 累计调用 {{ mcpStatus.calls }} 次
        </template>
      </div>
    </UiModal>

    <Toasts />
  </div>
</template>

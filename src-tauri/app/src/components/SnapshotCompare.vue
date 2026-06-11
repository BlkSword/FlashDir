<template>
  <div class="snapshot-panel">
    <div class="snapshot-panel-header">📸 快照对比</div>
    <div class="snapshot-panel-content">
      <!-- 操作区 -->
      <div class="snapshot-actions">
        <a-button
          type="primary"
          size="small"
          :loading="saving"
          :disabled="!props.items || props.items.length === 0"
          @click="handleSaveSnapshot"
        >
          💾 保存当前快照
        </a-button>
        <a-button
          size="small"
          :disabled="snapshots.length < 2"
          @click="handleQuickCompare"
        >
          ⚡ 对比最近两次
        </a-button>
      </div>

      <!-- 快照列表 -->
      <div class="snapshot-list" v-if="snapshots.length > 0">
        <div class="snapshot-section-title">
          历史快照 ({{ snapshots.length }})
        </div>
        <div
          v-for="snap in snapshots"
          :key="snap.id"
          class="snapshot-item"
          :class="{
            'snapshot-selected': selectedIds.includes(snap.id),
            'snapshot-latest': snap.id === snapshots[0]?.id
          }"
          @click="toggleSelect(snap.id)"
        >
          <div class="snapshot-select">
            <div
              class="snapshot-checkbox"
              :class="{ checked: selectedIds.includes(snap.id) }"
            >
              <span v-if="selectedIds.includes(snap.id)">✓</span>
            </div>
          </div>
          <div class="snapshot-info">
            <div class="snapshot-path">{{ formatPath(snap.path) }}</div>
            <div class="snapshot-meta">
              <span class="snapshot-size">{{ snap.totalSizeFormatted }}</span>
              <span class="snapshot-dot">·</span>
              <span>{{ snap.fileCount }} 文件</span>
              <span class="snapshot-dot">·</span>
              <span>{{ snap.dirCount }} 目录</span>
            </div>
            <div class="snapshot-time">{{ formatTime(snap.scanTime * 1000) }}</div>
          </div>
          <div class="snapshot-action">
            <a-button
              type="link"
              size="small"
              danger
              @click.stop="handleDelete(snap.id)"
            >删除</a-button>
          </div>
        </div>
      </div>

      <div class="snapshot-empty" v-else>
        <p>暂无双击快照</p>
        <p class="snapshot-hint">扫描目录后点击"保存当前快照"</p>
      </div>

      <!-- 比较按钮 -->
      <div class="snapshot-compare-bar" v-if="selectedIds.length === 2">
        <a-button type="primary" size="small" @click="handleCompare" :loading="comparing">
          对比所选快照 ({{ selectedIds.length }})
        </a-button>
      </div>

      <!-- 差异结果 -->
      <div class="diff-results" v-if="diffResult">
        <!-- 概览 -->
        <div class="diff-overview">
          <div class="diff-stat" :class="diffResult.netChange >= 0 ? 'diff-grow' : 'diff-shrink'">
            <div class="diff-stat-value">{{ formatDelta(diffResult.netChange) }}</div>
            <div class="diff-stat-label">净变化</div>
          </div>
          <div class="diff-stat diff-grow">
            <div class="diff-stat-value">+{{ diffResult.added.length }}</div>
            <div class="diff-stat-label">新增</div>
          </div>
          <div class="diff-stat diff-shrink">
            <div class="diff-stat-value">-{{ diffResult.removed.length }}</div>
            <div class="diff-stat-label">删除</div>
          </div>
          <div class="diff-stat diff-modify">
            <div class="diff-stat-value">{{ diffResult.modified.length }}</div>
            <div class="diff-stat-label">修改</div>
          </div>
        </div>

        <!-- 进度条 -->
        <div class="diff-growth-bar" v-if="diffResult.summary">
          <div class="diff-growth-label">
            {{ diffResult.summary.oldTotalSizeFormatted }}
            →
            {{ diffResult.summary.newTotalSizeFormatted }}
            ({{ diffResult.summary.growthPercent >= 0 ? '+' : '' }}{{ diffResult.summary.growthPercent.toFixed(1) }}%)
          </div>
          <div class="diff-bar-track">
            <div
              class="diff-bar-grow"
              :style="{ width: Math.max(0, diffResult.summary.growthPercent) + '%' }"
              v-if="diffResult.summary.growthPercent > 0"
            ></div>
          </div>
        </div>

        <!-- 新增文件 -->
        <div class="diff-section" v-if="diffResult.added.length > 0">
          <div class="diff-section-title diff-title-grow">
            🟢 新增 ({{ diffResult.added.length }} 项, {{ formatSize(diffResult.addedTotalSize) }})
          </div>
          <div class="diff-items">
            <div
              v-for="item in diffResult.added.slice(0, 20)"
              :key="'a-' + item.path"
              class="diff-item"
            >
              <span class="diff-item-name" :title="item.path">
                {{ item.isDir ? '📁' : '📄' }} {{ item.name.length > 40 ? item.name.substring(0, 40) + '...' : item.name }}
              </span>
              <span class="diff-item-size diff-grow-text">{{ item.sizeFormatted }}</span>
            </div>
            <div v-if="diffResult.added.length > 20" class="diff-more">
              ...还有 {{ diffResult.added.length - 20 }} 项
            </div>
          </div>
        </div>

        <!-- 删除文件 -->
        <div class="diff-section" v-if="diffResult.removed.length > 0">
          <div class="diff-section-title diff-title-shrink">
            🔴 删除 ({{ diffResult.removed.length }} 项, {{ formatSize(diffResult.removedTotalSize) }})
          </div>
          <div class="diff-items">
            <div
              v-for="item in diffResult.removed.slice(0, 20)"
              :key="'r-' + item.path"
              class="diff-item"
            >
              <span class="diff-item-name" :title="item.path">
                {{ item.isDir ? '📁' : '📄' }} {{ item.name.length > 40 ? item.name.substring(0, 40) + '...' : item.name }}
              </span>
              <span class="diff-item-size diff-shrink-text">{{ item.sizeFormatted }}</span>
            </div>
            <div v-if="diffResult.removed.length > 20" class="diff-more">
              ...还有 {{ diffResult.removed.length - 20 }} 项
            </div>
          </div>
        </div>

        <!-- 修改文件 -->
        <div class="diff-section" v-if="diffResult.modified.length > 0">
          <div class="diff-section-title diff-title-modify">
            🟡 大小变化 ({{ diffResult.modified.length }} 项)
          </div>
          <div class="diff-items">
            <div
              v-for="item in diffResult.modified.slice(0, 20)"
              :key="'m-' + item.path"
              class="diff-item"
            >
              <span class="diff-item-name" :title="item.path">
                {{ item.isDir ? '📁' : '📄' }} {{ item.name.length > 35 ? item.name.substring(0, 35) + '...' : item.name }}
              </span>
              <span class="diff-item-delta" :class="item.delta >= 0 ? 'diff-grow-text' : 'diff-shrink-text'">
                {{ item.deltaFormatted }}
              </span>
            </div>
            <div v-if="diffResult.modified.length > 20" class="diff-more">
              ...还有 {{ diffResult.modified.length - 20 }} 项
            </div>
          </div>
        </div>
      </div>
    </div>
  </div>
</template>

<script setup>
import { ref, watch, onMounted } from 'vue'
import { message } from 'ant-design-vue'
import { useTauri } from '../composables/useTauri'
import { formatSize, formatTime, debounce } from '../utils/format.js'

const { invoke } = useTauri()

const props = defineProps({
  items: { type: Array, default: () => [] },
  totalSize: { type: Number, default: 0 },
  currentPath: { type: String, default: '' }
})

const emit = defineEmits(['refresh'])

const snapshots = ref([])
const selectedIds = ref([])
const diffResult = ref(null)
const saving = ref(false)
const comparing = ref(false)

const formatPath = (path) => {
  if (!path) return ''
  const parts = path.replace(/\\/g, '/').split('/')
  return parts[parts.length - 1] || path
}

const formatDelta = (delta) => {
  const abs = Math.abs(delta)
  const formatted = formatSize(abs)
  return delta >= 0 ? `+${formatted}` : `-${formatted}`
}

const loadSnapshots = async () => {
  if (!props.currentPath || !invoke) return
  try {
    const list = await invoke('list_snapshots', { path: props.currentPath })
    snapshots.value = list || []
  } catch (error) {
    console.error('加载快照失败:', error)
  }
}

const toggleSelect = (id) => {
  const idx = selectedIds.value.indexOf(id)
  if (idx >= 0) {
    selectedIds.value.splice(idx, 1)
  } else {
    if (selectedIds.value.length >= 2) {
      selectedIds.value.shift()
    }
    selectedIds.value.push(id)
  }
}

const handleSaveSnapshot = async () => {
  if (!props.items || props.items.length === 0) {
    message.warning('请先扫描目录')
    return
  }
  saving.value = true
  try {
    await invoke('save_snapshot', {
      path: props.currentPath,
      items: props.items,
      totalSize: props.totalSize,
      totalSizeFormatted: formatSize(props.totalSize)
    })
    message.success('快照已保存')
    await loadSnapshots()
  } catch (error) {
    message.error('保存快照失败: ' + error)
  } finally {
    saving.value = false
  }
}

const handleQuickCompare = async () => {
  if (snapshots.value.length < 2) {
    message.warning('至少需要两个快照才能对比')
    return
  }
  selectedIds.value = [snapshots.value[1].id, snapshots.value[0].id]
  await doCompare(snapshots.value[1].id, snapshots.value[0].id)
}

const handleCompare = async () => {
  if (selectedIds.value.length !== 2) return
  const sorted = [...selectedIds.value].sort((a, b) => {
    const sa = snapshots.value.find(s => s.id === a)
    const sb = snapshots.value.find(s => s.id === b)
    return (sa?.scanTime || 0) - (sb?.scanTime || 0)
  })
  await doCompare(sorted[0], sorted[1])
}

const doCompare = async (oldId, newId) => {
  comparing.value = true
  try {
    const result = await invoke('compare_snapshots', { oldId, newId })
    diffResult.value = result
  } catch (error) {
    message.error('对比失败: ' + error)
  } finally {
    comparing.value = false
  }
}

const handleDelete = async (id) => {
  try {
    await invoke('delete_snapshot', { id })
    selectedIds.value = selectedIds.value.filter(s => s !== id)
    if (diffResult.value) {
      // Check if current diff involves deleted snapshot
      const snapIds = snapshots.value.map(s => s.id)
      if (!snapIds.includes(id)) {
        // This is the deleted snapshot - clear diff
      }
    }
    await loadSnapshots()
    message.success('快照已删除')
  } catch (error) {
    message.error('删除失败: ' + error)
  }
}

watch(() => props.currentPath, () => {
  loadSnapshots()
  diffResult.value = null
  selectedIds.value = []
})

onMounted(() => {
  loadSnapshots()
})
</script>

<style scoped>
.snapshot-panel {
  width: 100%;
  height: 100%;
  display: flex;
  flex-direction: column;
  overflow: hidden;
}

.snapshot-panel-header {
  display: none; /* handled by parent tabs */
}

.snapshot-panel-content {
  flex: 1;
  overflow-y: auto;
  padding: 12px;
}

.snapshot-actions {
  display: flex;
  gap: 8px;
  margin-bottom: 12px;
}

.snapshot-actions .ant-btn {
  flex: 1;
}

/* 快照列表 */
.snapshot-section-title {
  font-size: 11px;
  color: #8c8c8c;
  margin-bottom: 8px;
  text-transform: uppercase;
}

.snapshot-list {
  margin-bottom: 12px;
}

.snapshot-item {
  display: flex;
  align-items: flex-start;
  padding: 8px;
  margin-bottom: 4px;
  background: white;
  border: 1px solid #f0f0f0;
  border-radius: 6px;
  cursor: pointer;
  transition: all 0.15s;
}

.snapshot-item:hover {
  border-color: #1890ff;
  background: #f0f5ff;
}

.snapshot-selected {
  border-color: #1890ff;
  background: #e6f7ff;
}

.snapshot-latest {
  border-left: 3px solid #1890ff;
}

.snapshot-select {
  margin-right: 8px;
  padding-top: 2px;
}

.snapshot-checkbox {
  width: 16px;
  height: 16px;
  border: 2px solid #d9d9d9;
  border-radius: 3px;
  display: flex;
  align-items: center;
  justify-content: center;
  font-size: 10px;
  color: white;
  transition: all 0.15s;
}

.snapshot-checkbox.checked {
  background: #1890ff;
  border-color: #1890ff;
}

.snapshot-info {
  flex: 1;
  min-width: 0;
}

.snapshot-path {
  font-size: 12px;
  font-weight: 600;
  color: #262626;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.snapshot-meta {
  font-size: 10px;
  color: #8c8c8c;
  margin-top: 2px;
}

.snapshot-size {
  font-weight: 500;
  color: #595959;
}

.snapshot-dot {
  margin: 0 4px;
}

.snapshot-time {
  font-size: 10px;
  color: #bfbfbf;
  margin-top: 2px;
}

.snapshot-action {
  flex-shrink: 0;
}

/* 比较栏 */
.snapshot-compare-bar {
  padding: 8px 0;
}

.snapshot-compare-bar .ant-btn {
  width: 100%;
}

/* 空状态 */
.snapshot-empty {
  text-align: center;
  color: #bfbfbf;
  font-size: 12px;
  padding: 32px 0;
}

.snapshot-hint {
  font-size: 11px;
  margin-top: 4px;
}

/* 差异结果 */
.diff-results {
  margin-top: 16px;
}

.diff-overview {
  display: flex;
  gap: 8px;
  margin-bottom: 12px;
}

.diff-stat {
  flex: 1;
  background: white;
  border: 1px solid #f0f0f0;
  border-radius: 6px;
  padding: 8px;
  text-align: center;
}

.diff-stat-value {
  font-size: 16px;
  font-weight: 700;
}

.diff-stat-label {
  font-size: 10px;
  color: #8c8c8c;
  margin-top: 2px;
}

.diff-grow .diff-stat-value { color: #52c41a; }
.diff-shrink .diff-stat-value { color: #f5222d; }
.diff-modify .diff-stat-value { color: #faad14; }

/* 增长条 */
.diff-growth-bar {
  margin-bottom: 12px;
  padding: 8px;
  background: white;
  border-radius: 6px;
  border: 1px solid #f0f0f0;
}

.diff-growth-label {
  font-size: 11px;
  color: #595959;
  margin-bottom: 4px;
  text-align: center;
}

.diff-bar-track {
  height: 6px;
  background: #f5f5f5;
  border-radius: 3px;
  overflow: hidden;
}

.diff-bar-grow {
  height: 100%;
  background: linear-gradient(90deg, #52c41a, #f5222d);
  border-radius: 3px;
  transition: width 0.3s;
}

/* 差异区域 */
.diff-section {
  margin-bottom: 12px;
}

.diff-section-title {
  font-size: 12px;
  font-weight: 600;
  margin-bottom: 6px;
  padding: 4px 8px;
  border-radius: 4px;
}

.diff-title-grow { background: #f6ffed; color: #52c41a; }
.diff-title-shrink { background: #fff1f0; color: #f5222d; }
.diff-title-modify { background: #fffbe6; color: #faad14; }

.diff-items {
  max-height: 300px;
  overflow-y: auto;
}

.diff-item {
  display: flex;
  justify-content: space-between;
  align-items: center;
  padding: 3px 8px;
  font-size: 11px;
}

.diff-item:hover {
  background: #fafafa;
}

.diff-item-name {
  flex: 1;
  min-width: 0;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  color: #595959;
}

.diff-item-size {
  flex-shrink: 0;
  margin-left: 8px;
  font-weight: 500;
}

.diff-item-delta {
  flex-shrink: 0;
  margin-left: 8px;
  font-weight: 500;
}

.diff-grow-text { color: #52c41a; }
.diff-shrink-text { color: #f5222d; }

.diff-more {
  font-size: 11px;
  color: #bfbfbf;
  text-align: center;
  padding: 4px;
}
</style>

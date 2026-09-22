<template>
  <div class="duplicate-panel">
    <div class="duplicate-toolbar">
      <button class="btn primary" :disabled="scanning || !props.currentPath || props.items.length === 0" @click="handleScan">
        <span v-if="scanning" class="spinner" />扫描重复文件
      </button>
      <label class="filter-box" style="flex:0 0 150px">
        <span class="section-note">最小</span>
        <input type="number" min="0" max="102400" v-model.number="minSizeMB" style="width:56px" />
        <span class="section-note">MB</span>
      </label>
    </div>

    <div v-if="scanning" class="duplicate-empty">
      <div class="duplicate-empty-icon">⏳</div>
      <div>正在计算文件哈希，大目录可能需要一些时间…</div>
    </div>

    <div v-else-if="!result" class="duplicate-empty">
      <div class="duplicate-empty-icon">🔁</div>
      <div>按文件大小与内容哈希识别重复文件</div>
      <div class="duplicate-empty-hint">建议先扫描目录，再点“扫描重复文件”</div>
    </div>

    <template v-else>
      <div v-if="result.groups.length === 0" class="duplicate-empty">
        <div class="duplicate-empty-icon">✅</div>
        <div>未发现重复文件</div>
      </div>

      <template v-else>
        <div class="duplicate-summary">
          <div class="duplicate-stat">
            <div class="duplicate-stat-value">{{ result.totalGroups }}</div>
            <div class="duplicate-stat-label">重复组</div>
          </div>
          <div class="duplicate-stat">
            <div class="duplicate-stat-value">{{ result.totalFiles }}</div>
            <div class="duplicate-stat-label">重复文件</div>
          </div>
          <div class="duplicate-stat duplicate-stat--danger">
            <div class="duplicate-stat-value">{{ result.totalWastedFormatted }}</div>
            <div class="duplicate-stat-label">可回收空间</div>
          </div>
        </div>

        <div
          v-for="(group, gi) in result.groups"
          :key="gi"
          class="duplicate-group"
        >
          <div class="duplicate-group-header">
            <div class="duplicate-group-main">
              <span class="duplicate-group-size">{{ group.sizeFormatted }}</span>
              <span class="duplicate-group-count">{{ group.fileCount }} 个文件</span>
            </div>
            <span class="duplicate-group-wasted">可回收 {{ group.wastedFormatted }}</span>
          </div>

          <div class="duplicate-group-files">
            <div
              v-for="(file, fi) in group.files"
              :key="file.path"
              class="duplicate-file"
              :title="`点击打开：${file.path}`"
              @click="openPath(file.path)"
            >
              <span class="duplicate-file-index">{{ fi + 1 }}</span>
              <div class="duplicate-file-body">
                <span class="duplicate-file-name">{{ file.name }}</span>
                <span class="duplicate-file-path">{{ file.path }}</span>
              </div>
              <span class="duplicate-file-open">打开</span>
            </div>
          </div>
        </div>
      </template>
    </template>
  </div>
</template>

<script setup>
import { ref } from 'vue'
import { useToasts } from '../composables/useToasts.js'
import { useTauri } from '../composables/useTauri'

const toasts = useToasts()

const props = defineProps({
  items: { type: Array, default: () => [] },
  currentPath: { type: String, default: '' },
})

const { invoke } = useTauri()

const scanning = ref(false)
const result = ref(null)
const minSizeMB = ref(1)

const handleScan = async () => {
  if (!props.currentPath) return
  scanning.value = true
  result.value = null
  try {
    const minSize = Math.max(0, Math.round((minSizeMB.value || 0) * 1024 * 1024))
    result.value = await invoke('find_duplicates', {
      path: props.currentPath,
      minSize,
    })
  } catch (error) {
    console.error('重复文件检测失败:', error)
    toasts.err('重复文件检测失败: ' + error)
  } finally {
    scanning.value = false
  }
}

const openPath = async (path) => {
  try {
    await invoke('open_path', { path })
  } catch (error) {
    toasts.err('打开文件失败: ' + error)
  }
}
</script>

<style scoped>
.duplicate-panel {
  display: flex;
  flex-direction: column;
  gap: 12px;
}
.duplicate-toolbar {
  display: flex;
  align-items: center;
  gap: 8px;
}
.duplicate-empty {
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  gap: 6px;
  padding: 36px 12px;
  color: var(var(--tx-2));
  font-size: 12px;
  text-align: center;
  background: var(var(--bg-2));
  border: 1px dashed var(var(--bd-1));
  border-radius: 10px;
}
.duplicate-empty-icon {
  font-size: 26px;
  line-height: 1;
}
.duplicate-empty-hint {
  color: var(var(--tx-3));
  font-size: 11px;
}
.duplicate-summary {
  display: flex;
  gap: 8px;
}
.duplicate-stat {
  flex: 1;
  background: var(var(--bg-2));
  border: 1px solid var(var(--bd-1));
  border-radius: 8px;
  padding: 10px 8px;
  text-align: center;
}
.duplicate-stat-value {
  font-size: 15px;
  font-weight: 700;
  color: var(var(--tx-0));
  font-family: Consolas, 'JetBrains Mono', monospace;
}
.duplicate-stat-label {
  font-size: 10px;
  color: var(var(--tx-2));
  margin-top: 2px;
  text-transform: uppercase;
  letter-spacing: .4px;
}
.duplicate-stat--danger .duplicate-stat-value {
  color: var(var(--bad));
}
.duplicate-group {
  background: var(var(--bg-2));
  border: 1px solid var(var(--bd-1));
  border-radius: 10px;
  overflow: hidden;
}
.duplicate-group-header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 10px;
  padding: 8px 12px;
  border-bottom: 1px solid var(var(--bd-1));
  background: var(var(--bg-1));
}
.duplicate-group-main {
  display: flex;
  align-items: baseline;
  gap: 8px;
  min-width: 0;
}
.duplicate-group-size {
  font-weight: 700;
  font-size: 13px;
  color: var(var(--tx-0));
  font-family: Consolas, 'JetBrains Mono', monospace;
}
.duplicate-group-count {
  color: var(var(--tx-2));
  font-size: 11px;
}
.duplicate-group-wasted {
  flex-shrink: 0;
  color: var(var(--bad));
  font-size: 11px;
  background: rgba(244,135,113,.1);
  border: 1px solid rgba(244,135,113,.2);
  border-radius: 6px;
  padding: 2px 8px;
}
.duplicate-group-files {
  display: flex;
  flex-direction: column;
}
.duplicate-file {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 6px 12px;
  border-bottom: 1px solid rgba(255,255,255,0.04);
  cursor: pointer;
  transition: background .12s ease;
}
.duplicate-file:last-child {
  border-bottom: none;
}
.duplicate-file:hover {
  background: var(var(--bg-3));
}
.duplicate-file-index {
  width: 18px;
  height: 18px;
  border-radius: 5px;
  background: var(var(--bg-3));
  color: var(var(--tx-2));
  display: grid;
  place-items: center;
  font-size: 10px;
  font-family: Consolas, 'JetBrains Mono', monospace;
  flex-shrink: 0;
}
.duplicate-file-body {
  flex: 1;
  min-width: 0;
  display: flex;
  flex-direction: column;
}
.duplicate-file-name {
  font-size: 12px;
  color: var(var(--tx-0));
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}
.duplicate-file-path {
  font-size: 10px;
  color: var(var(--tx-3));
  font-family: Consolas, 'JetBrains Mono', monospace;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}
.duplicate-file-open {
  flex-shrink: 0;
  font-size: 11px;
  color: var(var(--accent));
  opacity: 0;
  transition: opacity .12s ease;
}
.duplicate-file:hover .duplicate-file-open {
  opacity: 1;
}
</style>

<template>
  <div class="duplicate-panel">
    <div class="duplicate-header">
      <a-button
        type="primary"
        size="small"
        :loading="scanning"
        :disabled="!props.currentPath || props.items.length === 0"
        @click="handleScan"
      >
        扫描重复文件
      </a-button>
      <a-input-number
        v-model:value="minSizeMB"
        :min="0"
        :max="102400"
        size="small"
        addon-after="MB"
        style="width: 120px"
      />
    </div>

    <div v-if="scanning" class="duplicate-empty">
      正在计算文件哈希，大目录可能需要一些时间…
    </div>

    <div v-else-if="!result" class="duplicate-empty">
      扫描后按文件大小与内容哈希识别重复文件
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
        <div class="duplicate-stat">
          <div class="duplicate-stat-value duplicate-warn">{{ result.totalWastedFormatted }}</div>
          <div class="duplicate-stat-label">可回收</div>
        </div>
      </div>

      <div v-if="result.groups.length === 0" class="duplicate-empty">
        未发现重复文件
      </div>

      <div
        v-for="(group, gi) in result.groups"
        :key="gi"
        class="duplicate-group"
      >
        <div class="duplicate-group-header">
          <span class="duplicate-group-size">{{ group.sizeFormatted }}</span>
          <span class="duplicate-group-count">{{ group.fileCount }} 个文件</span>
          <span class="duplicate-group-wasted">可回收 {{ group.wastedFormatted }}</span>
        </div>
        <div class="duplicate-group-files">
          <div
            v-for="(file, fi) in group.files"
            :key="file.path"
            class="duplicate-file"
            :title="file.path"
            @dblclick="openPath(file.path)"
          >
            <span class="duplicate-file-name">{{ file.name }}</span>
            <span class="duplicate-file-path">{{ file.path }}</span>
          </div>
        </div>
      </div>
    </template>
  </div>
</template>

<script setup>
import { ref } from 'vue'
import { message } from 'ant-design-vue'
import { useTauri } from '../composables/useTauri'

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
    message.error('重复文件检测失败: ' + error)
  } finally {
    scanning.value = false
  }
}

const openPath = async (path) => {
  try {
    await invoke('open_path', { path })
  } catch (error) {
    message.error('打开文件失败: ' + error)
  }
}
</script>

<style scoped>
.duplicate-panel {
  display: flex;
  flex-direction: column;
  gap: 10px;
}
.duplicate-header {
  display: flex;
  align-items: center;
  gap: 8px;
}
.duplicate-empty {
  text-align: center;
  color: var(--fd-text-2);
  font-size: 12px;
  padding: 24px 0;
}
.duplicate-summary {
  display: flex;
  gap: 8px;
}
.duplicate-stat {
  flex: 1;
  background: var(--fd-bg-0);
  border: 1px solid var(--fd-border);
  border-radius: 6px;
  padding: 10px;
  text-align: center;
}
.duplicate-stat-value {
  font-size: 15px;
  font-weight: 600;
  color: var(--fd-text-0);
}
.duplicate-stat-label {
  font-size: 10px;
  color: var(--fd-text-2);
  margin-top: 2px;
}
.duplicate-warn {
  color: var(--fd-danger);
}
.duplicate-group {
  background: var(--fd-bg-0);
  border: 1px solid var(--fd-border);
  border-radius: 6px;
  padding: 8px;
}
.duplicate-group-header {
  display: flex;
  gap: 10px;
  align-items: center;
  font-size: 12px;
  margin-bottom: 6px;
}
.duplicate-group-size {
  font-weight: 600;
  color: var(--fd-text-0);
}
.duplicate-group-count {
  color: var(--fd-text-2);
}
.duplicate-group-wasted {
  margin-left: auto;
  color: var(--fd-danger);
}
.duplicate-file {
  display: flex;
  flex-direction: column;
  padding: 3px 6px;
  border-radius: 4px;
  cursor: pointer;
}
.duplicate-file:hover {
  background: var(--fd-bg-2);
}
.duplicate-file-name {
  font-size: 12px;
  color: var(--fd-text-0);
}
.duplicate-file-path {
  font-size: 10px;
  color: var(--fd-text-2);
  font-family: Consolas, 'JetBrains Mono', monospace;
  word-break: break-all;
}
</style>

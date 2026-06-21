<template>
  <div class="dev-panel" v-if="hasData">
    <div class="dev-panel-header">开发者分析</div>
    <div class="dev-panel-content">
      <!-- 概览卡片 -->
      <div class="dev-summary" v-if="devData">
        <div class="dev-summary-card">
          <div class="dev-summary-label">开发工具占用</div>
          <div class="dev-summary-value dev-summary-primary">
            {{ devData.devTotalSize ? formatSize(devData.devTotalSize) : '计算中...' }}
          </div>
          <div class="dev-summary-sub" v-if="devData.devPercent > 0">
            占总量 {{ devData.devPercent.toFixed(1) }}%
          </div>
        </div>
        <div class="dev-summary-card" v-if="devData.categories && devData.categories.length > 0">
          <div class="dev-summary-label">最大类别</div>
          <div class="dev-summary-value">
            {{ devData.categories[0].icon }} {{ devData.categories[0].label }}
          </div>
          <div class="dev-summary-sub">
            {{ devData.categories[0].totalSizeFormatted }}
          </div>
        </div>
      </div>

      <!-- 类别列表 -->
      <div class="dev-category-list" v-if="devData && devData.categories && devData.categories.length > 0">
        <div
          v-for="cat in devData.categories"
          :key="cat.category"
          class="dev-category-item"
        >
          <div class="dev-category-header">
            <span class="dev-category-icon">{{ cat.icon }}</span>
            <div class="dev-category-info">
              <span class="dev-category-name">{{ cat.label }}</span>
              <span class="dev-category-desc">{{ cat.description }}</span>
            </div>
            <div class="dev-category-size">
              <span class="dev-size-value">{{ cat.totalSizeFormatted }}</span>
              <span class="dev-size-percent">{{ cat.percentOfDev.toFixed(1) }}%</span>
            </div>
          </div>

          <!-- 进度条 -->
          <div class="dev-progress-wrapper">
            <div
              class="dev-progress-bar"
              :style="{ width: cat.percentOfDev + '%', backgroundColor: getColor(cat.category) }"
            ></div>
          </div>

          <!-- Top 5 子项 -->
          <div class="dev-top-items" v-if="cat.topItems && cat.topItems.length > 0">
            <div
              v-for="(top, idx) in cat.topItems"
              :key="idx"
              class="dev-top-item"
            >
              <span class="dev-top-name" :title="top.name">{{ top.name.length > 35 ? top.name.substring(0, 35) + '...' : top.name }}</span>
              <span class="dev-top-size">{{ top.sizeFormatted }}</span>
            </div>
          </div>
        </div>
      </div>

      <!-- 无数据 -->
      <div class="dev-empty" v-else>
        未检测到常见开发者工具目录
      </div>
    </div>
  </div>
</template>

<script setup>
import { ref, watch, computed } from 'vue'
import { formatSize, debounce } from '../utils/format.js'
import { useTauri } from '../composables/useTauri'

const { invoke } = useTauri()

const props = defineProps({
  items: {
    type: Array,
    default: () => []
  },
  totalSize: {
    type: Number,
    default: 0
  },
  currentPath: {
    type: String,
    default: ''
  }
})

const devData = ref(null)
const lastDataFingerprint = ref('')

const hasData = computed(() => {
  return devData.value && devData.value.categories && devData.value.categories.length > 0
})

// 颜色映射
const colorMap = {
  node: '#339933',
  rust: '#dea584',
  rust_cache: '#b7410e',
  python_venv: '#3776ab',
  python_cache: '#ffd43b',
  java_gradle: '#a074c4',
  java_maven: '#c41d7f',
  git: '#f05032',
  dotnet: '#512bd4',
  dotnet_cache: '#0078d4',
  go: '#00add8',
  docker: '#2496ed',
  wsl: '#e95420',
  android: '#3ddc84',
  npm_cache: '#cb3837',
  pip_cache: '#3776ab',
  electron: '#47848f',
  vscode: '#007acc'
}

const getColor = (category) => {
  return colorMap[category] || '#8c8c8c'
}

const analyze = async () => {
  const fingerprint = `${props.currentPath}|${props.items.length}`
  if (fingerprint === lastDataFingerprint.value) return

  if (!props.currentPath || !props.items || props.items.length === 0) {
    devData.value = null
    lastDataFingerprint.value = ''
    return
  }
  lastDataFingerprint.value = fingerprint

  try {
    // 后端从内存缓存读取 items 并用 Rayon 分析（已按"匹配边界顶层"去重），
    // 前端不再传百万级 items，也不在主线程做 O(n) 路径匹配
    const result = await invoke('analyze_dev_disk', { path: props.currentPath })
    devData.value = result || null
  } catch (error) {
    console.error('开发者分析失败:', error)
    devData.value = null
  }
}

const debouncedAnalyze = debounce(analyze, 300)

watch(() => [props.items.length, props.currentPath], () => {
  debouncedAnalyze()
})

// 初始分析
analyze()
</script>

<style scoped>
.dev-panel {
  width: 320px;
  background: #fafafa;
  border-left: 1px solid #f0f0f0;
  display: flex;
  flex-direction: column;
  overflow: hidden;
}

.dev-panel-header {
  padding: 12px 16px;
  font-size: 14px;
  font-weight: 600;
  border-bottom: 1px solid #f0f0f0;
  background: white;
}

.dev-panel-content {
  flex: 1;
  overflow-y: auto;
  padding: 12px;
}

/* 概览卡片 */
.dev-summary {
  display: flex;
  gap: 8px;
  margin-bottom: 16px;
}

.dev-summary-card {
  flex: 1;
  background: white;
  border: 1px solid #f0f0f0;
  border-radius: 6px;
  padding: 10px 12px;
  text-align: center;
}

.dev-summary-label {
  font-size: 11px;
  color: #8c8c8c;
  margin-bottom: 4px;
}

.dev-summary-value {
  font-size: 14px;
  font-weight: 600;
  color: #262626;
}

.dev-summary-primary {
  color: #cf1322;
}

.dev-summary-sub {
  font-size: 11px;
  color: #8c8c8c;
  margin-top: 2px;
}

/* 类别列表 */
.dev-category-list {
  display: flex;
  flex-direction: column;
  gap: 12px;
}

.dev-category-item {
  background: white;
  border: 1px solid #f0f0f0;
  border-radius: 6px;
  padding: 10px 12px;
}

.dev-category-header {
  display: flex;
  align-items: center;
  gap: 8px;
}

.dev-category-icon {
  font-size: 20px;
  flex-shrink: 0;
}

.dev-category-info {
  flex: 1;
  display: flex;
  flex-direction: column;
  min-width: 0;
}

.dev-category-name {
  font-size: 13px;
  font-weight: 600;
  color: #262626;
}

.dev-category-desc {
  font-size: 11px;
  color: #8c8c8c;
}

.dev-category-size {
  text-align: right;
  flex-shrink: 0;
  display: flex;
  flex-direction: column;
}

.dev-size-value {
  font-size: 13px;
  font-weight: 600;
  color: #262626;
}

.dev-size-percent {
  font-size: 11px;
  color: #8c8c8c;
}

/* 进度条 */
.dev-progress-wrapper {
  height: 4px;
  background: #f0f0f0;
  border-radius: 2px;
  margin-top: 8px;
  overflow: hidden;
}

.dev-progress-bar {
  height: 100%;
  border-radius: 2px;
  transition: width 0.3s ease;
}

/* Top 项 */
.dev-top-items {
  margin-top: 8px;
  padding-top: 8px;
  border-top: 1px dashed #f0f0f0;
}

.dev-top-item {
  display: flex;
  justify-content: space-between;
  align-items: center;
  padding: 2px 0;
}

.dev-top-name {
  font-size: 11px;
  color: #595959;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  flex: 1;
  min-width: 0;
}

.dev-top-size {
  font-size: 11px;
  color: #8c8c8c;
  font-weight: 500;
  flex-shrink: 0;
  margin-left: 8px;
}

/* 空状态 */
.dev-empty {
  text-align: center;
  color: #bfbfbf;
  font-size: 12px;
  padding: 32px 16px;
}
</style>

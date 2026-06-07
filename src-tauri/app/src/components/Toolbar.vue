<template>
  <div class="toolbar">
    <a-space :size="8">
      <a-button :disabled="!canGoBack" @click="emit('navigate', 'back')">
        <ArrowLeftOutlined />
      </a-button>
      <a-button :disabled="!canGoForward" @click="emit('navigate', 'forward')">
        <ArrowRightOutlined />
      </a-button>
      <a-button :disabled="!canGoUp" @click="emit('navigate', 'up')">
        <ArrowUpOutlined />
      </a-button>
      <a-button @click="emit('show-history')">
        <HistoryOutlined />
      </a-button>
    </a-space>

    <div class="path-input-wrapper">
      <a-input
        v-model:value="localPath"
        placeholder="输入目录路径"
        :disabled="loading"
        @pressEnter="handleScan"
      />
      <a-button type="primary" :loading="loading" @click="handleScan">
        <SearchOutlined />
        扫描
      </a-button>
      <a-button @click="emit('browse')">
        <FolderOpenOutlined />
        浏览
      </a-button>
    </div>

    <div class="toolbar-right">
      <!-- 管理员权限提示 -->
      <a-tooltip v-if="!mftAvailable" title="管理员模式下扫描速度可提升 5-8 倍">
        <a-tag color="warning" style="cursor: pointer" @click="handleRestartAsAdmin">
          <SafetyCertificateOutlined />
          管理员模式
        </a-tag>
      </a-tooltip>

      <a-input-search
        v-model:value="searchKeyword"
        placeholder="搜索文件..."
        :disabled="loading"
        allow-clear
        @search="handleSearch"
        style="width: 200px"
      />
    </div>
  </div>
</template>

<script setup>
import { ref, watch, onMounted } from 'vue'
import {
  ArrowLeftOutlined,
  ArrowRightOutlined,
  ArrowUpOutlined,
  HistoryOutlined,
  FolderOpenOutlined,
  SearchOutlined,
  SafetyCertificateOutlined,
} from '@ant-design/icons-vue'

const props = defineProps({
  path: { type: String, default: '' },
  canGoBack: { type: Boolean, default: false },
  canGoForward: { type: Boolean, default: false },
  canGoUp: { type: Boolean, default: false },
  loading: { type: Boolean, default: false }
})

const emit = defineEmits(['scan', 'browse', 'navigate', 'show-history', 'search'])

const localPath = ref(props.path || '')
const searchKeyword = ref('')
const mftAvailable = ref(true)

const invoke = () => {
  return window.__TAURI__?.core?.invoke
}

const checkMftAvailable = async () => {
  // 检测 C 盘 MFT 可用性（Windows 上最常用）
  try {
    const available = await invoke()('check_mft_available', { path: 'C:\\' })
    mftAvailable.value = available
  } catch {
    // 非 Windows 或检测失败，不显示提示
    mftAvailable.value = true
  }
}

const handleRestartAsAdmin = async () => {
  try {
    await invoke()('restart_as_admin')
    // 如果提权成功，旧进程会退出（新进程以管理员身份启动）
    // 延迟关闭当前窗口
    setTimeout(() => window.close(), 500)
  } catch {
    // 提权失败，什么都不做
  }
}

onMounted(() => {
  checkMftAvailable()
})

watch(() => props.path, (newVal) => {
  localPath.value = newVal || ''
})

const handleScan = () => {
  if (localPath.value.trim()) {
    emit('scan', localPath.value.trim())
  }
}

const handleSearch = (value) => {
  emit('search', value)
}
</script>

<style scoped>
.toolbar {
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: 8px 16px;
  background: #fafafa;
  border-bottom: 1px solid #f0f0f0;
  gap: 16px;
}
.path-input-wrapper {
  display: flex;
  align-items: center;
  gap: 8px;
  flex: 1;
  max-width: 500px;
}
.toolbar-right {
  display: flex;
  align-items: center;
  gap: 8px;
}
</style>

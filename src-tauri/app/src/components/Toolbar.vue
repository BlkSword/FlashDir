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

    <a-input-search
      v-model:value="searchKeyword"
      placeholder="搜索文件..."
      :disabled="loading"
      allow-clear
      @search="handleSearch"
      style="width: 200px"
    />
  </div>
</template>

<script setup>
import { ref, watch } from 'vue'
import {
  ArrowLeftOutlined,
  ArrowRightOutlined,
  ArrowUpOutlined,
  HistoryOutlined,
  FolderOpenOutlined,
  SearchOutlined
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
</style>

<template>
  <div class="toolbar">
    <a-space :size="8">
      <a-tooltip title="返回">
        <a-button
          :disabled="!canGoBack"
          @click="$emit('navigate', 'back')"
        >
          <template #icon>
            <ArrowLeftOutlined />
          </template>
        </a-button>
      </a-tooltip>

      <a-tooltip title="前进">
        <a-button
          :disabled="!canGoForward"
          @click="$emit('navigate', 'forward')"
        >
          <template #icon>
            <ArrowRightOutlined />
          </template>
        </a-button>
      </a-tooltip>

      <a-tooltip title="上级目录">
        <a-button
          :disabled="!canGoUp"
          @click="$emit('navigate', 'up')"
        >
          <template #icon>
            <ArrowUpOutlined />
          </template>
        </a-button>
      </a-tooltip>

      <a-tooltip title="历史记录">
        <a-button @click="$emit('show-history')">
          <template #icon>
            <HistoryOutlined />
          </template>
        </a-button>
      </a-tooltip>
    </a-space>

    <div class="path-input-wrapper">
      <a-input
        v-model:value="localPath"
        placeholder="输入目录路径"
        :disabled="loading"
        @keypress.enter="handleScan"
      >
        <template #prefix>
          <FolderOutlined />
        </template>
        <template #suffix>
          <a-button
            type="primary"
            size="small"
            :loading="loading"
            @click="handleScan"
          >
            <template #icon>
              <SearchOutlined />
            </template>
            扫描
          </a-button>
        </template>
      </a-input>

      <a-button @click="$emit('browse')">
        <template #icon>
          <FolderOpenOutlined />
        </template>
        浏览
      </a-button>
    </div>

    <div class="search-wrapper">
      <a-input-search
        v-model:value="searchKeyword"
        placeholder="搜索文件..."
        :disabled="loading"
        allow-clear
        @input="handleSearchInput"
        @clear="handleSearchClear"
        style="width: 200px"
      />
    </div>
  </div>
</template>

<script setup>
import { ref, watch } from 'vue'
import {
  ArrowLeftOutlined,
  ArrowRightOutlined,
  ArrowUpOutlined,
  HistoryOutlined,
  FolderOutlined,
  FolderOpenOutlined,
  SearchOutlined
} from '@ant-design/icons-vue'

const props = defineProps({
  path: {
    type: String,
    default: ''
  },
  canGoBack: {
    type: Boolean,
    default: false
  },
  canGoForward: {
    type: Boolean,
    default: false
  },
  canGoUp: {
    type: Boolean,
    default: false
  },
  loading: {
    type: Boolean,
    default: false
  }
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

const handleSearchInput = (e) => {
  emit('search', '')
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

.path-input-wrapper .ant-input {
  flex: 1;
}

.search-wrapper {
  display: flex;
  align-items: center;
}
</style>

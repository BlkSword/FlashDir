<template>
  <header class="fd-toolbar">
    <div class="fd-brand" title="FlashDir">
      <svg fill="none" stroke="currentColor" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M4 4h6l2 2h8a2 2 0 012 2v10a2 2 0 01-2 2H4a2 2 0 01-2-2V6a2 2 0 012-2z"/><path d="M2 10h20"/></svg>
      <span class="fd-brand-text"><b>FlashDir</b><small>磁盘观测站</small></span>
    </div>
    <div class="fd-toolbar-group">
      <button
        class="fd-icon-btn"
        title="后退"
        :disabled="!canGoBack"
        @click="$emit('navigate', 'back')"
      >
        <svg fill="none" stroke="currentColor" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M15 19l-7-7 7-7" /></svg>
      </button>
      <button
        class="fd-icon-btn"
        title="前进"
        :disabled="!canGoForward"
        @click="$emit('navigate', 'forward')"
      >
        <svg fill="none" stroke="currentColor" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M9 5l7 7-7 7" /></svg>
      </button>
      <button
        class="fd-icon-btn"
        title="上级"
        :disabled="!canGoUp"
        @click="$emit('navigate', 'up')"
      >
        <svg fill="none" stroke="currentColor" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M5 10l7-7m0 0l7 7m-7-7v18" /></svg>
      </button>
    </div>

    <button
      class="fd-btn fd-btn-primary"
      :disabled="loading"
      @click="$emit('scan', localPath)"
    >
      <svg v-if="loading" class="animate-spin" width="13" height="13" fill="none" viewBox="0 0 24 24">
        <circle class="opacity-25" cx="12" cy="12" r="10" stroke="currentColor" stroke-width="4"></circle>
        <path class="opacity-75" fill="currentColor" d="M4 12a8 8 0 018-8V0C5.373 0 0 5.373 0 12h4zm2 5.291A7.962 7.962 0 014 12H0c0 3.042 1.135 5.824 3 7.938l3-2.647z"></path>
      </svg>
      <svg v-else width="13" height="13" fill="none" stroke="currentColor" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M21 21l-6-6m2-5a7 7 0 11-14 0 7 7 0 0114 0z" /></svg>
      {{ loading ? '扫描中' : '扫描' }}
    </button>

    <button v-if="loading" class="fd-btn" @click="$emit('cancel-scan')">取消</button>

    <button class="fd-btn" @click="$emit('browse')">浏览…</button>

    <div class="fd-path-bar">
      <input
        v-model="localPath"
        type="text"
        placeholder="输入目录路径，回车或点扫描"
        spellcheck="false"
        @keyup.enter="$emit('scan', localPath)"
      />
    </div>

    <GlobalSearchDropdown
      ref="globalSearchRef"
      @open-dir="$emit('open-dir', $event)"
    />

    <button class="fd-icon-btn" title="关于" @click="$emit('show-about')">
      <svg fill="none" stroke="currentColor" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M12 22c5.523 0 10-4.477 10-10S17.523 2 12 2 2 6.477 2 12s4.477 10 10 10z"/><path d="M12 16v-4m0-4h.01"/></svg>
    </button>

    <button class="fd-icon-btn" title="诊断" @click="$emit('show-diagnostics')">
      <svg fill="none" stroke="currentColor" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M13 16h-1v-4h-1m1-4h.01M21 12a9 9 0 11-18 0 9 9 0 0118 0z" /></svg>
    </button>

    <button class="fd-icon-btn" title="历史记录" @click="$emit('show-history')">
      <svg fill="none" stroke="currentColor" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M12 8v4l3 3m6-3a9 9 0 11-18 0 9 9 0 0118 0z" /></svg>
    </button>

    <button class="fd-icon-btn" title="收起/展开侧边栏" @click="$emit('toggle-sidebar')">
      <svg fill="none" stroke="currentColor" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M4 6h16M4 12h16M4 18h16" /></svg>
    </button>
  </header>
</template>

<script setup>
import { ref, watch } from 'vue'
import GlobalSearchDropdown from './GlobalSearchDropdown.vue'

const props = defineProps({
  path: { type: String, default: '' },
  canGoBack: { type: Boolean, default: false },
  canGoForward: { type: Boolean, default: false },
  canGoUp: { type: Boolean, default: false },
  loading: { type: Boolean, default: false },
})

// 路径输入框本地值：用户可自由编辑，父级路径变化（导航/历史/浏览）时同步
const localPath = ref(props.path)
watch(() => props.path, (v) => { localPath.value = v })

const globalSearchRef = ref(null)

const focusGlobalSearch = () => {
  globalSearchRef.value?.focusSearch?.()
}

defineEmits([
  'scan',
  'cancel-scan',
  'browse',
  'navigate',
  'show-history',
  'show-about',
  'show-diagnostics',
  'open-dir',
  'toggle-sidebar',
])

defineExpose({ focusGlobalSearch })
</script>

<style scoped>
.fd-toolbar {
  grid-column: 1 / -1;
  position: relative;
  z-index: 100;
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 0 10px;
  background: var(--fd-bg-1);
  border-bottom: 1px solid var(--fd-border);
}
.fd-brand {
  display: flex;
  align-items: center;
  gap: 7px;
  min-width: 132px;
  color: var(--fd-accent);
}
.fd-brand > svg {
  width: 22px;
  height: 22px;
}
.fd-brand-text {
  display: flex;
  flex-direction: column;
  line-height: 1.1;
}
.fd-brand-text b {
  font-size: 13px;
  color: var(--fd-text-0);
  letter-spacing: .3px;
}
.fd-brand-text small {
  font-size: 9px;
  color: var(--fd-text-2);
  letter-spacing: 1px;
}
.fd-toolbar-group {
  display: flex;
  align-items: center;
  gap: 4px;
}
.fd-btn {
  display: inline-flex;
  align-items: center;
  gap: 4px;
  padding: 4px 10px;
  border: 1px solid var(--fd-border);
  background: var(--fd-bg-2);
  color: var(--fd-text-1);
  border-radius: 3px;
  font-size: 12px;
  cursor: pointer;
}
.fd-btn:hover:not(:disabled) { background: var(--fd-bg-3); }
.fd-btn:disabled { opacity: 0.5; cursor: default; }
.fd-btn-primary {
  background: var(--fd-accent);
  border-color: var(--fd-accent);
  color: #fff;
}
.fd-btn-primary:hover:not(:disabled) { background: var(--fd-accent-hover); border-color: var(--fd-accent-hover); }
.fd-icon-btn {
  width: 26px;
  height: 26px;
  padding: 0;
  display: inline-grid;
  place-items: center;
  border: 1px solid var(--fd-border);
  background: var(--fd-bg-2);
  color: var(--fd-text-1);
  border-radius: 3px;
  cursor: pointer;
}
.fd-icon-btn:hover:not(:disabled) { background: var(--fd-bg-3); }
.fd-icon-btn.fd-icon-active {
  color: #fff;
  background: var(--fd-accent);
  border-color: var(--fd-accent);
}
.fd-icon-btn:disabled { opacity: 0.5; cursor: default; }
.fd-icon-btn svg { width: 14px; height: 14px; }
.fd-path-bar {
  flex: 1;
  display: flex;
  align-items: center;
  gap: 6px;
  padding: 4px 8px;
  background: var(--fd-bg-0);
  border: 1px solid var(--fd-border);
  border-radius: 3px;
  color: var(--fd-text-1);
  font-size: 12px;
  min-width: 0;
}
.fd-path-bar input {
  flex: 1;
  background: transparent;
  border: none;
  outline: none;
  color: var(--fd-text-1);
  font-family: Consolas, 'JetBrains Mono', monospace;
  font-size: 12px;
  min-width: 0;
}
.fd-path-bar input::placeholder { color: var(--fd-text-3); }
</style>

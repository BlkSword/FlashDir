<template>
  <header class="fd-toolbar">
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
      @click="$emit('scan', path)"
    >
      <svg v-if="loading" class="animate-spin" width="13" height="13" fill="none" viewBox="0 0 24 24">
        <circle class="opacity-25" cx="12" cy="12" r="10" stroke="currentColor" stroke-width="4"></circle>
        <path class="opacity-75" fill="currentColor" d="M4 12a8 8 0 018-8V0C5.373 0 0 5.373 0 12h4zm2 5.291A7.962 7.962 0 014 12H0c0 3.042 1.135 5.824 3 7.938l3-2.647z"></path>
      </svg>
      <svg v-else width="13" height="13" fill="none" stroke="currentColor" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M21 21l-6-6m2-5a7 7 0 11-14 0 7 7 0 0114 0z" /></svg>
      {{ loading ? '扫描中' : '扫描' }}
    </button>

    <button class="fd-btn" @click="$emit('browse')">浏览…</button>

    <div class="fd-path-bar">
      <span class="fd-path-prefix">C:</span>
      <input
        :value="path"
        type="text"
        @keyup.enter="$emit('scan', $event.target.value)"
      />
    </div>

    <div class="fd-search-box">
      <input
        type="text"
        placeholder="全局搜索 (Ctrl+K)"
        @focus="$emit('open-global-search')"
      />
      <svg fill="none" stroke="currentColor" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M21 21l-6-6m2-5a7 7 0 11-14 0 7 7 0 0114 0z" /></svg>
    </div>

    <button class="fd-icon-btn" title="历史记录" @click="$emit('show-history')">
      <svg fill="none" stroke="currentColor" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M12 8v4l3 3m6-3a9 9 0 11-18 0 9 9 0 0118 0z" /></svg>
    </button>

    <button class="fd-icon-btn" title="收起/展开侧边栏" @click="$emit('toggle-sidebar')">
      <svg fill="none" stroke="currentColor" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M4 6h16M4 12h16M4 18h16" /></svg>
    </button>
  </header>
</template>

<script setup>
defineProps({
  path: { type: String, default: '' },
  canGoBack: { type: Boolean, default: false },
  canGoForward: { type: Boolean, default: false },
  canGoUp: { type: Boolean, default: false },
  loading: { type: Boolean, default: false },
})

defineEmits([
  'scan',
  'browse',
  'navigate',
  'show-history',
  'open-global-search',
  'toggle-sidebar',
])
</script>

<style scoped>
.fd-toolbar {
  grid-column: 1 / -1;
  display: flex;
  align-items: center;
  gap: 6px;
  padding: 0 8px;
  background: var(--fd-bg-1);
  border-bottom: 1px solid var(--fd-border);
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
.fd-path-prefix { color: var(--fd-text-2); user-select: none; }
.fd-search-box {
  position: relative;
  width: 220px;
}
.fd-search-box input {
  width: 100%;
  padding: 5px 24px 5px 8px;
  background: var(--fd-bg-0);
  border: 1px solid var(--fd-border);
  border-radius: 3px;
  color: var(--fd-text-1);
  font-size: 12px;
  outline: none;
}
.fd-search-box input:focus { border-color: var(--fd-accent); }
.fd-search-box svg {
  position: absolute;
  right: 6px;
  top: 50%;
  transform: translateY(-50%);
  width: 13px;
  height: 13px;
  color: var(--fd-text-2);
}
</style>

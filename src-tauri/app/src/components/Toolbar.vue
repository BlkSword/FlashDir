<template>
  <header
    class="h-12 shrink-0 flex items-center gap-2 px-3 border-b"
    :class="isDark ? 'bg-slate-900 border-slate-700' : 'bg-white border-slate-300'"
  >
    <!-- Sidebar toggle -->
    <button
      class="p-1.5 rounded transition-colors"
      :class="isDark ? 'hover:bg-slate-800 text-slate-400' : 'hover:bg-slate-100 text-slate-500'"
      title="Toggle sidebar"
      @click="$emit('toggle-sidebar')"
    >
      <svg class="w-4 h-4" fill="none" stroke="currentColor" viewBox="0 0 24 24">
        <path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M4 6h16M4 12h16M4 18h16" />
      </svg>
    </button>

    <!-- Navigation -->
    <div class="flex items-center gap-0.5">
      <button
        class="p-1.5 rounded transition-colors disabled:opacity-30"
        :class="isDark ? 'hover:bg-slate-800 text-slate-400' : 'hover:bg-slate-100 text-slate-500'"
        :disabled="!canGoBack"
        title="Back"
        @click="$emit('navigate', 'back')"
      >
        <svg class="w-4 h-4" fill="none" stroke="currentColor" viewBox="0 0 24 24">
          <path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M15 19l-7-7 7-7" />
        </svg>
      </button>
      <button
        class="p-1.5 rounded transition-colors disabled:opacity-30"
        :class="isDark ? 'hover:bg-slate-800 text-slate-400' : 'hover:bg-slate-100 text-slate-500'"
        :disabled="!canGoForward"
        title="Forward"
        @click="$emit('navigate', 'forward')"
      >
        <svg class="w-4 h-4" fill="none" stroke="currentColor" viewBox="0 0 24 24">
          <path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M9 5l7 7-7 7" />
        </svg>
      </button>
      <button
        class="p-1.5 rounded transition-colors disabled:opacity-30"
        :class="isDark ? 'hover:bg-slate-800 text-slate-400' : 'hover:bg-slate-100 text-slate-500'"
        :disabled="!canGoUp"
        title="Up"
        @click="$emit('navigate', 'up')"
      >
        <svg class="w-4 h-4" fill="none" stroke="currentColor" viewBox="0 0 24 24">
          <path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M5 10l7-7m0 0l7 7m-7-7v18" />
        </svg>
      </button>
    </div>

    <div class="w-px h-5 mx-1" :class="isDark ? 'bg-slate-700' : 'bg-slate-300'"></div>

    <!-- Actions -->
    <button
      class="px-3 py-1.5 rounded text-xs font-medium transition-colors flex items-center gap-1.5"
      :class="isDark ? 'bg-blue-600 hover:bg-blue-500 text-white' : 'bg-blue-600 hover:bg-blue-700 text-white'"
      :disabled="loading"
      @click="$emit('scan')"
    >
      <svg v-if="loading" class="w-3.5 h-3.5 animate-spin" fill="none" viewBox="0 0 24 24">
        <circle class="opacity-25" cx="12" cy="12" r="10" stroke="currentColor" stroke-width="4"></circle>
        <path class="opacity-75" fill="currentColor" d="M4 12a8 8 0 018-8V0C5.373 0 0 5.373 0 12h4zm2 5.291A7.962 7.962 0 014 12H0c0 3.042 1.135 5.824 3 7.938l3-2.647z"></path>
      </svg>
      <svg v-else class="w-3.5 h-3.5" fill="none" stroke="currentColor" viewBox="0 0 24 24">
        <path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M21 21l-6-6m2-5a7 7 0 11-14 0 7 7 0 0114 0z" />
      </svg>
      {{ loading ? '扫描中' : '扫描' }}
    </button>

    <button
      class="px-3 py-1.5 rounded text-xs font-medium border transition-colors"
      :class="isDark ? 'border-slate-600 hover:bg-slate-800 text-slate-300' : 'border-slate-300 hover:bg-slate-50 text-slate-700'"
      @click="$emit('browse')"
    >
      选择目录
    </button>

    <button
      class="p-1.5 rounded transition-colors"
      :class="isDark ? 'hover:bg-slate-800 text-slate-400' : 'hover:bg-slate-100 text-slate-500'"
      title="Refresh"
      @click="$emit('scan')"
    >
      <svg class="w-4 h-4" fill="none" stroke="currentColor" viewBox="0 0 24 24">
        <path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M4 4v5h.582m15.356 2A8.001 8.001 0 004.582 9m0 0H9m11 11v-5h-.581m0 0a8.003 8.003 0 01-15.357-2m15.357 2H15" />
      </svg>
    </button>

    <div class="w-px h-5 mx-1" :class="isDark ? 'bg-slate-700' : 'bg-slate-300'"></div>

    <!-- Path bar -->
    <div class="flex-1 mx-1">
      <div
        class="flex items-center rounded border px-2 py-1"
        :class="isDark ? 'bg-slate-800 border-slate-600' : 'bg-slate-50 border-slate-300'"
      >
        <span class="text-xs mr-2 font-medium" :class="isDark ? 'text-slate-400' : 'text-slate-500'">C:</span>
        <input
          :value="path"
          type="text"
          class="flex-1 bg-transparent outline-none text-xs mono"
          :class="isDark ? 'text-slate-200' : 'text-slate-700'"
          @keyup.enter="$emit('scan', $event.target.value)"
        />
      </div>
    </div>

    <div class="w-px h-5 mx-1" :class="isDark ? 'bg-slate-700' : 'bg-slate-300'"></div>

    <!-- Filter -->
    <div class="relative">
      <input
        :value="filterKeyword"
        type="text"
        placeholder="过滤当前目录..."
        class="w-52 rounded border pl-8 pr-2 py-1.5 text-xs outline-none transition-colors"
        :class="isDark ? 'bg-slate-800 border-slate-600 text-slate-200 focus:border-blue-500 placeholder:text-slate-500' : 'bg-slate-50 border-slate-300 text-slate-700 focus:border-blue-500 placeholder:text-slate-400'"
        @input="$emit('search', $event.target.value)"
      />
      <svg class="w-4 h-4 absolute left-2 top-1.5" :class="isDark ? 'text-slate-500' : 'text-slate-400'" fill="none" stroke="currentColor" viewBox="0 0 24 24">
        <path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M21 21l-6-6m2-5a7 7 0 11-14 0 7 7 0 0114 0z" />
      </svg>
    </div>

    <!-- Global search -->
    <button
      class="px-3 py-1.5 rounded text-xs font-medium border transition-colors flex items-center gap-1.5"
      :class="isDark ? 'border-slate-600 hover:bg-slate-800 text-slate-300' : 'border-slate-300 hover:bg-slate-50 text-slate-700'"
      @click="$emit('open-global-search')"
    >
      <svg class="w-3.5 h-3.5" fill="none" stroke="currentColor" viewBox="0 0 24 24">
        <path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M21 21l-6-6m2-5a7 7 0 11-14 0 7 7 0 0114 0z" />
      </svg>
      全局搜索
    </button>

    <!-- History -->
    <button
      class="p-1.5 rounded transition-colors"
      :class="isDark ? 'hover:bg-slate-800 text-slate-400' : 'hover:bg-slate-100 text-slate-500'"
      title="History"
      @click="$emit('show-history')"
    >
      <svg class="w-4 h-4" fill="none" stroke="currentColor" viewBox="0 0 24 24">
        <path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M12 8v4l3 3m6-3a9 9 0 11-18 0 9 9 0 0118 0z" />
      </svg>
    </button>

    <!-- Theme toggle -->
    <button
      class="p-1.5 rounded transition-colors"
      :class="isDark ? 'hover:bg-slate-800 text-slate-400' : 'hover:bg-slate-100 text-slate-500'"
      title="Toggle theme"
      @click="$emit('toggle-theme')"
    >
      <svg v-if="isDark" class="w-4 h-4" fill="none" stroke="currentColor" viewBox="0 0 24 24">
        <path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M12 3v1m0 16v1m9-9h-1M4 12H3m15.364 6.364l-.707-.707M6.343 6.343l-.707-.707m12.728 0l-.707.707M6.343 17.657l-.707.707M16 12a4 4 0 11-8 0 4 4 0 018 0z" />
      </svg>
      <svg v-else class="w-4 h-4" fill="none" stroke="currentColor" viewBox="0 0 24 24">
        <path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M20.354 15.354A9 9 0 018.646 3.646 9.003 9.003 0 0012 21a9.003 9.003 0 008.354-5.646z" />
      </svg>
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
  filterKeyword: { type: String, default: '' },
  isDark: { type: Boolean, default: false },
})

defineEmits([
  'scan',
  'browse',
  'navigate',
  'show-history',
  'search',
  'open-global-search',
  'toggle-sidebar',
  'toggle-theme',
])
</script>

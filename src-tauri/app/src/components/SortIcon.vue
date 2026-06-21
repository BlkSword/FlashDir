<template>
  <span class="inline-flex flex-col text-[8px] leading-[6px] text-slate-400">
    <span :class="active && direction === 'asc' ? 'text-blue-500' : ''">▲</span>
    <span :class="active && direction === 'desc' ? 'text-blue-500' : ''">▼</span>
  </span>
</template>

<script setup>
defineProps({
  active: { type: Boolean, default: false },
  direction: { type: String, default: 'desc' },
})
</script>

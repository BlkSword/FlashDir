<script setup>
import Icon from './Icon.vue'
defineProps({
  open: { type: Boolean, default: false },
  title: { type: String, default: '' },
  width: { type: String, default: '860px' },
})
const emit = defineEmits(['close'])
</script>

<template>
  <template v-if="open">
    <div class="overlay" @click="emit('close')" />
    <div class="modal" :style="{ width }">
      <div class="modal-head">
        <span>{{ title }}</span>
        <span class="spacer" />
        <button class="theme-btn" title="关闭（Esc）" @click="emit('close')"><Icon name="x" :size="13" /></button>
      </div>
      <div class="modal-body"><slot /></div>
    </div>
  </template>
</template>

<script setup>
import { useToasts } from '../composables/useToasts.js'
import Icon from './Icon.vue'
const { toasts } = useToasts()
</script>

<template>
  <div class="toasts">
    <div v-for="t in toasts" :key="t.id" class="toast" :class="t.kind">
      <Icon :name="t.kind === 'err' ? 'warn' : t.kind === 'warn' ? 'warn' : 'check'" :size="13" />
      <span class="msg">{{ t.text }}</span>
    </div>
  </div>
</template>

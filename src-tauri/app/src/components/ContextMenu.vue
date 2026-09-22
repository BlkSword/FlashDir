<script setup>
import { computed, onMounted, onUnmounted, ref } from 'vue'
import Icon from './Icon.vue'

const props = defineProps({
  open: { type: Boolean, default: false },
  x: { type: Number, default: 0 },
  y: { type: Number, default: 0 },
  items: { type: Array, default: () => [] },
})
const emit = defineEmits(['close', 'pick'])

const style = computed(() => {
  const w = 210
  const h = Math.min(360, props.items.length * 26 + 10)
  const left = Math.min(props.x, window.innerWidth - w - 8)
  const top = Math.min(props.y, window.innerHeight - h - 8)
  return { left: left + 'px', top: top + 'px' }
})

const onDoc = (e) => {
  if (props.open) emit('close')
}
onMounted(() => document.addEventListener('mousedown', onDoc, true))
onUnmounted(() => document.removeEventListener('mousedown', onDoc, true))
</script>

<template>
  <div v-if="open" class="ctx" :style="style" @mousedown.stop>
    <template v-for="(it, i) in items" :key="i">
      <hr v-if="it.sep" />
      <button v-else :disabled="it.disabled" @click="emit('pick', it.id); emit('close')">
        <Icon :name="it.icon || 'right'" :size="13" />
        {{ it.label }}
      </button>
    </template>
  </div>
</template>

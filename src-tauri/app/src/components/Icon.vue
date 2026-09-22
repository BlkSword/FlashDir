<script setup>
defineProps({
  name: { type: String, required: true },
  size: { type: [Number, String], default: 14 },
})
</script>

<template>
  <svg :width="size" :height="size" viewBox="0 0 16 16" aria-hidden="true">
    <use :href="`#i-${name}`" />
  </svg>
</template>

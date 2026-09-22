import { ref } from 'vue'

const toasts = ref([])
let seq = 0

function push(kind, text, ms = 3200) {
  const id = ++seq
  toasts.value.push({ id, kind, text })
  setTimeout(() => {
    toasts.value = toasts.value.filter((t) => t.id !== id)
  }, ms)
}

export function useToasts() {
  return {
    toasts,
    ok: (t) => push('ok', t),
    err: (t) => push('err', t, 5200),
    warn: (t) => push('warn', t, 4200),
    info: (t) => push('', t),
  }
}

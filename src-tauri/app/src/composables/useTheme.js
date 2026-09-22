import { ref, watch } from 'vue'

const KEY = 'flashdir.theme'          // 'auto' | 'light' | 'dark'
const mode = ref(localStorage.getItem(KEY) || 'auto')

function apply(m) {
  const root = document.documentElement
  if (m === 'auto') {
    // auto 交给 CSS 的 prefers-color-scheme 分支
    root.setAttribute('data-theme', 'auto')
  } else {
    root.setAttribute('data-theme', m)
  }
}
apply(mode.value)

watch(mode, (m) => {
  localStorage.setItem(KEY, m)
  apply(m)
})

export function useTheme() {
  const cycle = () => {
    mode.value = mode.value === 'auto' ? 'dark' : mode.value === 'dark' ? 'light' : 'auto'
  }
  return { mode, cycle }
}

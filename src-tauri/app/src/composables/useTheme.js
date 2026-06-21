import { ref, onMounted } from 'vue'

const isDark = ref(false)

export function useTheme() {
  const applyTheme = (dark) => {
    isDark.value = dark
    if (dark) {
      document.documentElement.classList.add('dark')
    } else {
      document.documentElement.classList.remove('dark')
    }
    try {
      localStorage.setItem('flashdir-theme', dark ? 'dark' : 'light')
    } catch {}
  }

  const toggleTheme = () => {
    applyTheme(!isDark.value)
  }

  onMounted(() => {
    let theme = 'light'
    try {
      theme = localStorage.getItem('flashdir-theme') || 'light'
    } catch {}
    applyTheme(theme === 'dark')
  })

  return {
    isDark,
    toggleTheme,
    applyTheme,
  }
}

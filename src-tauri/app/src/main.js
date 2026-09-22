import { createApp } from 'vue'
import App from './App.vue'
import './style.css'

// 主题：默认跟随系统（data-theme="auto"），用户选择存于 localStorage
document.documentElement.setAttribute('data-theme', localStorage.getItem('flashdir.theme') || 'auto')

createApp(App).mount('#app')

if (import.meta.env.PROD) {
  window.addEventListener('error', (e) => console.error('[Production Error]', e.error))
  window.addEventListener('unhandledrejection', (e) => console.error('[Unhandled Rejection]', e.reason))
}

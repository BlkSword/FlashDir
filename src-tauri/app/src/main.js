import { createApp } from 'vue'
import App from './App.vue'
import './style.css'

// 主题：默认跟随系统（data-theme="auto"），用户选择存于 localStorage
document.documentElement.setAttribute('data-theme', localStorage.getItem('flashdir.theme') || 'auto')

const app = createApp(App)

// 渲染/生命周期错误：不再静默白屏 —— 控制台 + 底部错误条（生产构建会移除 console.*，
// 所以必须给用户一个可见出口）
app.config.errorHandler = (err, instance, info) => {
  const name = (instance && instance.$options && (instance.$options.__name || instance.$options.name)) || '?'
  const detail = info + ' @' + name + ': ' + ((err && (err.message || err)) || '')
  window.__FLASHDIR_UI_ERROR__ = detail
  try {
    console.error('[UI Error]', detail, err)
  } catch (_) {}
  let bar = document.getElementById('fd-error-bar')
  if (!bar) {
    bar = document.createElement('div')
    bar.id = 'fd-error-bar'
    bar.style.cssText =
      'position:fixed;left:0;right:0;bottom:0;z-index:9999;background:#7d3733;color:#fff;' +
      'font:12px/1.6 "Segoe UI","Microsoft YaHei UI",sans-serif;padding:6px 10px;white-space:pre-wrap'
    document.body.appendChild(bar)
  }
  bar.textContent = '界面渲染错误：' + detail + '（可通过 Ctrl+K 运行"查看诊断"反馈）'
}

app.mount('#app')

if (import.meta.env.PROD) {
  window.addEventListener('error', (e) => console.error('[Production Error]', e.error))
  window.addEventListener('unhandledrejection', (e) => console.error('[Unhandled Rejection]', e.reason))
}

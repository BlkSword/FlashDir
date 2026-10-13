import { createApp } from 'vue'
import App from './App.vue'
import './style.css'

// 主题：默认跟随系统（data-theme="auto"），用户选择存于 localStorage
document.documentElement.setAttribute('data-theme', localStorage.getItem('flashdir.theme') || 'auto')

/* ── 界面错误上报 ─────────────────────────────────────────────
   旧实现的问题有两个：
   1) window.__FLASHDIR_UI_ERROR__ 只写不读，全仓没有任何读取点，等于只留了个没人看的变量；
   2) window error / unhandledrejection 的兜底只调 console.error，而 vite.config.js
      生产构建开了 esbuild.drop=['console']，这些监听器在生产包里被削成空函数，
      兜底链路完全失效。
   这里改为「模块级环形数组 + window 全局 + 页面底部红条」三条都不依赖 console 的出口，
   并删掉只写不读的 __FLASHDIR_UI_ERROR__：诊断弹窗（App.vue showDiagnostics）
   直接读 __FLASHDIR_UI_ERRORS__ 整份列表一起展示。 */
const UI_ERROR_MAX = 20
const uiErrors = []
window.__FLASHDIR_UI_ERRORS__ = uiErrors // 诊断弹窗读取的完整列表（最近 20 条）

function reportUiError(detail) {
  const time = new Date().toISOString().slice(11, 19)
  uiErrors.push(`${time} ${detail}`)
  if (uiErrors.length > UI_ERROR_MAX) uiErrors.shift()

  let bar = document.getElementById('fd-error-bar')
  if (!bar) {
    bar = document.createElement('div')
    bar.id = 'fd-error-bar'
    bar.style.cssText =
      'position:fixed;left:0;right:0;bottom:0;z-index:9999;background:#7d3733;color:#fff;' +
      'font:12px/1.6 "Segoe UI","Microsoft YaHei UI",sans-serif;padding:6px 10px;white-space:pre-wrap'
    document.body.appendChild(bar)
  }
  bar.textContent = '界面错误：' + detail + '（可按 Ctrl+K 运行“运行诊断”查看）'
}

const app = createApp(App)

app.config.errorHandler = (err, instance, info) => {
  const name = (instance && instance.$options && (instance.$options.__name || instance.$options.name)) || '?'
  reportUiError(info + ' @' + name + ': ' + ((err && (err.message || err)) || ''))
}

app.mount('#app')

// 生产兜底：未捕获错误与未处理的 Promise 拒绝也走同一条可见链路（不再是 console-only）
if (import.meta.env.PROD) {
  window.addEventListener('error', (e) => {
    reportUiError('[未捕获错误] ' + ((e && (e.message || (e.error && e.error.message))) || String(e)))
  })
  window.addEventListener('unhandledrejection', (e) => {
    const reason = e && e.reason
    reportUiError('[未处理的 Promise 拒绝] ' + ((reason && (reason.message || reason)) || String(reason)))
  })
}

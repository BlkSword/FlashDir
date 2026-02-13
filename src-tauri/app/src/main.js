import { createApp } from 'vue'
import Antd from 'ant-design-vue'
import App from './App.vue'
import 'ant-design-vue/dist/reset.css'
import './style.css'
import lazyDirective from './directives/lazy.js'

const app = createApp(App)

// 注册全局指令
app.directive('lazy', lazyDirective)

// 注册 Ant Design Vue
app.use(Antd)

// 挂载应用
app.mount('#app')

// 开发环境性能提示
if (import.meta.env.DEV) {
  console.log('[Performance] 应用已加载')
  
  // 监控首次渲染时间
  if (typeof performance !== 'undefined' && performance.mark) {
    performance.mark('app-mounted')
    
    // 计算从导航到挂载的时间
    window.addEventListener('load', () => {
      performance.mark('page-loaded')
      performance.measure('page-load', 'navigationStart', 'page-loaded')
      
      const entries = performance.getEntriesByName('page-load')
      if (entries.length > 0) {
        console.log(`[Performance] 页面加载时间: ${entries[0].duration.toFixed(2)}ms`)
      }
    })
  }
}

// 生产环境错误处理
if (import.meta.env.PROD) {
  window.addEventListener('error', (e) => {
    console.error('[Production Error]', e.error)
  })
  
  window.addEventListener('unhandledrejection', (e) => {
    console.error('[Unhandled Promise Rejection]', e.reason)
  })
}

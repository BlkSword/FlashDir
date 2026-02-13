import { nextTick } from 'vue'

/**
 * 懒加载指令
 * 用于图片或组件的懒加载，减少初始渲染压力
 */

// Intersection Observer 实例
let observer = null

// 等待懒加载的元素队列
const lazyQueue = new Map()

/**
 * 创建 Intersection Observer
 */
function createObserver() {
  if (observer) return observer
  
  const options = {
    root: null,
    rootMargin: '50px',  // 提前 50px 开始加载
    threshold: 0.01,
  }
  
  observer = new IntersectionObserver((entries) => {
    entries.forEach(entry => {
      if (entry.isIntersecting) {
        const el = entry.target
        const callback = lazyQueue.get(el)
        
        if (callback) {
          callback(el)
          lazyQueue.delete(el)
        }
        
        observer.unobserve(el)
      }
    })
  }, options)
  
  return observer
}

/**
 * 图片懒加载
 * @param {HTMLElement} el - 图片元素
 * @param {string} src - 图片地址
 */
function lazyLoadImage(el, src) {
  if (!src) return
  
  // 设置占位图或加载状态
  el.classList.add('lazy-loading')
  
  const img = new Image()
  img.onload = () => {
    el.src = src
    el.classList.remove('lazy-loading')
    el.classList.add('lazy-loaded')
  }
  img.onerror = () => {
    el.classList.remove('lazy-loading')
    el.classList.add('lazy-error')
  }
  img.src = src
}

/**
 * 组件懒加载（执行回调）
 * @param {HTMLElement} el - 容器元素
 * @param {Function} callback - 加载回调
 */
function lazyLoadComponent(el, callback) {
  el.classList.add('lazy-loading')
  
  nextTick(() => {
    try {
      callback(el)
      el.classList.remove('lazy-loading')
      el.classList.add('lazy-loaded')
    } catch (error) {
      console.error('懒加载组件失败:', error)
      el.classList.remove('lazy-loading')
      el.classList.add('lazy-error')
    }
  })
}

/**
 * v-lazy 指令
 * 用法:
 *   <!-- 图片懒加载 -->
 *   <img v-lazy="imageSrc" :src="placeholderSrc">
 *   
 *   <!-- 组件懒加载（配合 v-if 使用）-->
 *   <div v-lazy="loadComponent" v-if="shouldLoad">
 */
const lazyDirective = {
  mounted(el, binding) {
    const value = binding.value
    
    if (!value) return
    
    const obs = createObserver()
    
    // 如果是图片元素
    if (el.tagName === 'IMG') {
      const src = value
      // 保存原始 src
      el.dataset.lazySrc = src
      
      lazyQueue.set(el, () => {
        lazyLoadImage(el, src)
      })
    } else {
      // 组件懒加载
      lazyQueue.set(el, () => {
        lazyLoadComponent(el, value)
      })
    }
    
    obs.observe(el)
  },
  
  updated(el, binding) {
    // 更新懒加载地址
    if (el.tagName === 'IMG' && binding.value !== binding.oldValue) {
      el.dataset.lazySrc = binding.value
      
      // 如果已经加载过，重新加载新图片
      if (el.classList.contains('lazy-loaded')) {
        el.classList.remove('lazy-loaded')
        lazyLoadImage(el, binding.value)
      }
    }
  },
  
  unmounted(el) {
    if (observer) {
      observer.unobserve(el)
    }
    lazyQueue.delete(el)
  },
}

/**
 * 批量懒加载优化
 * 用于处理大量元素的懒加载场景
 */
export function useBatchLazyLoader(options = {}) {
  const {
    batchSize = 10,
    interval = 100,
  } = options
  
  let batchQueue = []
  let processing = false
  let intervalId = null
  
  /**
   * 添加元素到批处理队列
   * @param {HTMLElement} el - 元素
   * @param {Function} callback - 加载回调
   */
  const addToBatch = (el, callback) => {
    batchQueue.push({ el, callback })
    
    if (!processing) {
      startProcessing()
    }
  }
  
  /**
   * 开始批处理
   */
  const startProcessing = () => {
    processing = true
    
    intervalId = setInterval(() => {
      if (batchQueue.length === 0) {
        stopProcessing()
        return
      }
      
      // 批量处理
      const batch = batchQueue.splice(0, batchSize)
      
      requestAnimationFrame(() => {
        batch.forEach(({ el, callback }) => {
          try {
            callback(el)
          } catch (error) {
            console.error('批处理懒加载失败:', error)
          }
        })
      })
    }, interval)
  }
  
  /**
   * 停止批处理
   */
  const stopProcessing = () => {
    processing = false
    if (intervalId) {
      clearInterval(intervalId)
      intervalId = null
    }
  }
  
  /**
   * 清空队列
   */
  const clear = () => {
    batchQueue = []
    stopProcessing()
  }
  
  return {
    addToBatch,
    clear,
    get queueSize() {
      return batchQueue.length
    },
  }
}

export default lazyDirective

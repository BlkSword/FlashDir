import { defineConfig } from 'vite'
import vue from '@vitejs/plugin-vue'
import { resolve } from 'path'
import { visualizer } from 'rollup-plugin-visualizer'

// https://vitejs.dev/config/
export default defineConfig(({ mode }) => {
  const isProduction = mode === 'production'
  const shouldAnalyze = process.env.ANALYZE === 'true'

  return {
    root: './src',
    plugins: [
      vue({
        // 启用编译时优化
        template: {
          compilerOptions: {
            // 不将 ant-design-vue 组件识别为自定义元素
            // 只将特定已知 Web Components 标记为自定义元素
            isCustomElement: (tag) => {
              // 显式排除 ant-design-vue 组件
              const antdComponents = [
                'a-button', 'a-input', 'a-input-search', 'a-space', 'a-tooltip',
                'a-pagination', 'a-modal', 'a-tree', 'a-table', 'a-card',
                'a-tag', 'a-empty', 'a-list', 'a-list-item', 'a-list-item-meta'
              ]
              if (antdComponents.includes(tag)) {
                return false
              }
              // 其他以 a- 开头的元素可能是自定义元素
              return tag.startsWith('a-') && !tag.includes('-')
            }
          }
        }
      }),
      // 包大小分析（仅在 ANALYZE=true 时启用）
      shouldAnalyze && visualizer({
        open: true,
        gzipSize: true,
        brotliSize: true,
        filename: 'dist/stats.html'
      })
    ].filter(Boolean),
    resolve: {
      alias: {
        '@': resolve(__dirname, './src'),
      },
      // 使用更高效的模块解析策略
      mainFields: ['module', 'jsnext:main', 'jsnext'],
    },
    build: {
      outDir: '../dist',
      emptyOutDir: true,
      sourcemap: !isProduction,  // 生产环境不生成 source map
      // 启用 CSS 代码分割
      cssCodeSplit: true,
      // 启用 CSS 压缩
      cssMinify: true,
      rollupOptions: {
        output: {
          // 优化代码分割策略
          manualChunks: {
            // 将 Ant Design Vue 单独打包
            'ant-design': ['ant-design-vue', '@ant-design/icons-vue'],
            // 将 Chart.js 单独打包
            'charts': ['chart.js'],
            // Vue 核心
            'vue-core': ['vue'],
            // Tauri API
            'tauri': ['@tauri-apps/api']
          },
          // 使用内容哈希优化缓存
          assetFileNames: (assetInfo) => {
            const info = assetInfo.name.split('.')
            const ext = info[info.length - 1]
            if (/\.(png|jpe?g|gif|svg|webp|ico)$/i.test(assetInfo.name)) {
              return 'assets/images/[name]-[hash][extname]'
            }
            if (/\.(woff2?|eot|ttf|otf)$/i.test(assetInfo.name)) {
              return 'assets/fonts/[name]-[hash][extname]'
            }
            if (ext === 'css') {
              return 'assets/css/[name]-[hash][extname]'
            }
            return 'assets/[name]-[hash][extname]'
          },
          chunkFileNames: 'assets/js/[name]-[hash].js',
          entryFileNames: 'assets/js/[name]-[hash].js',
        },
        // 排除 Tauri 注入的全局变量
        external: [],
      },
      // 启用压缩
      minify: 'terser',
      terserOptions: {
        compress: {
          // 删除 console 和 debugger
          drop_console: isProduction,
          drop_debugger: isProduction,
          // 启用更积极的压缩
          passes: 2,
          // 移除未使用的代码
          pure_funcs: ['console.log', 'console.info', 'console.debug'],
        },
        mangle: {
          // 混淆属性名
          properties: false,
        },
        format: {
          // 删除注释
          comments: false,
        },
      },
      // 设置块大小警告阈值（单位：KB）
      chunkSizeWarningLimit: 500,
      // 启用 brotli 压缩
      reportCompressedSize: true,
    },
    // 优化开发服务器配置
    server: {
      port: 5173,
      strictPort: true,
      // 启用 HMR 优化
      hmr: {
        overlay: false,  // 禁用全屏错误覆盖层
      },
      watch: {
        // 忽略 node_modules 和 dist 目录
        ignored: ['**/node_modules/**', '**/dist/**', '../dist/**'],
        // 使用轮询的间隔（某些文件系统需要）
        interval: 1000,
      },
      // 预热常用文件
      preTransformRequests: true,
    },
    // 优化依赖预构建
    optimizeDeps: {
      // 预构建这些依赖
      include: [
        'vue',
        'ant-design-vue',
        '@ant-design/icons-vue',
        'chart.js',
        '@tauri-apps/api'
      ],
      // 排除某些依赖的预构建
      exclude: [],
      // 强制依赖优化
      force: false,
    },
    // 缓存目录
    cacheDir: '../node_modules/.vite',
    // 定义全局常量
    define: {
      __VUE_OPTIONS_API__: false,  // 禁用 Options API 以减小包体积
      __VUE_PROD_DEVTOOLS__: false,  // 生产环境禁用 devtools
    },
    // ESBuild 优化
    esbuild: {
      // 生产环境删除 console 和 debugger
      drop: isProduction ? ['console', 'debugger'] : [],
      // 目标浏览器
      target: 'es2020',
    },
    // 预渲染优化
    preview: {
      port: 4173,
      strictPort: true,
    },
  }
})

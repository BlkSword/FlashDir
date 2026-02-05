import { defineConfig } from 'vite'
import vue from '@vitejs/plugin-vue'
import { resolve } from 'path'

export default defineConfig({
  root: './src',
  plugins: [vue()],
  resolve: {
    alias: {
      '@': resolve(__dirname, './src'),
    },
  },
  build: {
    outDir: '../dist',
    emptyOutDir: true,
    // 开发模式下不生成 source map，减少文件写入
    sourcemap: false,
    rollupOptions: {
      output: {
        assetFileNames: 'assets/[name]-[hash][extname]',
        chunkFileNames: 'assets/[name]-[hash].js',
        entryFileNames: 'assets/[name]-[hash].js',
      },
    },
  },
  // 优化开发服务器配置
  server: {
    port: 5173,
    strictPort: true,
    // 禁用文件监听时的某些功能
    watch: {
      // 忽略 node_modules 和 dist 目录
      ignored: ['**/node_modules/**', '**/dist/**', '../dist/**'],
    },
  },
  // 清空缓存目录
  cacheDir: '../node_modules/.vite',
})

// 只保留 autoprefixer：主题完全由 style.css 的 CSS 变量实现，
// 全仓无 @tailwind/@apply，也没有任何 Tailwind 工具类，tailwindcss 插件纯属空转。
export default {
  plugins: {
    autoprefixer: {},
  },
}

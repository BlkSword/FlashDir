<script setup>
import { computed } from 'vue'
import Icon from './Icon.vue'
import { formatSize, formatSizeCompact } from '../utils/format.js'

const props = defineProps({
  path: { type: String, default: '' },
  totalItems: { type: Number, default: 0 },
  totalSize: { type: Number, default: 0 },
  fileCount: { type: Number, default: 0 },
  dirCount: { type: Number, default: 0 },
  scanTime: { type: Number, default: 0 },
  cacheSource: { type: String, default: '' },
  mftAvailable: { type: Boolean, default: false },
  isAdmin: { type: Boolean, default: false },
  indexCount: { type: Number, default: 0 },
  /**
   * 索引正在构建/载入。
   * 必须与"未建立"区分开：构建期间 `indexCount` 仍是 0，旧实现只按 count 判断，
   * 于是整个构建过程在状态栏里都显示成"未建立"，用户只能看到"未建立 / N 项"两态。
   */
  indexBusy: { type: Boolean, default: false },
  /** 构建中的简短文案（由 App 依据进度事件 + 已用时长生成，例如"扫描中 · 12s"） */
  indexBusyLabel: { type: String, default: '' },
  indexPartial: { type: Boolean, default: false },
  usnVerified: { type: Boolean, default: false },
  filter: { type: String, default: '' },
  selected: { type: Object, default: null },
  /** MCP 状态：null = 未启用/不可用 */
  mcp: { type: Object, default: null },
})
const emit = defineEmits(['restart-admin', 'index-action', 'diagnostics', 'mcp-config'])

const cacheLabel = computed(() => ({
  memory: '内存命中',
  disk: '磁盘 blob',
  usn: 'USN 增量',
  'usn-disk': 'USN 增量',
  derived: '上层推导',
  scan: props.mftAvailable ? 'MFT 直读' : '目录遍历',
}[props.cacheSource] || '—'))

const indexLabel = computed(() => {
  // 构建中 indexCount 还是 0：不做这一层区分就会显示成"未建立"
  if (props.indexBusy) return props.indexBusyLabel || '构建中…'
  if (!props.indexCount) return '未建立'
  return `${props.indexCount.toLocaleString()} 项`
})
</script>

<template>
  <div class="statusbar">
    <span class="st" :title="mftAvailable ? '扫描走 NTFS MFT 直读（管理员）' : '未以管理员运行，回退目录遍历'">
      <span class="dot" :class="mftAvailable ? '' : 'warn'" />
      {{ mftAvailable ? 'MFT 直读' : '目录遍历' }}
    </span>
    <span class="st click" :title="usnVerified ? '该目录的已校验 USN 已生效，刷新走增量快路径' : '尚无已校验 USN，刷新依赖 mtime 判断'" @click="emit('diagnostics')">
      <span class="dot" :class="usnVerified ? '' : 'idle'" />
      {{ usnVerified ? 'USN 已校验' : 'USN 未校验' }}
    </span>
    <span class="st" :title="'本次结果来源'">
      缓存 <b>{{ cacheLabel }}</b>
      <template v-if="scanTime"> · {{ scanTime < 1 ? Math.round(scanTime * 1000) + 'ms' : scanTime.toFixed(2) + 's' }}</template>
    </span>
    <span
      v-if="mcp"
      class="st click"
      :title="mcp.connected
        ? 'AI 客户端已通过 MCP 连接' + (mcp.client ? '（' + mcp.client + '）' : '') + '，点击查看配置'
        : 'MCP 未连接（点击复制配置到 Claude Desktop / Cursor）'"
      @click="emit('mcp-config')"
    >
      <Icon name="dev" :size="12" />
      MCP
      <b v-if="mcp.connected" style="color:var(--ok)">已连接</b>
      <b v-else>未连接</b>
      <template v-if="mcp.connected && mcp.lastTool"> · {{ mcp.lastTool }}</template>
      <template v-else-if="mcp.calls"> · {{ mcp.calls }} 次</template>
    </span>
    <span
      class="st click"
      :title="indexBusy
        ? '全局索引正在构建：' + (indexBusyLabel || '进行中') + '。构建期间搜索仍可用，结果为已索引部分；完成后自动转为增量跟随'
        : '全局索引：' + indexLabel + '（点击重建/刷新）'"
      @click="emit('index-action')"
    >
      <span v-if="indexBusy" class="dot busy" />
      <Icon v-else name="search" :size="12" />
      索引 <b>{{ indexLabel }}</b>
      <span v-if="indexPartial && !indexBusy" class="badge warn">部分</span>
    </span>
    <span v-if="!isAdmin" class="st click" title="以管理员重启可获得 MFT 直读与 USN 增量" @click="emit('restart-admin')">
      <Icon name="shield" :size="12" />
      以管理员重启
    </span>
    <span class="spacer" />
    <span v-if="selected" class="st optional">
      选中 <b>{{ selected.name }}</b> · {{ formatSizeCompact(selected.size) }}
    </span>
    <span class="st grow">
      {{ totalItems.toLocaleString() }} 项 · {{ formatSize(totalSize) }}
      <template v-if="dirCount"> · {{ fileCount.toLocaleString() }} 文件 / {{ dirCount.toLocaleString() }} 目录</template>
    </span>
    <span v-if="filter" class="st optional">过滤 <b>{{ filter }}</b></span>
    <span class="st" :title="path"><Icon name="folder" :size="12" />{{ (path || '').split('/').filter(Boolean).pop() || '—' }}</span>
  </div>
</template>

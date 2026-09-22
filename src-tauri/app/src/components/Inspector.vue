<script setup>
import { computed } from 'vue'
import Icon from './Icon.vue'
import { formatSize, formatDateTime, formatRelative, percentOf } from '../utils/format.js'

const props = defineProps({
  item: { type: Object, default: null },
  parentPath: { type: String, default: '' },
  parentTotal: { type: Number, default: 0 },
  topFiles: { type: Array, default: () => [] },
  accessReliable: { type: Boolean, default: true },
  cacheSource: { type: String, default: '' },
  mftAvailable: { type: Boolean, default: false },
  snapshotCount: { type: Number, default: 0 },
})
const emit = defineEmits(['open', 'copy', 'search-here', 'duplicates', 'snapshot', 'navigate'])

const sourceLabel = computed(() => ({
  memory: '内存缓存命中',
  disk: '磁盘 blob 缓存',
  usn: 'USN 增量',
  'usn-disk': 'USN 增量（磁盘基底）',
  derived: '上层目录推导',
  scan: props.mftAvailable ? 'MFT 直接读取' : '目录遍历',
}[props.cacheSource] || props.cacheSource || '—'))

const hint = computed(() => {
  const it = props.item
  if (!it) return ''
  if (it.isDir) return '目录'
  if (/(\\|\/)(temp|tmp|crashdumps|cache|caches|logs?)(\\|\/|$)/i.test(it.path || '')) return '临时/缓存类路径，通常可安全清理'
  if (it.mtime && Date.now() / 1000 - it.mtime > 86400 * 180) return '半年以上未修改，可能是冷数据'
  return ''
})

const pct = computed(() => (props.item ? percentOf(props.item.size, props.parentTotal).toFixed(1) + '%' : ''))
</script>

<template>
  <div class="fd-col insp">
    <div class="tree-head">
      <Icon name="info" :size="12" />
      <span>检查器</span>
    </div>
    <div class="insp-scroll">
      <template v-if="item">
        <div class="insp-title">
          <Icon :name="item.isDir ? 'folder' : 'file'" :size="14" />
          <span style="overflow:hidden;text-overflow:ellipsis">{{ item.name }}</span>
        </div>
        <dl class="kv">
          <dt>完整路径</dt>
          <dd :title="item.path">{{ item.path }}</dd>
          <dt>大小</dt>
          <dd>{{ formatSize(item.size) }}<template v-if="!item.isDir"> · 占父目录 {{ pct }}</template></dd>
          <dt>类型</dt>
          <dd>{{ item.isDir ? '目录' : (item.name.includes('.') ? item.name.slice(item.name.lastIndexOf('.') + 1).toUpperCase() + ' 文件' : '文件') }}</dd>
          <dt>修改时间</dt>
          <dd :title="formatDateTime(item.mtime)">{{ formatDateTime(item.mtime) }}（{{ formatRelative(item.mtime) }}）</dd>
          <dt>访问时间</dt>
          <dd>
            <template v-if="accessReliable && item.atime">{{ formatDateTime(item.atime) }}</template>
            <template v-else>该卷未启用访问时间更新</template>
          </dd>
          <dt>结果来源</dt>
          <dd>{{ sourceLabel }}</dd>
          <dt v-if="hint">提示</dt>
          <dd v-if="hint" style="font-family:var(--font-ui);color:var(--warn)">{{ hint }}</dd>
        </dl>

        <div class="chips">
          <span class="chip" @click="emit('open', item)"><Icon name="folder-open" />打开位置</span>
          <span class="chip" @click="emit('copy', item.path)"><Icon name="copy" />复制路径</span>
          <span class="chip" @click="emit('search-here', item.isDir ? item.path : parentPath)"><Icon name="search" />在此搜索</span>
          <span class="chip" @click="emit('duplicates', item.path)"><Icon name="dupes" />查重复</span>
          <span class="chip" @click="emit('snapshot', parentPath)"><Icon name="diff" />保存/对比快照</span>
        </div>
      </template>
      <div v-else class="section-note">
        在文件表中选中一行查看详情。双击可打开所在位置，右键有更多操作。
      </div>

      <template v-if="topFiles.length">
        <div class="insp-h">同目录大文件</div>
        <div class="rows">
          <div v-for="f in topFiles" :key="f.path" class="rowline" style="cursor:pointer" @click="emit('open', f)">
            <span class="k">{{ formatSize(f.size).replace(' ', '') }}</span>
            <span class="p" :title="f.path">{{ f.name }}</span>
          </div>
        </div>
      </template>

      <div class="insp-h">快照</div>
      <div v-if="snapshotCount >= 2" class="section-note">
        该目录已有 {{ snapshotCount }} 份快照，可在下方"快照对比"标签查看变化。
      </div>
      <div v-else class="section-note">
        保存两份以上快照后，可对比目录增长/清理情况（下方"快照对比"标签）。
      </div>
      <div class="chips">
        <span class="chip" @click="emit('snapshot', parentPath)"><Icon name="clock" />保存当前快照</span>
      </div>
    </div>
  </div>
</template>

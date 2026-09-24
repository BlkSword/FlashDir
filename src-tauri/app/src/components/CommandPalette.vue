<script setup>
import { ref, computed, watch, nextTick } from 'vue'
import { invoke } from '@tauri-apps/api/core'
import Icon from './Icon.vue'
import { formatSizeCompact } from '../utils/format.js'
import { searchGlobal } from '../utils/globalSearchApi.js'

const props = defineProps({
  open: { type: Boolean, default: false },
  commands: { type: Array, default: () => [] },
  indexCount: { type: Number, default: 0 },
  indexPartial: { type: Boolean, default: false },
  scope: { type: String, default: '' },
  seed: { type: String, default: '' },
})
const emit = defineEmits(['close', 'run', 'open-path', 'navigate'])

const q = ref('')
const active = ref(0)
const results = ref([])
const searching = ref(false)
/** 索引未就绪 / 空结果时的诊断信息 */
const searchNote = ref('')
const inputRef = ref(null)
let timer = null
let seq = 0

const isCommandMode = computed(() => q.value.trim().startsWith('>'))
const cmdQuery = computed(() => q.value.trim().replace(/^>\s*/, '').toLowerCase())

const filteredCommands = computed(() => {
  const list = props.commands
  if (!cmdQuery.value) return list
  return list.filter((c) =>
    (c.label + ' ' + (c.keywords || '')).toLowerCase().includes(cmdQuery.value)
  )
})

const items = computed(() => {
  if (isCommandMode.value) {
    return filteredCommands.value.map((c) => ({
      kind: 'cmd', id: c.id, label: c.label, hint: c.hint, icon: c.icon, right: c.shortcut || '',
    }))
  }
  return results.value.map((r) => ({
    kind: 'file',
    id: r.path,
    label: r.name || (r.path || '').split('/').pop(),
    path: r.path,
    isDir: r.isDir,
    size: r.size,
    right: formatSizeCompact(r.size),
  }))
})

const runSearch = async () => {
  const query = q.value.trim()
  if (!query || isCommandMode.value) {
    results.value = []
    return
  }
  const my = ++seq
  searching.value = true
  searchNote.value = ''
  try {
    const res = await searchGlobal(query, 60)
    if (my !== seq) return
    results.value = res.results
    if (!res.ready) {
      searchNote.value = '全局索引尚未就绪，正在后台构建；稍后重试或按 Ctrl+K 运行“建立全局索引”'
    } else if (!res.results.length && res.indexSize) {
      searchNote.value =
        '索引中有 ' + res.indexSize.toLocaleString() + ' 项但没有匹配' +
        (res.sampleNames.length ? '；索引内名称示例：' + res.sampleNames.slice(0, 3).join('、') : '')
    }
  } catch (e) {
    if (my === seq) {
      results.value = []
      searchNote.value = '搜索失败：' + (e && (e.message || e))
    }
  } finally {
    if (my === seq) searching.value = false
  }
}

watch(q, () => {
  active.value = 0
  clearTimeout(timer)
  if (isCommandMode.value) { results.value = []; return }
  timer = setTimeout(runSearch, 160)
})

watch(() => props.open, async (v) => {
  if (!v) return
  q.value = props.seed || ''
  results.value = []
  searchNote.value = ''
  active.value = 0
  await nextTick()
  inputRef.value?.focus()
  if (q.value) runSearch()
})

const move = (delta) => {
  const n = items.value.length
  if (!n) return
  active.value = (active.value + delta + n) % n
}

const choose = (idx = active.value) => {
  const it = items.value[idx]
  if (!it) return
  if (it.kind === 'cmd') emit('run', it.id)
  else if (it.isDir) emit('navigate', it.path)
  else emit('open-path', it.path)
  emit('close')
}

const onKey = (e) => {
  if (e.key === 'ArrowDown') { e.preventDefault(); move(1) }
  else if (e.key === 'ArrowUp') { e.preventDefault(); move(-1) }
  else if (e.key === 'Enter') { e.preventDefault(); choose() }
  else if (e.key === 'Escape') { e.preventDefault(); emit('close') }
  else if (e.key === 'Tab') {
    // Tab 在"文件搜索 / 命令"之间来回切换（早期只能单向进入命令模式，用户会以为搜索坏了）
    e.preventDefault()
    q.value = isCommandMode.value ? q.value.replace(/^>\s*/, '') : '> ' + q.value.trim()
    if (!isCommandMode.value) runSearch()
  }
}
</script>

<template>
  <template v-if="open">
    <div class="overlay" @click="emit('close')" />
    <div class="palette" role="dialog">
      <div class="palette-input">
        <Icon :name="isCommandMode ? 'zap' : 'search'" :size="15" />
        <input
          ref="inputRef"
          v-model="q"
          spellcheck="false"
          :placeholder="isCommandMode ? '输入命令名…' : '搜索文件（Everything 式语法）；Tab 切换到命令'"
          @keydown="onKey"
        />
        <span class="scope">
          {{ isCommandMode ? '命令' : (indexPartial ? '部分索引' : '全部索引') }}
          <template v-if="indexCount"> · {{ indexCount.toLocaleString() }} 项</template>
          <template v-if="scope"> · {{ scope }}</template>
        </span>
      </div>

      <div class="palette-list">
        <div v-if="searching" style="padding:10px 12px;color:var(--tx-3);font-size:11.5px">正在搜索…</div>
        <div v-else-if="searchNote" style="padding:8px 12px;color:var(--warn);font-size:11.5px;line-height:1.7">{{ searchNote }}</div>
        <div v-else-if="!items.length && !searchNote" style="padding:10px 12px;color:var(--tx-3);font-size:11.5px">
          <template v-if="isCommandMode">没有匹配的命令。</template>
          <template v-else-if="q.trim()">没有匹配的文件。语法：ext:zip size:&gt;1GB dir:node_modules !tmp type:dir</template>
          <template v-else>输入文件名开始搜索；Tab 切换到命令模式（&gt;）。</template>
        </div>
        <div
          v-for="(it, i) in items"
          :key="it.kind + ':' + it.id"
          class="palette-item"
          :class="{ on: i === active }"
          @mouseenter="active = i"
          @click="choose(i)"
        >
          <Icon class="ico" :name="it.kind === 'cmd' ? (it.icon || 'zap') : (it.isDir ? 'folder' : 'file')" :size="14" />
          <span class="main">
            {{ it.label }}
            <span v-if="it.path" class="p"> — {{ it.path }}</span>
          </span>
          <span class="right">{{ it.right }}</span>
        </div>
      </div>

      <div class="palette-foot">
        <span><kbd>↑</kbd><kbd>↓</kbd> 选择</span>
        <span><kbd>Enter</kbd> 打开</span>
        <span><kbd>Tab</kbd> 命令模式</span>
        <span><kbd>Esc</kbd> 关闭</span>
        <span style="margin-left:auto">文件搜索走全局索引；目录项回车进入并扫描</span>
      </div>
    </div>
  </template>
</template>

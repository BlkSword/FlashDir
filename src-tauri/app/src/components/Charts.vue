<template>
  <div class="chart-panel">
    <div class="chart-panel-header">统计分析</div>
    <div class="chart-panel-content">
      <div class="chart-section">
        <h6>文件类型分布 (按大小)</h6>
        <a-card :bordered="true" size="small">
          <div v-if="typeStats.length === 0" class="chart-placeholder"></div>
          <div v-else class="chart-container">
            <canvas ref="doughnutChart"></canvas>
          </div>
        </a-card>
      </div>

      <div class="chart-section">
        <h6>Top 5 大文件/文件夹</h6>
        <a-card :bordered="true" size="small">
          <div v-if="topItems.length === 0" class="chart-placeholder"></div>
          <div v-else class="chart-container">
            <canvas ref="barChart"></canvas>
          </div>
        </a-card>
      </div>
    </div>
  </div>
</template>

<script setup>
import { ref, watch, onMounted, nextTick, onBeforeUnmount } from 'vue'
import { Chart, registerables } from 'chart.js'
import { formatSize, debounce } from '../utils/format.js'

Chart.register(...registerables)

const props = defineProps({
  items: {
    type: Array,
    default: () => []
  },
  totalSize: {
    type: Number,
    default: 0
  }
})

const doughnutChart = ref(null)
const barChart = ref(null)
let doughnutChartInstance = null
let barChartInstance = null

const typeStats = ref([])
const topItems = ref([])

const updateStats = () => {
  if (props.items.length === 0) {
    typeStats.value = []
    topItems.value = []
    return
  }

  // 计算文件类型分布
  const stats = {}
  const colors = ['#1890ff', '#13c2c2', '#52c41a', '#faad14', '#f5222d', '#722ed1', '#eb2f96', '#fa8c16', '#a0d911', '#2f54eb']

  props.items.forEach(item => {
    if (!item.isDir) {
      const ext = item.name.split('.').pop().toLowerCase() || '无扩展名'
      if (!stats[ext]) {
        stats[ext] = { size: 0, count: 0 }
      }
      stats[ext].size += item.size || 0
      stats[ext].count += 1
    }
  })

  typeStats.value = Object.entries(stats)
    .sort((a, b) => b[1].size - a[1].size)
    .slice(0, 10)
    .map((entry, index) => ({
      type: entry[0],
      size: entry[1].size,
      sizeFormatted: formatSize(entry[1].size),
      percent: props.totalSize > 0 ? ((entry[1].size / props.totalSize) * 100).toFixed(1) : 0,
      color: colors[index % colors.length]
    }))

  // Top 5 项目
  topItems.value = [...props.items]
    .sort((a, b) => (b.size || 0) - (a.size || 0))
    .slice(0, 5)
    .map(item => ({
      ...item,
      name: item.name.length > 30 ? item.name.substring(0, 30) + '...' : item.name
    }))
}

// 创建或更新环形图
const createDoughnutChart = () => {
  if (!doughnutChart.value) return

  const ctx = doughnutChart.value.getContext('2d')

  if (doughnutChartInstance) {
    doughnutChartInstance.data.labels = typeStats.value.map(s => s.type)
    doughnutChartInstance.data.datasets[0].data = typeStats.value.map(s => s.size)
    doughnutChartInstance.data.datasets[0].backgroundColor = typeStats.value.map(s => s.color)
    doughnutChartInstance.update('none')
    return
  }

  doughnutChartInstance = new Chart(ctx, {
    type: 'doughnut',
    data: {
      labels: typeStats.value.map(s => s.type),
      datasets: [{
        data: typeStats.value.map(s => s.size),
        backgroundColor: typeStats.value.map(s => s.color),
        borderWidth: 1
      }]
    },
    options: {
      responsive: true,
      maintainAspectRatio: true,
      plugins: {
        legend: {
          position: 'right',
          labels: {
            boxWidth: 12,
            padding: 8,
            font: {
              size: 11
            },
            generateLabels: (chart) => {
              const data = chart.data
              return data.labels.map((label, i) => ({
                text: `${label} (${typeStats.value[i].percent}%)`,
                fillStyle: data.datasets[0].backgroundColor[i],
                hidden: false,
                index: i
              }))
            }
          }
        },
        tooltip: {
          callbacks: {
            label: (context) => {
              const stat = typeStats.value[context.dataIndex]
              return `${stat.type}: ${stat.sizeFormatted} (${stat.percent}%)`
            }
          }
        }
      }
    }
  })
}

const createBarChart = () => {
  if (!barChart.value) return

  const ctx = barChart.value.getContext('2d')

  if (barChartInstance) {
    barChartInstance.data.labels = topItems.value.map(i => i.name)
    barChartInstance.data.datasets[0].data = topItems.value.map(i => i.size || 0)
    barChartInstance.update('none')
    return
  }

  barChartInstance = new Chart(ctx, {
    type: 'bar',
    data: {
      labels: topItems.value.map(i => i.name),
      datasets: [{
        label: '大小',
        data: topItems.value.map(i => i.size || 0),
        backgroundColor: ['#1890ff', '#13c2c2', '#52c41a', '#faad14', '#f5222d'],
        borderWidth: 1
      }]
    },
    options: {
      indexAxis: 'y',
      responsive: true,
      maintainAspectRatio: true,
      plugins: {
        legend: {
          display: false
        },
        tooltip: {
          callbacks: {
            label: (context) => {
              const item = topItems.value[context.dataIndex]
              return item.sizeFormatted || '0 B'
            }
          }
        }
      },
      scales: {
        x: {
          ticks: {
            callback: (value) => formatSize(value),
            font: {
              size: 10
            }
          }
        },
        y: {
          ticks: {
            font: {
              size: 11
            },
            maxRotation: 0,
            minRotation: 0
          }
        }
      }
    }
  })
}

const debouncedUpdateCharts = debounce(() => {
  updateStats()

  if (typeStats.value.length > 0) {
    nextTick(() => {
      createDoughnutChart()
    })
  } else if (doughnutChartInstance) {
    doughnutChartInstance.destroy()
    doughnutChartInstance = null
  }

  if (topItems.value.length > 0) {
    nextTick(() => {
      createBarChart()
    })
  } else if (barChartInstance) {
    barChartInstance.destroy()
    barChartInstance = null
  }
}, 150) // 150ms 防抖延迟

const updateCharts = () => {
  debouncedUpdateCharts()
}

watch(() => [props.items, props.totalSize], () => {
  updateCharts()
})

onMounted(() => {
  updateCharts()
})

onBeforeUnmount(() => {
  if (doughnutChartInstance) {
    doughnutChartInstance.destroy()
  }
  if (barChartInstance) {
    barChartInstance.destroy()
  }
})
</script>

<style scoped>
.chart-panel {
  width: 320px;
  background: #fafafa;
  border-left: 1px solid #f0f0f0;
  display: flex;
  flex-direction: column;
  overflow: hidden;
}

.chart-panel-header {
  padding: 12px 16px;
  font-size: 14px;
  font-weight: 600;
  border-bottom: 1px solid #f0f0f0;
  background: white;
}

.chart-panel-content {
  flex: 1;
  overflow-y: auto;
  padding: 16px;
}

.chart-section {
  margin-bottom: 24px;
}

.chart-section h6 {
  font-size: 12px;
  font-weight: 600;
  color: #8c8c8c;
  margin-bottom: 12px;
  text-transform: uppercase;
}

.chart-container {
  position: relative;
  height: 200px;
  display: flex;
  align-items: center;
  justify-content: center;
}

.chart-placeholder {
  height: 200px;
  display: flex;
  align-items: center;
  justify-content: center;
  color: #bfbfbf;
  font-size: 12px;
}

.ant-card-body {
  padding: 12px;
}
</style>

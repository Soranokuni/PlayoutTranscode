<template>
  <section class="panel">
    <div class="panel-header">
      <span class="panel-title">GLOBAL ASSET REGISTRY</span>
      <div class="filter-bar">
        <button
          v-for="f in filters"
          :key="f.key"
          :class="['filter-btn', { active: activeFilter === f.key }]"
          @click="activeFilter = f.key"
        >
          {{ f.label }}
          <span class="filter-count">{{ filteredCount(f.key) }}</span>
        </button>
      </div>
      <input
        v-model="search"
        class="search-input"
        placeholder="Search assets..."
      />
    </div>

    <div v-if="!displayedAssets.length" class="empty">
      No assets found.
    </div>

    <div v-else class="table-wrapper">
      <table>
        <thead>
          <tr>
            <th>Name</th>
            <th>Status</th>
            <th>Duration</th>
            <th>Rating</th>
            <th>Folder</th>
            <th>Path</th>
          </tr>
        </thead>
        <tbody>
          <tr v-for="asset in displayedAssets" :key="asset.uuid">
            <td class="cell-name" :title="asset.display_name || asset.uuid.slice(0,8)">
              {{ asset.display_name || asset.uuid.slice(0,8) }}
            </td>
            <td>
              <span :class="['status-chip', asset.status]">{{ asset.status }}</span>
            </td>
            <td class="cell-duration">{{ formatDuration(asset.duration_ms) }}</td>
            <td class="cell-rating">{{ asset.rating || '—' }}</td>
            <td class="cell-folder" :title="asset.virtual_folder">
              {{ asset.virtual_folder === '/' ? '/' : asset.virtual_folder }}
            </td>
            <td class="cell-path" :title="asset.current_path">
              {{ shortFileName(asset.current_path) }}
            </td>
          </tr>
        </tbody>
      </table>
    </div>
  </section>
</template>

<script setup lang="ts">
import { ref, computed } from 'vue'
import type { AssetRecord } from '../composables/useEventStream'

const props = defineProps<{
  assets: AssetRecord[]
}>()

const activeFilter = ref('all')
const search = ref('')

const filters = [
  { key: 'all', label: 'All' },
  { key: 'ready', label: 'Ready' },
  { key: 'error', label: 'Error' },
  { key: 'processing', label: 'Processing' },
]

function shortFileName(path: string) {
  return path?.split('\\').pop()?.split('/').pop() || path
}

function formatDuration(ms: number): string {
  if (!ms || ms <= 0) return '—'
  const totalSecs = ms / 1000
  if (totalSecs < 60) return `${totalSecs.toFixed(1)}s`
  const m = Math.floor(totalSecs / 60)
  const s = Math.floor(totalSecs % 60)
  return `${m}:${s.toString().padStart(2, '0')}`
}

function filteredCount(key: string): number {
  if (key === 'all') return props.assets.length
  return props.assets.filter((a) => a.status === key).length
}

const displayedAssets = computed(() => {
  let list = props.assets
  if (activeFilter.value !== 'all') {
    list = list.filter((a) => a.status === activeFilter.value)
  }
  const q = search.value.toLowerCase()
  if (q) {
    list = list.filter(
      (a) =>
        (a.display_name && a.display_name.toLowerCase().includes(q)) ||
        (a.current_path && a.current_path.toLowerCase().includes(q)),
    )
  }
  return list
})
</script>

<style scoped>
.panel {
  background: var(--bg-panel);
  border: 1px solid var(--border-subtle);
  border-radius: var(--radius-base);
  padding: 16px;
  overflow: hidden;
}
.panel-header {
  display: flex;
  align-items: center;
  gap: 12px;
  margin-bottom: 12px;
  flex-wrap: wrap;
}
.panel-title {
  font-size: 11px;
  font-weight: 800;
  letter-spacing: 0.06em;
  color: var(--accent-cyan);
  white-space: nowrap;
}
.filter-bar {
  display: flex;
  gap: 2px;
  background: var(--bg-surface);
  border-radius: 6px;
  padding: 2px;
}
.filter-btn {
  font-size: 11px;
  font-weight: 600;
  padding: 4px 10px;
  border: none;
  background: transparent;
  color: var(--text-secondary);
  border-radius: 4px;
  cursor: pointer;
  transition: all 0.15s;
  display: flex;
  align-items: center;
  gap: 4px;
}
.filter-btn:hover {
  color: var(--text-primary);
  background: rgba(255,255,255,0.04);
}
.filter-btn.active {
  background: var(--accent-cyan);
  color: #000;
}
.filter-count {
  font-size: 10px;
  opacity: 0.7;
}
.search-input {
  background: var(--bg-surface);
  border: 1px solid var(--border-subtle);
  border-radius: 6px;
  color: var(--text-primary);
  font-size: 12px;
  padding: 5px 10px;
  outline: none;
  width: 180px;
  margin-left: auto;
}
.search-input:focus {
  border-color: var(--accent-cyan);
}
.empty {
  text-align: center;
  padding: 24px;
  color: var(--text-secondary);
  font-size: 13px;
}
.table-wrapper {
  overflow-x: auto;
}
table {
  width: 100%;
  border-collapse: collapse;
  font-size: 12px;
}
thead {
  border-bottom: 1px solid var(--border-subtle);
}
th {
  text-align: left;
  padding: 8px 10px;
  font-size: 10px;
  font-weight: 700;
  text-transform: uppercase;
  letter-spacing: 0.05em;
  color: var(--text-secondary);
  white-space: nowrap;
}
td {
  padding: 8px 10px;
  border-bottom: 1px solid rgba(255,255,255,0.03);
  color: var(--text-primary);
}
tr:last-child td {
  border-bottom: none;
}
tr:hover td {
  background: rgba(255,255,255,0.015);
}
.cell-name {
  max-width: 200px;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  font-weight: 600;
}
.cell-duration {
  font-variant-numeric: tabular-nums;
  white-space: nowrap;
  color: var(--text-secondary);
}
.cell-rating {
  font-weight: 700;
  font-size: 11px;
}
.cell-folder {
  max-width: 140px;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  color: var(--text-secondary);
  font-size: 11px;
}
.cell-path {
  max-width: 240px;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  color: var(--text-secondary);
  font-size: 11px;
}
.status-chip {
  font-size: 10px;
  font-weight: 700;
  text-transform: uppercase;
  letter-spacing: 0.04em;
  padding: 2px 8px;
  border-radius: 10px;
  white-space: nowrap;
}
.status-chip.ready {
  background: rgba(26,127,69,0.15);
  color: var(--accent-emerald);
}
.status-chip.processing {
  background: rgba(51,190,204,0.12);
  color: var(--accent-cyan);
}
.status-chip.error {
  background: rgba(229,57,53,0.12);
  color: var(--accent-crimson);
}
</style>

<template>
  <section class="panel">
    <div class="panel-header">
      <span class="panel-title">GLOBAL ASSET REGISTRY</span>
      <div class="filter-bar">
        <button
          v-for="f in filters"
          :key="f.key"
          :class="['filter-btn', { active: activeFilter === f.key }]"
          :aria-pressed="activeFilter === f.key"
          @click="activeFilter = f.key"
        >
          {{ f.label }}
          <span class="filter-count mono">{{ counts[f.key] ?? 0 }}</span>
        </button>
      </div>
      <input
        v-model="searchInput"
        class="search-input"
        type="search"
        aria-label="Search assets"
        placeholder="Search assets..."
      />
    </div>

    <ConfirmDialog
      :open="pending !== null"
      :title="pending?.title ?? ''"
      :body="pending?.body ?? ''"
      :detail="pending?.detail"
      :confirm-label="pending?.label ?? 'Confirm'"
      :busy="pending !== null && busy.has(pending.uuid)"
      @cancel="pending = null"
      @confirm="runPending"
    >
      <!-- The file question belongs inside the decision, not before it. -->
      <template v-if="pending?.kind === 'purge'" #extra>
        <label class="file-opt">
          <input v-model="alsoDeleteFile" type="checkbox" />
          <span>
            Also delete the media file from disk
            <em v-if="pending.sharedWith" class="file-opt-note">
              — kept anyway: {{ pending.sharedWith }} other entr{{ pending.sharedWith === 1 ? 'y' : 'ies' }} still play this file
            </em>
          </span>
        </label>
        <div class="file-opt-path">{{ pending.path }}</div>
      </template>
    </ConfirmDialog>

    <div v-if="actionMsg" class="action-msg" :class="actionOk ? 'ok' : 'err'" role="status" aria-live="polite">
      {{ actionMsg }}
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
            <th class="col-actions">Actions</th>
          </tr>
        </thead>
        <tbody>
          <tr v-for="asset in visibleAssets" :key="asset.uuid">
            <td class="cell-name" :title="asset.display_name || asset.uuid.slice(0,8)">
              {{ asset.display_name || asset.uuid.slice(0,8) }}
            </td>
            <td>
              <span :class="['status-chip', asset.status]">{{ asset.status }}</span>
              <!-- Otherwise an operator leaves the file in the watch folder,
                   sees nothing happen, and concludes the watcher is broken. -->
              <span
                v-if="heldBack(asset)"
                class="held-chip"
                :title="`This media already failed on ${heldBack(asset)} under the current settings, so it is not encoded again. Use Try again to force one more attempt.`"
              >won't retry</span>
            </td>
            <td class="cell-duration">{{ formatDuration(asset.duration_ms) }}</td>
            <td class="cell-rating">{{ asset.rating || '—' }}</td>
            <td class="cell-folder" :title="asset.virtual_folder">
              {{ asset.virtual_folder === '/' ? '/' : asset.virtual_folder }}
            </td>
            <td class="cell-path" :title="asset.current_path">
              {{ shortFileName(asset.current_path) }}
            </td>
            <td class="cell-actions">
              <button
                v-if="heldBack(asset)"
                class="btn btn-mini"
                :disabled="busy.has(asset.uuid)"
                title="Examine this media again the next time it is offered, even though it failed before."
                @click="onTryAgain(asset)"
              >Try again</button>
              <button
                class="btn btn-mini"
                :disabled="busy.has(asset.uuid)"
                title="Move this entry to the recycle bin. The media file is not touched and you can restore it."
                @click="askTrash(asset)"
              >Remove</button>
              <button
                class="btn btn-mini btn-delete"
                :disabled="busy.has(asset.uuid)"
                title="Delete this entry for good, and optionally its media file."
                @click="askPurge(asset)"
              >Delete…</button>
            </td>
          </tr>
        </tbody>
      </table>
      <!--
        The grid used to render one <tr> per asset for the whole library. At a
        few thousand assets that is a few thousand rows built on every filter
        change, for a table nobody scrolls past the first screen of (SF-04).
      -->
      <div v-if="visibleAssets.length < displayedAssets.length" class="show-more">
        <button class="btn" @click="showMore">
          Show more ({{ displayedAssets.length - visibleAssets.length }} remaining)
        </button>
        <span class="text-muted show-more-count mono">
          Showing {{ visibleAssets.length }} of {{ displayedAssets.length }}
        </span>
      </div>
    </div>
  </section>
</template>

<script setup lang="ts">
import ConfirmDialog from './ConfirmDialog.vue'
import { ref, computed, watch, onUnmounted } from 'vue'
import type { AssetRecord } from '../composables/useEventStream'

/**
 * The delete actions arrive as props rather than being pulled from
 * `useEventStream` here: that composable is not a singleton, so calling it in a
 * second component would open a second SSE connection to the service. App.vue
 * owns the connection and hands the wired calls down.
 */
const props = defineProps<{
  assets: AssetRecord[]
  trashAsset: (uuid: string) => Promise<{ success: boolean; error?: string }>
  purgeAsset: (
    uuid: string,
    deleteFile: boolean,
  ) => Promise<{ success: boolean; mediaRemoved: boolean; warnings: string[]; error?: string }>
  clearAssetVerdict: (uuid: string) => Promise<{ success: boolean; error?: string }>
}>()

/**
 * Raised after any action that changes the registry, so the owner can refetch.
 * Without it the row an operator just deleted stays on screen until the next
 * poll, which reads as "the button did nothing".
 */
const emit = defineEmits<{ (e: 'changed', uuid: string): void }>()

const activeFilter = ref('all')
const search = ref('')
const searchInput = ref('')

// Same debounce the DB tab already uses, so typing does not refilter the whole
// library on every keystroke.
const SEARCH_DEBOUNCE_MS = 250
let searchTimer = 0
watch(searchInput, (value) => {
  window.clearTimeout(searchTimer)
  searchTimer = window.setTimeout(() => {
    search.value = value
  }, SEARCH_DEBOUNCE_MS)
})
onUnmounted(() => {
  window.clearTimeout(searchTimer)
  window.clearTimeout(flashTimer)
})

const PAGE = 100
const shown = ref(PAGE)
function showMore() {
  shown.value += PAGE
}
// Any change to what is being listed starts again from the first page.
watch([activeFilter, search], () => {
  shown.value = PAGE
})

const filters = [
  { key: 'all', label: 'All' },
  { key: 'ready', label: 'Ready' },
  { key: 'error', label: 'Error' },
  // T-4. The server now says so itself when a published mezzanine is no longer
  // on disk, instead of leaving it looking airable until someone tries.
  { key: 'missing', label: 'Missing' },
  { key: 'processing', label: 'Processing' },
]

/**
 * Is the service refusing to encode this media again, and why?
 *
 * `retry_suppressed` comes from the server, which is the only side that knows:
 * it depends on a recorded verdict key the payload does not carry. Inferring it
 * here from the status and the warnings looked right and was wrong the moment
 * an operator pressed Try again -- neither of those changes, so the badge went
 * on claiming the media would not be retried after it had been released.
 *
 * Returns the reason for the tooltip, or '' when the row is not held back.
 */
const ENVIRONMENTAL = ['keyframe_scan_failed']
function heldBack(asset: AssetRecord): string {
  if (!asset.retry_suppressed) return ''
  const real = (asset.warnings ?? []).filter((w) => !ENVIRONMENTAL.includes(w))
  return real.join(', ') || 'a previous failure'
}

type PendingAction = {
  kind: 'trash' | 'purge'
  uuid: string
  title: string
  body: string
  detail?: string
  label: string
  path?: string
  sharedWith?: number
}

const pending = ref<PendingAction | null>(null)
const alsoDeleteFile = ref(false)
/** Uuids with a request in flight. A single slot let two rows overwrite each
 *  other's busy state. */
const busy = ref(new Set<string>())
function setBusy(uuid: string, on: boolean) {
  const next = new Set(busy.value)
  if (on) next.add(uuid)
  else next.delete(uuid)
  busy.value = next
}
const actionMsg = ref('')
const actionOk = ref(false)

let flashTimer = 0
function flash(msg: string, ok: boolean) {
  actionMsg.value = msg
  actionOk.value = ok
  // An older message's timer used to clear a newer message early.
  window.clearTimeout(flashTimer)
  flashTimer = window.setTimeout(() => { actionMsg.value = '' }, 5000)
}

function assetLabel(a: AssetRecord) {
  return a.display_name || a.uuid.slice(0, 8)
}

/** How many other rows play the same physical file (i.e. its sub-clips). */
function othersSharingFile(a: AssetRecord): number {
  if (!a.current_path) return 0
  return props.assets.filter((x) => x.uuid !== a.uuid && x.current_path === a.current_path).length
}

function askTrash(a: AssetRecord) {
  pending.value = {
    kind: 'trash',
    uuid: a.uuid,
    title: `Remove "${assetLabel(a)}" from the library?`,
    body: 'The entry moves to the recycle bin. The media file stays exactly where it is.',
    detail: 'You can put it back from the recycle bin at any time.',
    label: 'Remove',
  }
}

function askPurge(a: AssetRecord) {
  const shared = othersSharingFile(a)
  // Default the checkbox off. The reversible choice is the one a tired
  // operator should land on by pressing Return.
  alsoDeleteFile.value = false
  pending.value = {
    kind: 'purge',
    uuid: a.uuid,
    title: `Delete "${assetLabel(a)}" permanently?`,
    body: 'The registry entry is removed for good. This cannot be undone from the recycle bin.',
    detail: shared
      ? 'Its media file is shared with other entries, so the file itself is kept whatever you choose below.'
      : undefined,
    label: 'Delete permanently',
    path: a.current_path,
    sharedWith: shared || undefined,
  }
}

async function onTryAgain(a: AssetRecord) {
  if (busy.value.has(a.uuid)) return
  setBusy(a.uuid, true)
  try {
    const r = await props.clearAssetVerdict(a.uuid)
    if (r.success) emit('changed', a.uuid)
    flash(
      r.success
        ? `"${assetLabel(a)}" will be examined again next time it is offered.`
        : r.error || 'Could not clear the verdict',
      r.success,
    )
  } finally {
    setBusy(a.uuid, false)
  }
}

async function runPending() {
  const p = pending.value
  if (!p || busy.value.has(p.uuid)) return
  setBusy(p.uuid, true)
  try {
    if (p.kind === 'trash') {
      const r = await props.trashAsset(p.uuid)
      if (r.success) emit('changed', p.uuid)
      flash(r.success ? 'Moved to the recycle bin.' : r.error || 'Could not remove', r.success)
    } else {
      const r = await props.purgeAsset(p.uuid, alsoDeleteFile.value)
      if (r.success) emit('changed', p.uuid)
      if (!r.success) {
        flash(r.error || 'Could not delete', false)
      } else if (alsoDeleteFile.value && !r.mediaRemoved) {
        // Say so plainly rather than reporting a success the operator did not get.
        flash(
          'Entry deleted. The media file was kept: ' +
            (r.warnings[0] || 'something else still references it.'),
          true,
        )
      } else {
        flash(r.mediaRemoved ? 'Entry and media file deleted.' : 'Entry deleted.', true)
      }
    }
  } finally {
    setBusy(p.uuid, false)
    pending.value = null
  }
}

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

/** All the chip counts in one pass, instead of one `filter` per chip on
 *  every render. */
const counts = computed<Record<string, number>>(() => {
  // `missing` was absent here, so `asset.status in out` never matched and the
  // Missing chip always read 0.
  const out: Record<string, number> = { all: 0, ready: 0, error: 0, missing: 0, processing: 0 }
  for (const asset of props.assets) {
    out.all!++
    if (asset.status in out) out[asset.status]!++
  }
  return out
})

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

const visibleAssets = computed(() => displayedAssets.value.slice(0, shown.value))
</script>

<style scoped>
.show-more {
  display: flex;
  align-items: center;
  gap: 12px;
  padding: 10px 2px 2px;
}

.show-more-count {
  font-size: 12px;
}

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
.col-actions, .cell-actions {
  white-space: nowrap;
  text-align: right;
}
.cell-actions .btn-mini + .btn-mini {
  margin-left: 6px;
}
/* Not the global `.btn-danger`, which is a filled red block: one of those per
   row turns the whole library into a wall of alarm and stops meaning anything.
   Quiet until hovered, loud once it is the thing under the cursor. */
.btn-delete {
  color: var(--accent-crimson);
  border-color: var(--border-subtle);
}
.btn-delete:hover:not(:disabled) {
  background: var(--accent-crimson);
  border-color: var(--accent-crimson);
  color: #fff;
}
.held-chip {
  margin-left: 6px;
  font-size: 9px;
  font-weight: 700;
  text-transform: uppercase;
  letter-spacing: 0.04em;
  padding: 2px 6px;
  border-radius: 10px;
  border: 1px solid var(--accent-crimson);
  color: var(--accent-crimson);
  white-space: nowrap;
}
.file-opt {
  display: flex;
  gap: 8px;
  align-items: flex-start;
  cursor: pointer;
}
.file-opt-note {
  display: block;
  color: var(--text-secondary);
  font-style: normal;
  font-size: 12px;
}
.file-opt-path {
  margin-top: 6px;
  font-family: monospace;
  font-size: 11px;
  color: var(--text-secondary);
  word-break: break-all;
}
.action-msg {
  margin-bottom: 10px;
  font-size: 12px;
}
.action-msg.ok { color: var(--accent-emerald); }
.action-msg.err { color: var(--accent-crimson); }
.status-chip.missing {
  background: rgba(229,57,53,0.22);
  color: var(--accent-crimson);
}
</style>

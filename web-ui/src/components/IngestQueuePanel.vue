<template>
  <section class="panel">
    <div class="panel-header">
      <span class="panel-title">ACTIVE INGEST QUEUE</span>
      <span class="panel-badge">{{ processing.length + queued.length + failed.length }}</span>
      <button
        v-if="failed.length"
        class="btn btn-retry-all"
        :disabled="retryingAll"
        @click="onRetryAll"
      >
        <span v-if="retryingAll">Retrying…</span>
        <span v-else>Retry all failed ({{ failed.length }})</span>
      </button>
      <button
        v-if="failed.length"
        class="btn btn-clear-all"
        :disabled="clearingAll"
        :title="`Remove ${failed.length} failed job record${failed.length === 1 ? '' : 's'} from this list. Assets and media files are not touched.`"
        @click="onClearAll"
      >
        <span v-if="clearingAll">Clearing…</span>
        <span v-else>Clear all ({{ failed.length }})</span>
      </button>
      <span
        v-if="retryMsg"
        class="retry-msg"
        :class="retryOk ? 'ok' : 'err'"
        role="status"
        aria-live="polite"
      >{{ retryMsg }}</span>
    </div>

    <div v-if="!processing.length && !queued.length && !failed.length" class="empty">
      No active or failed ingests.
    </div>

    <div v-else class="queue-list">
      <div v-for="job in processing" :key="job.id" class="queue-row">
        <div class="queue-main">
          <span v-if="job.attempt && job.attempt > 1" class="retry-chip" title="Retry attempt">
            ⟳ #{{ job.attempt }}<span v-if="job.max_attempts">/{{ job.max_attempts }}</span>
          </span>
          <span class="queue-filename">{{ shortFileName(job.input_path) }}</span>
          <span v-if="job.phase" class="queue-phase" :class="{ cancelling: isCancelling(job) }">
            {{ isCancelling(job) ? 'cancelling' : job.phase.replace('_', ' ') }}
          </span>
          <span class="queue-profile">{{ job.profile }}</span>
          <ProgressBar
            :percent="job.progress"
            :determinate="(job.duration_ms || 0) > 0 || job.duration_secs > 0 || job.source_frame_count > 0"
            :speed="job.encode_speed"
            :duration-ms="job.duration_ms || (job.duration_secs || 0) * 1000"
            :current-time-ms="job.current_time_ms || 0"
          />
          <span v-if="job.encode_fps" class="queue-fps">{{ Math.round(job.encode_fps) }} fps</span>
          <span v-if="job.encode_bitrate" class="queue-bitrate">{{ job.encode_bitrate }}</span>
          <!-- Publishing is the atomic rename + registry write. It cannot be
               interrupted, so a cancel there only promised something the
               server would refuse. -->
          <button
            v-if="job.phase !== 'publishing'"
            class="btn btn-mini btn-cancel"
            :disabled="busy.has(job.id) || isCancelling(job)"
            :aria-label="`Cancel the transcode of ${shortFileName(job.input_path)}`"
            :title="isCancelling(job) ? 'Cancelling…' : 'Cancel transcode job'"
            @click="run(job.id, () => cancel(job.id))"
          >
            <span v-if="busy.has(job.id) || isCancelling(job)" aria-hidden="true">…</span>
            <span v-else aria-hidden="true">✕</span>
          </button>
        </div>
        <div v-if="job.source_frame_count" class="queue-meta">
          Frame {{ job.current_frame }}/{{ job.source_frame_count }}
          <span v-if="job.duration_secs">| {{ job.duration_secs.toFixed(1) }}s</span>
          <span v-if="job.uuid">| {{ job.uuid.slice(0, 8) }}</span>
        </div>
      </div>

      <!-- Waiting behind max_concurrency, or re-queued by a retry. These used
           to appear nowhere, so a retried job simply vanished until it
           started. -->
      <div v-for="job in queued" :key="job.id" class="queue-row">
        <div class="queue-main">
          <span v-if="job.attempt && job.attempt > 1" class="retry-chip" title="Retry attempt">
            ⟳ #{{ job.attempt }}<span v-if="job.max_attempts">/{{ job.max_attempts }}</span>
          </span>
          <span class="queue-filename">{{ shortFileName(job.input_path) }}</span>
          <span class="queue-phase queued">queued</span>
          <span class="queue-note">{{ job.current_stage }}</span>
          <button
            class="btn btn-mini btn-cancel"
            :disabled="busy.has(job.id)"
            :aria-label="`Cancel the queued job for ${shortFileName(job.input_path)}`"
            title="Take this job out of the queue"
            @click="run(job.id, () => cancel(job.id))"
          >
            <span v-if="busy.has(job.id)" aria-hidden="true">…</span>
            <span v-else aria-hidden="true">✕</span>
          </button>
        </div>
      </div>

      <div v-for="job in failed" :key="job.id" class="error-alert">
        <div class="error-header">
          <span class="queue-status failed">Failed</span>
          <span v-if="job.attempt && job.attempt > 1" class="retry-chip">⟳ #{{ job.attempt }}<span v-if="job.max_attempts">/{{ job.max_attempts }}</span></span>
          <span class="queue-filename">{{ shortFileName(job.input_path) }}</span>
          <span
            v-if="job.error_category"
            class="error-category"
            :title="job.error_category"
          >⚠ {{ describeErrorCategory(job.error_category).label }}</span>
          <span class="queue-profile">{{ job.profile }}</span>
          <div class="error-actions">
            <button
              class="btn btn-mini"
              :disabled="busy.has(job.id)"
              :aria-label="`Retry ${shortFileName(job.input_path)}`"
              @click="run(job.id, () => retry(job.id))"
            >
              <span v-if="busy.has(job.id)">…</span>
              <span v-else>Retry</span>
            </button>
            <button
              class="btn btn-mini btn-dismiss"
              :disabled="busy.has(job.id)"
              :aria-label="`Dismiss the failed job for ${shortFileName(job.input_path)}`"
              title="Remove this job record from the list. The asset and its media file are not touched."
              @click="run(job.id, () => dismiss(job.id))"
            >
              <span v-if="busy.has(job.id)" aria-hidden="true">…</span>
              <span v-else aria-hidden="true">✕</span>
            </button>
          </div>
        </div>
        <div class="error-summary">{{ shortError(job.error) }}</div>
        <div v-if="describeErrorCategory(job.error_category).hint" class="error-hint">
          {{ describeErrorCategory(job.error_category).hint }}
        </div>
        <!-- The work was done and only the bookkeeping failed, so there is a
             finished mezzanine on disk that nothing references. -->
        <div v-if="describeErrorCategory(job.error_category).quarantined" class="error-quarantine">
          The encoded file was kept in the target folder's <code>quarantine\</code>
          directory. Nothing references it, so it is safe to inspect or delete.
        </div>
        <details v-if="job.stderr_log && job.stderr_log.length" class="error-details">
          <summary>ffmpeg stderr tail ({{ job.stderr_log.length }} lines)</summary>
          <pre class="error-body">{{ job.stderr_log.join('\n') }}</pre>
        </details>
      </div>
    </div>

    <!--
      A job that finished used to vanish, and a duplicate that was skipped
      (state Completed, phase skipped) never appeared anywhere but as a number
      in the stats. An operator who dropped a file and saw nothing could not
      tell "done" from "ignored" (UX-04). Cancelled jobs are listed too: they
      fell out of every bucket, so a cancel looked like nothing had happened.
    -->
    <details v-if="recent.length" class="recent">
      <summary class="recent-summary">
        Recent ({{ recent.length }})
      </summary>
      <div class="recent-list">
        <div v-for="job in recent" :key="job.id" class="recent-row">
          <span class="recent-badge" :class="recentKind(job)">
            {{ recentLabel(job) }}
          </span>
          <span class="queue-filename">{{ shortFileName(job.input_path) }}</span>
          <span v-if="job.phase === 'skipped' && job.uuid" class="recent-note">
            duplicate of {{ job.uuid.slice(0, 8) }}
          </span>
          <span v-if="job.duration_secs" class="recent-note mono">
            {{ job.duration_secs.toFixed(1) }}s
          </span>
          <span v-if="job.finished_at" class="recent-note mono">{{ clockTime(job.finished_at) }}</span>
          <button
            class="btn btn-mini btn-dismiss recent-dismiss"
            :disabled="busy.has(job.id)"
            :aria-label="`Remove ${shortFileName(job.input_path)} from the recent list`"
            title="Remove this record from the list. Assets and media files are not touched."
            @click="run(job.id, () => dismiss(job.id))"
          >
            <span aria-hidden="true">✕</span>
          </button>
        </div>
      </div>
    </details>
  </section>
</template>

<script setup lang="ts">
import { computed, onUnmounted, ref } from 'vue'
import type { JobRecord } from '../composables/useEventStream'
import { describeErrorCategory } from '../lib/errorCategories'
import ProgressBar from './ProgressBar.vue'

/**
 * Row actions are async function props rather than emits, so a button stays
 * busy exactly as long as its request. With emits the spinner reset after a
 * fixed 400 ms whatever the server was doing, and a stale Retry was clickable
 * again while the first retry was still in flight.
 */
const props = defineProps<{
  jobs: Map<string, JobRecord>
  retry: (id: string) => Promise<unknown>
  cancel: (id: string) => Promise<unknown>
  dismiss: (id: string) => Promise<unknown>
  clearAll: () => Promise<unknown>
}>()

const emit = defineEmits<{
  (e: 'retry-all'): void
}>()

function shortFileName(path: string) {
  return path?.split('\\').pop()?.split('/').pop() || path
}

function shortError(err?: string): string {
  if (!err) return 'Unknown error'
  const first = (err.split('\n')[0] || '').trim()
  return first.length > 220 ? first.slice(0, 217) + '…' : first
}

function isCancelling(job: JobRecord): boolean {
  return job.phase === 'cancel_requested' || !!job.cancel_requested
}

/** One pass over the Map instead of one per list. */
const buckets = computed(() => {
  const processing: JobRecord[] = []
  const queued: JobRecord[] = []
  const failed: JobRecord[] = []
  const finished: JobRecord[] = []
  for (const job of props.jobs.values()) {
    if (job.state === 'Processing') processing.push(job)
    else if (job.state === 'Pending') queued.push(job)
    else if (job.state === 'Failed') failed.push(job)
    else finished.push(job)
  }
  queued.sort((a, b) => a.created_at.localeCompare(b.created_at))
  return { processing, queued, failed, finished }
})

const processing = computed(() => buckets.value.processing)
const queued = computed(() => buckets.value.queued)
const failed = computed(() => buckets.value.failed)

const RECENT_LIMIT = 20
const recent = computed(() =>
  buckets.value.finished
    .slice()
    .sort((a, b) => (b.finished_at || '').localeCompare(a.finished_at || ''))
    .slice(0, RECENT_LIMIT),
)

function recentKind(job: JobRecord): string {
  if (job.state === 'Cancelled') return 'cancelled'
  return job.phase === 'skipped' ? 'skipped' : 'done'
}

function recentLabel(job: JobRecord): string {
  if (job.state === 'Cancelled') return '✕ cancelled'
  return job.phase === 'skipped' ? '⊘ skipped' : '✓ completed'
}

function clockTime(iso: string): string {
  const d = new Date(iso)
  return Number.isNaN(d.getTime())
    ? ''
    : d.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit', second: '2-digit' })
}

/** Ids with a request in flight: one slot per row, so rows don't fight. */
const busy = ref(new Set<string>())
const clearingAll = ref(false)
const retryingAll = ref(false)
const retryMsg = ref('')
const retryOk = ref(false)
let msgTimer = 0

async function run(id: string, action: () => Promise<unknown>) {
  if (busy.value.has(id)) return
  busy.value = new Set(busy.value).add(id)
  try {
    await action()
  } finally {
    const next = new Set(busy.value)
    next.delete(id)
    busy.value = next
  }
}

async function onClearAll() {
  if (clearingAll.value) return
  clearingAll.value = true
  try {
    await props.clearAll()
  } finally {
    clearingAll.value = false
  }
}

function onRetryAll() {
  retryMsg.value = ''
  emit('retry-all')
}

function showRetryMsg(msg: string, ok: boolean) {
  retryMsg.value = msg
  retryOk.value = ok
  // An older message's timer used to clear a newer message early.
  window.clearTimeout(msgTimer)
  msgTimer = window.setTimeout(() => { retryMsg.value = '' }, 4000)
}

/** Driven by App for the confirm-then-retry-all round trip. */
function setRetryingAll(value: boolean) {
  retryingAll.value = value
}

onUnmounted(() => window.clearTimeout(msgTimer))

defineExpose({ showRetryMsg, setRetryingAll })
</script>

<style scoped>
/* Quieter than Retry: clearing the list is the housekeeping action, not the
   one an operator came here to press. */
.btn-clear-all {
  background: transparent;
  border: 1px solid var(--border-subtle);
  color: var(--text-secondary);
}
.btn-clear-all:hover:not(:disabled) {
  color: var(--text-primary);
  border-color: var(--text-secondary);
}
.btn-dismiss {
  min-width: 28px;
  padding-inline: 8px;
  color: var(--text-secondary);
}
.btn-dismiss:hover:not(:disabled) {
  color: var(--accent-crimson);
}
.panel {
  background: var(--bg-panel);
  border: 1px solid var(--border-subtle);
  border-radius: var(--radius-base);
  padding: 16px;
  margin-bottom: 16px;
}
.panel-header {
  display: flex;
  align-items: center;
  gap: 8px;
  margin-bottom: 12px;
}
.panel-title {
  font-size: 11px;
  font-weight: 800;
  letter-spacing: 0.06em;
  color: var(--accent-cyan);
}
.panel-badge {
  font-size: 11px;
  font-weight: 700;
  background: rgba(51,190,204,0.12);
  color: var(--accent-cyan);
  border-radius: 10px;
  padding: 1px 8px;
}
.btn-retry-all {
  margin-left: auto;
  font-size: 11px;
  font-weight: 700;
  padding: 4px 10px;
  border: 1px solid var(--accent-amber);
  background: rgba(255,170,40,0.08);
  color: var(--accent-amber);
  border-radius: 4px;
  cursor: pointer;
}
.btn-retry-all:disabled {
  opacity: 0.6;
  cursor: default;
}
.retry-msg {
  font-size: 11px;
  font-weight: 600;
  padding: 2px 8px;
}
.retry-msg.ok { color: var(--accent-emerald); }
.retry-msg.err { color: var(--accent-crimson); }
.empty {
  text-align: center;
  padding: 24px;
  color: var(--text-secondary);
  font-size: 13px;
}
.queue-list {
  display: flex;
  flex-direction: column;
  gap: 2px;
}
.queue-row {
  padding: 8px 10px;
  border-bottom: 1px solid rgba(255,255,255,0.03);
  transition: background 0.15s;
}
.queue-row:last-child {
  border-bottom: none;
}
.queue-row:hover {
  background: rgba(255,255,255,0.02);
}
.queue-main {
  display: flex;
  align-items: center;
  gap: 10px;
  flex-wrap: wrap;
}
.queue-filename {
  font-size: 13px;
  font-weight: 600;
  color: var(--text-primary);
  min-width: 160px;
  max-width: 280px;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.queue-profile {
  font-size: 11px;
  font-weight: 700;
  color: var(--accent-cyan);
  background: rgba(51,190,204,0.08);
  padding: 1px 6px;
  border-radius: 4px;
}
.queue-phase {
  font-size: 10px;
  font-weight: 700;
  text-transform: uppercase;
  letter-spacing: 0.05em;
  color: var(--accent-emerald);
  background: rgba(46,204,113,0.1);
  padding: 1px 6px;
  border-radius: 4px;
}
.error-category {
  font-size: 10px;
  font-weight: 700;
  color: var(--accent-amber);
  background: rgba(255,170,40,0.1);
  padding: 1px 6px;
  border-radius: 4px;
}

.error-hint {
  font-size: 12px;
  line-height: 1.5;
  color: var(--text-secondary);
  margin-top: 4px;
}

.error-quarantine {
  font-size: 12px;
  line-height: 1.5;
  margin-top: 8px;
  padding: 7px 10px;
  color: var(--accent-amber);
  background: rgba(245, 166, 35, 0.08);
  border-left: 2px solid var(--accent-amber);
  border-radius: 3px;
}

.recent {
  margin-top: 10px;
  border-top: 1px solid var(--border-subtle);
  padding-top: 8px;
}

.recent-summary {
  cursor: pointer;
  font-size: 12px;
  color: var(--text-secondary);
  user-select: none;
}

.recent-list {
  margin-top: 8px;
  display: flex;
  flex-direction: column;
  gap: 4px;
}

.recent-row {
  display: flex;
  align-items: center;
  gap: 10px;
  font-size: 12px;
  padding: 3px 2px;
}

.recent-badge {
  font-size: 10px;
  font-weight: 700;
  padding: 1px 6px;
  border-radius: 4px;
  white-space: nowrap;
}

.recent-badge.done {
  color: var(--accent-emerald);
  background: rgba(63, 185, 80, 0.1);
}

.recent-badge.skipped {
  color: var(--text-secondary);
  background: rgba(255, 255, 255, 0.05);
}

.recent-badge.cancelled {
  color: var(--accent-amber);
  background: rgba(255, 170, 40, 0.1);
}

.recent-dismiss {
  margin-left: auto;
  padding: 0 6px;
  min-width: 0;
}

.queue-phase.queued {
  color: var(--text-secondary);
  background: rgba(255, 255, 255, 0.05);
}

.queue-phase.cancelling {
  color: var(--accent-amber);
  background: rgba(255, 170, 40, 0.1);
}

.queue-note {
  font-size: 11px;
  color: var(--text-secondary);
  flex: 1;
}

.recent-note {
  font-size: 11px;
  color: var(--text-secondary);
}
.btn-cancel {
  border-color: rgba(229,57,53,0.4);
  color: var(--accent-crimson);
  background: rgba(229,57,53,0.08);
  padding: 2px 6px;
}
.btn-cancel:hover:not(:disabled) {
  background: rgba(229,57,53,0.2);
}
.retry-chip {
  font-size: 10px;
  font-weight: 700;
  background: rgba(255,170,40,0.12);
  color: var(--accent-amber);
  padding: 1px 6px;
  border-radius: 4px;
}
.queue-fps {
  font-size: 11px;
  color: var(--text-secondary);
  font-variant-numeric: tabular-nums;
}
.queue-bitrate {
  font-size: 11px;
  color: var(--text-secondary);
}
.queue-meta {
  font-size: 10px;
  color: var(--text-secondary);
  padding-left: 8px;
  margin-top: 2px;
}
.error-alert {
  background: rgba(229,57,53,0.06);
  border: 1px solid rgba(229,57,53,0.18);
  border-radius: 6px;
  padding: 10px 12px;
  margin-bottom: 4px;
}
.error-header {
  display: flex;
  align-items: center;
  gap: 8px;
  margin-bottom: 6px;
}
.queue-status {
  font-size: 10px;
  font-weight: 800;
  text-transform: uppercase;
  letter-spacing: 0.06em;
  padding: 1px 6px;
  border-radius: 4px;
}
.queue-status.failed {
  background: rgba(229,57,53,0.15);
  color: var(--accent-crimson);
}
.error-actions {
  margin-left: auto;
}
.btn-mini {
  font-size: 10px;
  font-weight: 700;
  padding: 3px 10px;
  border: 1px solid var(--accent-cyan);
  background: rgba(51,190,204,0.08);
  color: var(--accent-cyan);
  border-radius: 4px;
  cursor: pointer;
}
.btn-mini:disabled {
  opacity: 0.5;
  cursor: default;
}
.error-summary {
  font-size: 12px;
  line-height: 1.4;
  color: var(--accent-crimson);
  word-break: break-word;
}
.error-details {
  margin-top: 6px;
}
.error-details > summary {
  font-size: 10px;
  cursor: pointer;
  color: var(--text-secondary);
  user-select: none;
  padding: 2px 0;
}
.error-details[open] > summary {
  margin-bottom: 4px;
}
.error-body {
  font-family: 'Cascadia Code', 'Consolas', monospace;
  font-size: 10px;
  line-height: 1.4;
  color: rgba(255,160,150,0.85);
  white-space: pre-wrap;
  word-break: break-word;
  margin: 0;
  max-height: 220px;
  overflow: auto;
  background: rgba(0,0,0,0.25);
  padding: 6px 8px;
  border-radius: 4px;
}
</style>
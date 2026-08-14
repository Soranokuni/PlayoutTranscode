<template>
  <section class="panel">
    <div class="panel-header">
      <span class="panel-title">ACTIVE INGEST QUEUE</span>
      <span class="panel-badge">{{ processing.length + failed.length }}</span>
      <button
        v-if="failed.length"
        class="btn btn-retry-all"
        :disabled="retryingAll"
        @click="onRetryAll"
      >
        <span v-if="retryingAll">Retrying…</span>
        <span v-else>Retry all failed ({{ failed.length }})</span>
      </button>
      <span v-if="retryMsg" class="retry-msg" :class="retryOk ? 'ok' : 'err'">{{ retryMsg }}</span>
    </div>

    <div v-if="!processing.length && !failed.length" class="empty">
      No active or failed ingests.
    </div>

    <div v-else class="queue-list">
      <div v-for="job in processing" :key="job.id" class="queue-row">
        <div class="queue-main">
          <span v-if="job.attempt && job.attempt > 0" class="retry-chip" title="Retry attempt">
            ⟳ #{{ job.attempt }}<span v-if="job.max_attempts">/{{ job.max_attempts }}</span>
          </span>
          <span class="queue-filename">{{ shortFileName(job.input_path) }}</span>
          <span v-if="job.phase" class="queue-phase">{{ job.phase }}</span>
          <span class="queue-profile">{{ job.profile }}</span>
          <ProgressBar
            :percent="job.progress"
            :determinate="job.duration_secs > 0 || job.source_frame_count > 0"
            :speed="job.encode_speed"
            :duration-ms="(job.duration_secs || 0) * 1000"
            :current-time-ms="job.current_time_ms || 0"
          />
          <span v-if="job.encode_fps" class="queue-fps">{{ Math.round(job.encode_fps) }} fps</span>
          <span v-if="job.encode_bitrate" class="queue-bitrate">{{ job.encode_bitrate }}</span>
          <button
            class="btn btn-mini btn-cancel"
            :disabled="cancellingId === job.id"
            title="Cancel transcode job"
            @click="onCancel(job.id)"
          >
            <span v-if="cancellingId === job.id">…</span>
            <span v-else>✕</span>
          </button>
        </div>
        <div v-if="job.source_frame_count" class="queue-meta">
          Frame {{ job.current_frame }}/{{ job.source_frame_count }}
          <span v-if="job.duration_secs">| {{ job.duration_secs.toFixed(1) }}s</span>
          <span v-if="job.uuid">| {{ job.uuid.slice(0, 8) }}</span>
        </div>
      </div>

      <div v-for="job in failed" :key="job.id" class="error-alert">
        <div class="error-header">
          <span class="queue-status failed">Failed</span>
          <span v-if="job.attempt && job.attempt > 0" class="retry-chip">⟳ #{{ job.attempt }}<span v-if="job.max_attempts">/{{ job.max_attempts }}</span></span>
          <span class="queue-filename">{{ shortFileName(job.input_path) }}</span>
          <span v-if="job.error_category" class="error-category">{{ job.error_category }}</span>
          <span class="queue-profile">{{ job.profile }}</span>
          <div class="error-actions">
            <button class="btn btn-mini" :disabled="retryingId === job.id" @click="onRetry(job.id)">
              <span v-if="retryingId === job.id">…</span>
              <span v-else>Retry</span>
            </button>
          </div>
        </div>
        <div class="error-summary">{{ shortError(job.error) }}</div>
        <details v-if="job.stderr_log && job.stderr_log.length" class="error-details">
          <summary>ffmpeg stderr tail ({{ job.stderr_log.length }} lines)</summary>
          <pre class="error-body">{{ job.stderr_log.join('\n') }}</pre>
        </details>
      </div>
    </div>
  </section>
</template>

<script setup lang="ts">
import { computed, ref } from 'vue'
import type { JobRecord } from '../composables/useEventStream'
import ProgressBar from './ProgressBar.vue'

const props = defineProps<{
  jobs: Map<string, JobRecord>
}>()

const emit = defineEmits<{
  (e: 'retry', id: string): void
  (e: 'cancel', id: string): void
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

const processing = computed(() =>
  Array.from(props.jobs.values()).filter((j) => j.state === 'Processing')
)
const failed = computed(() =>
  Array.from(props.jobs.values()).filter((j) => j.state === 'Failed')
)

const retryingId = ref<string | null>(null)
const cancellingId = ref<string | null>(null)
const retryingAll = ref(false)
const retryMsg = ref('')
const retryOk = ref(false)

async function onRetry(id: string) {
  retryingId.value = id
  try {
    emit('retry', id)
  } finally {
    setTimeout(() => { retryingId.value = null }, 400)
  }
}

async function onCancel(id: string) {
  cancellingId.value = id
  try {
    emit('cancel', id)
  } finally {
    setTimeout(() => { cancellingId.value = null }, 400)
  }
}

async function onRetryAll() {
  retryingAll.value = true
  retryMsg.value = ''
  try {
    emit('retry-all')
  } finally {
    setTimeout(() => { retryingAll.value = false }, 600)
  }
}

defineExpose({ showRetryMsg: (msg: string, ok: boolean) => { retryMsg.value = msg; retryOk.value = ok; setTimeout(() => { retryMsg.value = '' }, 4000) } })
</script>

<style scoped>
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
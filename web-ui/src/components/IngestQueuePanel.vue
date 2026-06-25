<template>
  <section class="panel">
    <div class="panel-header">
      <span class="panel-title">ACTIVE INGEST QUEUE</span>
      <span class="panel-badge">{{ processing.length + failed.length }}</span>
    </div>

    <div v-if="!processing.length && !failed.length" class="empty">
      No active or failed ingests.
    </div>

    <div v-else class="queue-list">
      <div
        v-for="job in processing"
        :key="job.id"
        class="queue-row"
      >
        <div class="queue-main">
          <span class="queue-filename">{{ shortFileName(job.input_path) }}</span>
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
        </div>
        <div v-if="job.source_frame_count" class="queue-meta">
          Frame {{ job.current_frame }}/{{ job.source_frame_count }}
          <span v-if="job.duration_secs">| {{ job.duration_secs.toFixed(1) }}s</span>
          <span v-if="job.uuid">| {{ job.uuid.slice(0, 8) }}</span>
        </div>
      </div>

      <div
        v-for="job in failed"
        :key="job.id"
        class="error-alert"
      >
        <div class="error-header">
          <span class="queue-status failed">Failed</span>
          <span class="queue-filename">{{ shortFileName(job.input_path) }}</span>
          <span class="queue-profile">{{ job.profile }}</span>
        </div>
        <pre class="error-body">{{ job.error || 'Unknown error' }}</pre>
      </div>
    </div>
  </section>
</template>

<script setup lang="ts">
import { computed } from 'vue'
import type { JobRecord } from '../composables/useEventStream'
import ProgressBar from './ProgressBar.vue'

const props = defineProps<{
  jobs: Map<string, JobRecord>
}>()

function shortFileName(path: string) {
  return path?.split('\\').pop()?.split('/').pop() || path
}

const processing = computed(() =>
  Array.from(props.jobs.values()).filter((j) => j.state === 'Processing')
)

const failed = computed(() =>
  Array.from(props.jobs.values()).filter((j) => j.state === 'Failed')
)
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
.error-body {
  font-family: 'Cascadia Code', 'Consolas', monospace;
  font-size: 11px;
  line-height: 1.5;
  color: var(--accent-crimson);
  white-space: pre-wrap;
  word-break: break-word;
  margin: 0;
}
</style>

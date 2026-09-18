<template>
  <header class="top-bar">
    <div class="tb-left">
      <h1 class="tb-title">PlayoutTranscode</h1>
      <span class="tb-divider"></span>

      <div class="tb-path-block">
        <span class="tb-label">Watch</span>
        <span class="tb-path" :title="watch?.watch_folder">{{ watch?.watch_folder || '—' }}</span>
      </div>
      <div class="tb-path-block">
        <span class="tb-label">Target</span>
        <span class="tb-path" :title="watch?.target_folder">{{ watch?.target_folder || '—' }}</span>
      </div>

      <span class="tb-divider"></span>

      <div class="tb-stat">
        <span class="tb-label">Concurrency</span>
        <span class="tb-value">{{ watch?.max_concurrency ?? '—' }}</span>
      </div>
      <div class="tb-stat">
        <span class="tb-label">Uptime</span>
        <span class="tb-value mono">{{ uptimeText }}</span>
      </div>

      <span class="tb-divider"></span>

      <div class="tb-tool">
        <span class="status-dot" :class="tool.ffmpeg_found ? 'ok' : 'err'" />
        <span class="tb-tool-name">ffmpeg</span>
        <span v-if="tool.ffmpeg_version" class="tb-tool-ver">{{ toolShortVer(tool.ffmpeg_version) }}</span>
        <span v-else class="text-danger" style="font-size:10px">MISSING</span>
        <button class="tb-btn-dl" :disabled="dl" @click="$emit('download')">Download</button>
      </div>

      <div v-if="dl" class="tb-dl-indicator">
        <span class="spinner"></span>
        <span class="text-warning" style="font-size:11px">Downloading FFmpeg...</span>
      </div>
    </div>

    <div class="tb-right">
      <!-- How the page is getting its data. A dead stream behind a working
           poll used to look exactly like a live one. -->
      <div class="tb-link" :title="linkTitle">
        <span class="status-dot" :class="link === 'live' ? 'ok' : 'warn'" />
        <span :class="link === 'live' ? 'text-muted' : 'text-warning'" class="tb-link-label">
          {{ link === 'live' ? 'Live' : 'Reconnecting' }}
        </span>
      </div>

      <div class="tb-service">
        <span class="status-dot" :class="serviceDot" />
        <span :class="serviceTextClass" class="tb-service-label">
          {{ serviceGlyph }} {{ serviceLabel }}
        </span>
        <button
          v-if="!running"
          class="btn btn-primary tb-action"
          :disabled="busy || transitioning"
          @click="$emit('start')"
        >Start</button>
        <button
          v-else
          class="btn btn-danger tb-action"
          :disabled="busy || transitioning"
          @click="$emit('stop')"
        >Stop</button>
      </div>
    </div>
  </header>
</template>

<script setup lang="ts">
import { computed } from 'vue'
import type { LinkState, ToolchainPayload, WatchfolderPayload } from '../composables/useEventStream'

const props = defineProps<{
  watch: WatchfolderPayload | null
  tool: ToolchainPayload
  running: boolean
  /** The full lifecycle state from `/api/service/status`, which the boolean
   *  `running` collapses. Undefined until the first status fetch lands. */
  serviceState?: string
  /** Whether the SSE stream is up. */
  link: LinkState
  /** A start/stop request is in flight. */
  busy?: boolean
  dl: boolean
  uptime: number
}>()

defineEmits<{
  start: []
  stop: []
  download: []
}>()

/** `starting` and `stopping` are not states to act on. */
const transitioning = computed(
  () => props.serviceState === 'starting' || props.serviceState === 'stopping',
)

const serviceDot = computed(() => {
  switch (props.serviceState) {
    case 'running': return 'ok'
    case 'starting':
    case 'stopping': return 'warn'
    case 'stopped': return 'idle'
    default: return props.running ? 'ok' : 'idle'
  }
})

/** A glyph beside every colour-coded state, for operators who cannot rely on
 *  the dot (UX-07). */
const serviceGlyph = computed(() => {
  switch (serviceDot.value) {
    case 'ok': return '●'
    case 'warn': return '◐'
    default: return '○'
  }
})

const serviceLabel = computed(() => {
  switch (props.serviceState) {
    case 'running': return 'Ingest running'
    case 'starting': return 'Starting…'
    case 'stopping': return 'Stopping…'
    // The HTTP server answered, so the service is reachable -- what is stopped
    // is the ingest loop. A bare "Stopped" read as "nothing is up".
    case 'stopped': return 'Service reachable, ingest stopped'
    default: return props.running ? 'Ingest running' : 'Ingest stopped'
  }
})

const serviceTextClass = computed(() => {
  switch (serviceDot.value) {
    case 'ok': return 'text-success'
    case 'warn': return 'text-warning'
    default: return 'text-muted'
  }
})

const linkTitle = computed(() =>
  props.link === 'live'
    ? 'Live: updates arrive over the event stream'
    : 'The event stream is down; falling back to polling every 2 s',
)

function toolShortVer(v: string) {
  return v ? v.split(' ').pop() || v : ''
}

const uptimeText = computed(() => {
  const totalSecs = Math.floor(props.uptime / 1000)
  const h = Math.floor(totalSecs / 3600)
  const m = Math.floor((totalSecs % 3600) / 60)
  const s = totalSecs % 60
  if (h > 0) return `${h}h ${m}m`
  return `${m}m ${s}s`
})
</script>

<style scoped>
.top-bar {
  background: var(--bg-panel);
  border-bottom: 1px solid var(--border-subtle);
  padding: 8px 20px;
  display: flex;
  justify-content: space-between;
  align-items: center;
  gap: 12px;
  flex-shrink: 0;
}
.tb-left {
  display: flex;
  align-items: center;
  gap: 10px;
  flex-wrap: wrap;
}
.tb-right {
  display: flex;
  align-items: center;
  gap: 8px;
}
.tb-title {
  font-size: 16px;
  font-weight: 700;
  color: var(--accent-cyan);
  white-space: nowrap;
  letter-spacing: 0.02em;
}
.tb-divider {
  width: 1px;
  height: 22px;
  background: var(--border-subtle);
}
.tb-path-block {
  display: flex;
  align-items: center;
  gap: 4px;
}
.tb-label {
  font-size: 10px;
  font-weight: 700;
  text-transform: uppercase;
  letter-spacing: 0.05em;
  color: var(--text-secondary);
}
.tb-path {
  font-size: 12px;
  max-width: 180px;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  color: var(--text-primary);
}
.tb-stat {
  display: flex;
  align-items: center;
  gap: 4px;
}
.tb-link {
  display: flex;
  align-items: center;
  gap: 4px;
  margin-right: 12px;
}
.tb-link-label {
  font-size: 11px;
  font-weight: 600;
}
.tb-service-label {
  font-size: 12px;
  font-weight: 600;
}
/* Repeated inline styles moved into a class (UX-07). */
.tb-action {
  padding: 4px 14px;
  font-size: 12px;
}
.tb-value {
  font-size: 12px;
  font-weight: 600;
  font-variant-numeric: tabular-nums;
  color: var(--text-primary);
}
.tb-tool {
  display: flex;
  align-items: center;
  gap: 4px;
  background: var(--bg-surface);
  border: 1px solid var(--border-subtle);
  border-radius: 6px;
  padding: 3px 8px;
}
.tb-tool-name {
  font-size: 11px;
  font-weight: 600;
}
.tb-tool-ver {
  font-size: 10px;
  color: var(--text-secondary);
}
.tb-btn-dl {
  font-size: 10px;
  padding: 2px 8px;
  background: rgba(248,180,0,0.12);
  border: 1px solid var(--accent-amber);
  color: var(--accent-amber);
  border-radius: 4px;
  cursor: pointer;
  font-weight: 600;
  margin-left: 4px;
}
.tb-btn-dl:hover:not(:disabled) {
  background: rgba(248,180,0,0.22);
}
.tb-btn-dl:disabled {
  opacity: 0.35;
  cursor: not-allowed;
}
.tb-dl-indicator {
  display: flex;
  align-items: center;
  gap: 6px;
}
.tb-service {
  display: flex;
  align-items: center;
  gap: 6px;
}
.spinner {
  width: 12px;
  height: 12px;
  border: 2px solid var(--border-subtle);
  border-top-color: var(--accent-amber);
  border-radius: 50%;
  animation: spin 0.8s linear infinite;
}
@keyframes spin {
  to { transform: rotate(360deg); }
}
</style>

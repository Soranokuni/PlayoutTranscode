<template>
  <div class="app-shell">
    <header class="header">
      <div class="header-left">
        <h1 class="app-title">PlayoutTranscode</h1>
        <span class="header-divider"></span>

        <div class="tool-card">
          <div class="tool-row">
            <span class="status-dot" :class="toolchain.ffmpeg_found ? 'ok' : 'err'" />
            <span class="tool-name">ffmpeg</span>
            <span v-if="toolchain.ffmpeg_version" class="tool-ver text-muted">{{ toolShortVer(toolchain.ffmpeg_version) }}</span>
            <span v-else class="text-danger" style="font-size:11px">MISSING</span>
          </div>
          <button class="btn btn-download" style="font-size:11px;padding:3px 10px" :disabled="downloading" @click="downloadFFmpeg">
            {{ toolchain.ffmpeg_found ? 'Reinstall' : 'Download' }}
          </button>
        </div>

        <div class="tool-card">
          <div class="tool-row">
            <span class="status-dot" :class="toolchain.ffprobe_found ? 'ok' : 'err'" />
            <span class="tool-name">ffprobe</span>
            <span v-if="toolchain.ffprobe_version" class="tool-ver text-muted">{{ toolShortVer(toolchain.ffprobe_version) }}</span>
            <span v-else class="text-danger" style="font-size:11px">MISSING</span>
          </div>
          <button class="btn btn-download" style="font-size:11px;padding:3px 10px" :disabled="downloading" @click="downloadFFmpeg">
            {{ toolchain.ffprobe_found ? 'Reinstall' : 'Download' }}
          </button>
        </div>

        <div v-if="downloading" class="download-indicator">
          <span class="spinner"></span>
          <span class="text-warning" style="font-size:12px">Downloading FFmpeg...</span>
        </div>
      </div>

      <div class="header-right">
        <div class="service-ctrl">
          <span class="status-dot" :class="serviceRunning ? 'ok' : 'idle'" />
          <span :class="serviceRunning ? 'text-success' : 'text-muted'" style="font-size:13px;font-weight:500">
            {{ serviceRunning ? 'Running' : 'Stopped' }}
          </span>
          <button v-if="!serviceRunning" class="btn btn-primary" style="padding:4px 14px;font-size:12px" @click="startService">Start</button>
          <button v-else class="btn btn-danger" style="padding:4px 14px;font-size:12px" @click="stopService">Stop</button>
        </div>
        <button class="btn" style="font-size:12px" @click="installService">Install Service</button>
        <button class="btn" style="font-size:12px" @click="uninstallService">Uninstall</button>
      </div>
    </header>

    <nav class="tab-bar">
      <button v-for="t in tabs" :key="t.id" :class="['tab-btn', { active: activeTab === t.id }]" @click="activeTab = t.id">
        {{ t.label }}
      </button>
    </nav>

    <main class="main-content">
      <div v-if="activeTab === 'dashboard'" class="tab-panel">
        <section class="bento-row">
          <div class="bento-card">
            <div class="card-header text-accent">TOOLCHAIN</div>
            <div class="card-body">
              <div class="tool-status-line">
                <span class="status-dot" :class="toolchain.ffmpeg_found ? 'ok' : 'err'" />
                <span>ffmpeg</span>
                <span v-if="toolchain.ffmpeg_version" class="text-muted" style="font-size:11px">{{ toolShortVer(toolchain.ffmpeg_version) }}</span>
              </div>
              <div class="tool-status-line">
                <span class="status-dot" :class="toolchain.ffprobe_found ? 'ok' : 'err'" />
                <span>ffprobe</span>
                <span v-if="toolchain.ffprobe_version" class="text-muted" style="font-size:11px">{{ toolShortVer(toolchain.ffprobe_version) }}</span>
              </div>
              <button class="btn btn-download" style="margin-top:8px;width:100%" :disabled="downloading" @click="downloadFFmpeg">
                {{ toolchain.ffmpeg_found ? 'Reinstall FFmpeg' : 'Download FFmpeg' }}
              </button>
            </div>
          </div>

          <div class="bento-card">
            <div class="card-header" :class="serviceRunning ? 'text-success' : 'text-muted'">SERVICE</div>
            <div class="card-body">
              <div style="font-size:18px;font-weight:700" :class="serviceRunning ? 'text-success' : 'text-muted'">
                {{ serviceRunning ? 'Running' : 'Stopped' }}
              </div>
              <div v-if="serviceRunning" class="text-muted" style="font-size:12px;margin-top:4px">
                {{ stats.active }} active &middot; {{ stats.completed }} done &middot; {{ stats.failed }} failed
              </div>
              <div style="margin-top:8px;display:flex;gap:8px">
                <button v-if="!serviceRunning" class="btn btn-primary" style="flex:1" @click="startService">Start</button>
                <button v-else class="btn btn-danger" style="flex:1" @click="stopService">Stop</button>
              </div>
            </div>
          </div>

          <div class="bento-card">
            <div class="card-header text-accent">JOBS</div>
            <div class="card-body">
              <div class="stats-grid">
                <div class="stat"><span class="stat-num text-accent">{{ stats.active }}</span><span class="stat-label">Active</span></div>
                <div class="stat"><span class="stat-num text-success">{{ stats.completed }}</span><span class="stat-label">Done</span></div>
                <div class="stat"><span class="stat-num text-danger">{{ stats.failed }}</span><span class="stat-label">Failed</span></div>
                <div class="stat"><span class="stat-num text-warning">{{ stats.pending }}</span><span class="stat-label">Pending</span></div>
                <div class="stat"><span class="stat-num text-muted">{{ stats.total }}</span><span class="stat-label">Total</span></div>
              </div>
            </div>
          </div>

          <div class="bento-card">
            <div class="card-header text-accent">PATHS</div>
            <div class="card-body">
              <div class="path-row" :title="config.paths?.watch_folder">
                <span class="text-muted" style="font-size:11px;margin-right:4px">Watch:</span>
                <span style="font-size:12px;word-break:break-all">{{ config.paths?.watch_folder || '(not set)' }}</span>
              </div>
              <div class="path-row" :title="config.paths?.target_folder + '/videos/'">
                <span class="text-muted" style="font-size:11px;margin-right:4px">Videos:</span>
                <span style="font-size:12px;word-break:break-all">{{ config.paths?.target_folder ? config.paths.target_folder + '/videos/' : '(not set)' }}</span>
              </div>

            </div>
          </div>

          <div class="bento-card">
            <div class="card-header text-warning">ENCODING</div>
            <div class="card-body">
              <div style="font-size:12px">Preset: {{ config.encoding?.preset }} &middot; Tune: {{ config.encoding?.tune }}</div>
              <div style="font-size:12px">Audio: {{ config.encoding?.audio_codec }}</div>
              <div style="font-size:11px;color:var(--text-secondary);margin-top:2px">
                A: CRF {{ config.profiles?.a?.crf }} &middot; B: CRF {{ config.profiles?.b?.crf }} &middot; C: CRF {{ config.profiles?.c?.crf }}
              </div>
            </div>
          </div>
        </section>

        <section v-if="activeJobs.length" class="glass-panel" style="padding:16px;margin-top:16px">
          <div class="card-header text-accent" style="margin-bottom:12px">ACTIVE JOBS ({{ activeJobs.length }})</div>
          <div v-for="job in activeJobs" :key="job.id" class="job-row">
            <div class="job-main">
              <span class="mono" style="font-weight:600">{{ shortFileName(job.input_path) }}</span>
              <div class="progress-bar"><div class="progress-fill" :style="{ width: job.progress + '%' }" /></div>
              <span style="font-size:12px">{{ Math.round(job.progress) }}%</span>
              <span class="text-accent" style="font-size:11px">{{ job.current_stage }}</span>
              <span v-if="job.encode_speed" class="text-muted" style="font-size:11px">{{ Math.round(job.encode_fps) }} fps {{ job.encode_speed }}</span>
            </div>
            <div v-if="job.source_frame_count" class="text-muted" style="font-size:10px;padding-left:8px">
              Frame {{ job.current_frame }}/{{ job.source_frame_count }} | Profile: {{ job.profile }} | {{ job.duration_secs?.toFixed(1) }}s
            </div>
          </div>
        </section>

        <section v-if="recentJobs.length" class="glass-panel" style="padding:16px;margin-top:12px">
          <div class="card-header text-muted" style="margin-bottom:12px">RECENT ({{ recentJobs.length }})</div>
          <div v-for="job in recentJobs" :key="job.id" class="recent-job">
            <span>{{ job.state === 'Completed' ? '✅' : '❌' }}</span>
            <span style="font-size:12px">{{ shortFileName(job.input_path) }}</span>
            <span v-if="job.error" class="text-danger" style="font-size:11px">{{ job.error }}</span>
          </div>
        </section>

        <div v-if="!serviceRunning && !stats.total" class="empty-state">
          <div style="font-size:18px;color:var(--text-secondary);margin-bottom:12px">Service is stopped</div>
          <button class="btn btn-play" @click="startService">▶  Start Service</button>
        </div>
      </div>

      <div v-if="activeTab === 'config'" class="tab-panel">
        <div class="glass-panel config-section">
          <div class="card-header text-accent">FILE PATHS</div>
          <div class="form-row">
            <label>Watch Folder</label>
            <input v-model="editWatchFolder" class="glass-input" style="flex:1" />
          </div>
          <div class="form-row">
            <label>Target Folder</label>
            <input v-model="editTargetFolder" class="glass-input" style="flex:1" />
          </div>
        </div>

        <div class="glass-panel config-section">
          <div class="card-header text-warning">ENCODING</div>
          <div class="form-row">
            <label>x264 Preset</label>
            <select v-model="editPreset">
              <option v-for="p in PRESETS" :key="p" :value="p">{{ p }}</option>
            </select>
            <label style="margin-left:16px">Tune</label>
            <select v-model="editTune">
              <option v-for="t in TUNES" :key="t" :value="t">{{ t }}</option>
            </select>
            <label style="margin-left:16px">Audio</label>
            <select v-model="editAudioCodec">
              <option v-for="a in AUDIO_CODECS" :key="a" :value="a">{{ a }}</option>
            </select>
          </div>
          <div class="form-row">
            <label>Profile A CRF</label>
            <input type="range" min="0" max="51" v-model.number="editCrfA" />
            <span class="mono">{{ editCrfA }}</span>
            <label style="margin-left:16px">Profile B CRF</label>
            <input type="range" min="0" max="51" v-model.number="editCrfB" />
            <span class="mono">{{ editCrfB }}</span>
            <label style="margin-left:16px">Profile C CRF</label>
            <input type="range" min="0" max="51" v-model.number="editCrfC" />
            <span class="mono">{{ editCrfC }}</span>
          </div>
          <div class="form-row">
            <label>CPU threads</label>
            <input type="number" min="0" max="128" v-model.number="editThreads" class="glass-input" style="width:70px" />
            <span class="text-muted" style="font-size:11px">0 = auto (all cores)</span>
          </div>
          <div class="form-row">
            <label>HD Maxrate</label>
            <input v-model="editMaxrateA" class="glass-input" style="width:80px" />
            <label style="margin-left:16px">SD Maxrate</label>
            <input v-model="editMaxrateC" class="glass-input" style="width:80px" />
          </div>
        </div>

        <div class="glass-panel config-section">
          <div class="card-header text-muted">SERVICE</div>
          <div class="form-row">
            <label>Max concurrent</label>
            <input type="number" min="1" max="16" v-model.number="editConcurrency" class="glass-input" style="width:70px" />
            <label style="margin-left:16px">Poll interval (s)</label>
            <input v-model="editPollSecs" class="glass-input" style="width:70px" />
            <label style="margin-left:16px">Settle time (s)</label>
            <input v-model="editSettleSecs" class="glass-input" style="width:70px" />
          </div>
          <div class="form-row">
            <label>Stable polls</label>
            <input type="number" min="1" max="20" v-model.number="editStablePolls" class="glass-input" style="width:70px" />
            <label style="margin-left:16px">Retry policy</label>
            <select v-model="editRetryPolicy">
              <option v-for="r in RETRY_POLICIES" :key="r" :value="r">{{ r }}</option>
            </select>
          </div>
        </div>

        <button class="btn btn-primary" style="margin-top:16px;padding:10px 32px;font-size:14px" @click="saveConfig">Save Configuration</button>
      </div>

      <div v-if="activeTab === 'logs'" class="tab-panel">
        <div class="glass-panel" style="padding:12px;display:flex;flex-direction:column;height:calc(100vh - 220px)">
          <div style="display:flex;gap:8px;margin-bottom:8px">
            <button class="btn" style="font-size:12px" @click="clearLogs">Clear</button>
            <span class="text-muted" style="font-size:12px;line-height:28px">{{ logs.length }} entries</span>
          </div>
          <div class="log-viewer" ref="logViewerRef">
            <div v-if="!logs.length" class="text-muted" style="padding:20px;text-align:center">No log entries</div>
            <div v-for="(line, i) in logs" :key="i" class="log-line" :class="logLevel(line)">
              {{ line }}
            </div>
          </div>
        </div>
      </div>
    </main>
  </div>
</template>

<script setup lang="ts">
import { ref, reactive, onMounted, onUnmounted, nextTick, watch } from 'vue'

const PRESETS = ['ultrafast', 'veryfast', 'faster', 'fast', 'medium', 'slow', 'slower', 'veryslow']
const AUDIO_CODECS = ['aac', 'pcm_s16le']
const TUNES = ['film', 'grain', 'animation', 'none']
const RETRY_POLICIES = ['never', 'once', 'always']

const tabs = [
  { id: 'dashboard', label: 'Dashboard' },
  { id: 'config', label: 'Configuration' },
  { id: 'logs', label: 'Logs' },
]
const activeTab = ref('dashboard')

const serviceRunning = ref(false)
const downloading = ref(false)
const logs = ref<string[]>([])

const toolchain = reactive({ ffmpeg_found: false, ffprobe_found: false, ffmpeg_version: null as string | null, ffprobe_version: null as string | null })
const stats = reactive({ pending: 0, active: 0, completed: 0, failed: 0, total: 0 })
const config = reactive<any>({ paths: {}, encoding: {}, profiles: { a: {}, b: {}, c: {} }, ingestion: {} })
const activeJobs = ref<any[]>([])
const recentJobs = ref<any[]>([])

const editWatchFolder = ref('')
const editTargetFolder = ref('')
const editPreset = ref('medium')
const editTune = ref('film')
const editAudioCodec = ref('aac')
const editCrfA = ref(24)
const editCrfB = ref(23)
const editCrfC = ref(20)
const editMaxrateA = ref('15M')
const editMaxrateC = ref('5M')
const editConcurrency = ref(2)
const editPollSecs = ref('10')
const editSettleSecs = ref('5')
const editStablePolls = ref(2)
const editRetryPolicy = ref('once')
const editThreads = ref(0)

const logViewerRef = ref<HTMLElement | null>(null)
let sseConnection: EventSource | null = null
let pollInterval: number = 0

function shortFileName(path: string) { return path?.split('\\').pop()?.split('/').pop() || path }
function toolShortVer(v: string) { return v ? v.split(' ').pop() || v : '' }

function logLevel(line: string): string {
  if (line.includes('[ERROR]') || line.includes('error:')) return 'error'
  if (line.includes('[WARN]')) return 'warn'
  if (line.includes('Completed')) return 'success'
  return 'info'
}

async function apiGet(path: string) {
  try { const r = await fetch('/api' + path); return r.ok ? await r.json() : null }
  catch { return null }
}

async function apiPost(path: string) {
  try { const r = await fetch('/api' + path, { method: 'POST' }); return r.ok ? await r.json() : null }
  catch { return null }
}

async function fetchAll() {
  const [h, t, s, j, l] = await Promise.all([
    apiGet('/health'),
    apiGet('/toolchain'),
    apiGet('/stats'),
    apiGet('/jobs'),
    apiGet('/logs'),
  ])
  if (h) serviceRunning.value = h.service_running
  if (t) Object.assign(toolchain, t)
  if (s) Object.assign(stats, s)
  if (j) {
    activeJobs.value = j.filter((x: any) => x.state === 'Processing')
    recentJobs.value = j.filter((x: any) => x.state === 'Completed' || x.state === 'Failed').slice(0, 20)
  }
  if (l) logs.value = l
}

async function fetchConfig() {
  const c = await apiGet('/config')
  if (!c) return
  Object.assign(config, c)
  editWatchFolder.value = c.paths?.watch_folder || ''
  editTargetFolder.value = c.paths?.target_folder || ''
  editPreset.value = c.encoding?.preset || 'medium'
  editTune.value = c.encoding?.tune || 'film'
  editAudioCodec.value = c.encoding?.audio_codec || 'aac'
  editCrfA.value = c.profiles?.a?.crf || 24
  editCrfB.value = c.profiles?.b?.crf || 23
  editCrfC.value = c.profiles?.c?.crf || 20
  editMaxrateA.value = c.profiles?.a?.maxrate || '15M'
  editMaxrateC.value = c.profiles?.c?.maxrate || '5M'
  editConcurrency.value = c.ingestion?.max_concurrency || 2
  editPollSecs.value = String(c.ingestion?.poll_secs || 10)
  editSettleSecs.value = String(c.ingestion?.settle_secs || 5)
  editStablePolls.value = c.ingestion?.stable_polls_min || 2
  editRetryPolicy.value = c.ingestion?.retry_policy || 'once'
  editThreads.value = c.encoding?.ffmpeg_threads || 0
}

async function startService() {
  const r = await apiPost('/service/start')
  if (r?.success) { serviceRunning.value = true; await fetchAll() }
  else alert(r?.error || 'Failed to start service')
}

async function stopService() {
  await apiPost('/service/stop')
  serviceRunning.value = false
}

async function downloadFFmpeg() {
  const r = await apiPost('/download/start')
  if (r?.success) downloading.value = true
}

async function installService() {
  const r = await apiPost('/service/install')
  alert(r?.message || r?.error || 'Done')
}

async function uninstallService() {
  const r = await apiPost('/service/uninstall')
  alert(r?.message || r?.error || 'Done')
}

async function saveConfig() {
  // Not implemented server-side yet — just show what would be saved
  alert('Configuration save is not yet implemented via the API. Use the wizard or edit config.toml directly.')
}

function clearLogs() { logs.value = [] }

watch(activeTab, () => {
  if (activeTab.value === 'config') fetchConfig()
})

watch(logs, async () => {
  await nextTick()
  if (logViewerRef.value) logViewerRef.value.scrollTop = logViewerRef.value.scrollHeight
}, { deep: true })

onMounted(() => {
  fetchAll()
  fetchConfig()
  pollInterval = window.setInterval(fetchAll, 2000)
  pollInterval = window.setInterval(async () => {
    const ds = await apiGet('/download/status')
    downloading.value = ds?.status === 'downloading'
    if (ds?.status === 'ok' || ds?.status?.startsWith('error:')) {
      await fetchAll()
    }
  }, 1000)

  sseConnection = new EventSource('/api/events')
  sseConnection.addEventListener('job_update', () => fetchAll())
  sseConnection.addEventListener('connected', () => fetchAll())
  sseConnection.onerror = () => { setTimeout(() => { sseConnection = new EventSource('/api/events') }, 5000) }
})

onUnmounted(() => {
  clearInterval(pollInterval)
  sseConnection?.close()
})
</script>

<style scoped>
.app-shell { min-height: 100vh; display: flex; flex-direction: column; }

.header {
  background: var(--bg-secondary);
  border-bottom: 1px solid var(--glass-border);
  padding: 8px 20px;
  display: flex;
  justify-content: space-between;
  align-items: center;
  gap: 12px;
  flex-shrink: 0;
}
.header-left { display: flex; align-items: center; gap: 12px; flex-wrap: wrap; }
.header-right { display: flex; align-items: center; gap: 8px; }
.app-title { font-size: 17px; font-weight: 700; color: var(--accent-blue); white-space: nowrap; }
.header-divider { width: 1px; height: 24px; background: var(--glass-border); }
.tool-card { display: flex; align-items: center; gap: 8px; background: var(--glass-bg); border: 1px solid var(--glass-border); border-radius: 8px; padding: 4px 10px; }
.tool-row { display: flex; align-items: center; gap: 4px; font-size: 12px; }
.tool-name { font-weight: 600; }
.tool-ver { font-size: 10px; }
.download-indicator { display: flex; align-items: center; gap: 6px; }
.spinner { width: 14px; height: 14px; border: 2px solid var(--glass-border); border-top-color: var(--accent-yellow); border-radius: 50%; animation: spin 0.8s linear infinite; }
@keyframes spin { to { transform: rotate(360deg); } }
.service-ctrl { display: flex; align-items: center; gap: 6px; }

.tab-bar { display: flex; gap: 2px; padding: 6px 20px; background: var(--bg-primary); border-bottom: 1px solid var(--glass-border); flex-shrink: 0; }
.tab-btn { border: 1px solid transparent; background: none; color: var(--text-secondary); padding: 6px 18px; border-radius: 6px; cursor: pointer; font-size: 13px; font-weight: 500; transition: all 0.15s; }
.tab-btn:hover { color: var(--text-primary); background: var(--glass-bg); }
.tab-btn.active { color: #000; background: var(--accent-blue); border-color: var(--accent-blue); font-weight: 700; }

.main-content { flex: 1; overflow-y: auto; padding: 20px; }
.tab-panel { max-width: 1440px; margin: 0 auto; }

.bento-row { display: flex; gap: 12px; flex-wrap: wrap; }
.bento-card { flex: 1; min-width: 200px; background: var(--glass-bg); border: 1px solid var(--glass-border); border-radius: var(--border-radius-base); padding: 14px; display: flex; flex-direction: column; }
.card-header { font-size: 11px; font-weight: 800; letter-spacing: 0.05em; margin-bottom: 10px; text-transform: uppercase; }
.card-body { flex: 1; }

.tool-status-line { display: flex; align-items: center; gap: 4px; font-size: 12px; margin-bottom: 2px; }

.stats-grid { display: grid; grid-template-columns: 1fr 1fr; gap: 4px 16px; }
.stat { display: flex; align-items: baseline; gap: 4px; }
.stat-num { font-size: 16px; font-weight: 700; }
.stat-label { font-size: 11px; color: var(--text-secondary); }

.path-row { margin-bottom: 4px; overflow: hidden; text-overflow: ellipsis; }

.job-row { padding: 6px 0; border-bottom: 1px solid rgba(255,255,255,0.04); }
.job-row:last-child { border-bottom: none; }
.job-main { display: flex; align-items: center; gap: 8px; flex-wrap: wrap; }
.recent-job { display: flex; align-items: center; gap: 6px; padding: 3px 0; }

.empty-state { text-align: center; padding: 60px 20px; }

.config-section { padding: 16px; margin-bottom: 10px; }
.form-row { display: flex; align-items: center; gap: 8px; margin-bottom: 8px; flex-wrap: wrap; }
.form-row label { font-size: 13px; color: var(--text-primary); white-space: nowrap; min-width: 80px; }

.log-viewer { flex: 1; overflow-y: auto; font-family: 'Cascadia Code', 'Consolas', monospace; font-size: 11px; line-height: 1.6; }
.log-line { padding: 2px 4px; border-bottom: 1px solid rgba(255,255,255,0.03); color: var(--text-secondary); }
.log-line.error { color: var(--accent-red); }
.log-line.warn { color: var(--accent-yellow); }
.log-line.success { color: var(--accent-green); }
</style>

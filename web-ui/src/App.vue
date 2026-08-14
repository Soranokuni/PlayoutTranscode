<template>
  <div class="app-shell">
    <BroadcastTopBar
      :watch="watchfolder"
      :tool="toolchain"
      :running="serviceRunning"
      :dl="downloading"
      :uptime="uptimeMs"
      @start="onStart"
      @stop="stopService"
      @download="downloadFFmpeg"
      @install="onInstall"
      @uninstall="onUninstall"
    />

    <nav class="tab-bar">
      <button v-for="t in tabs" :key="t.id" :class="['tab-btn', { active: activeTab === t.id }]" @click="activeTab = t.id">
        {{ t.label }}
      </button>
    </nav>

    <main class="main-content">
      <div v-if="configStatus === 'loading'" class="loading-splash">
        <div class="spinner"></div>
        <span>Loading configuration...</span>
      </div>

      <div v-else-if="showWizard" class="tab-panel">
        <div class="wizard-card">
          <div class="wizard-header">
            <h2>Welcome to PlayoutTranscode</h2>
            <p>This appears to be your first run. Let's set up your media processing pipeline.</p>
          </div>

          <div class="panel config-section">
            <div class="panel-header"><span class="panel-title text-accent">MEDIA PATHS</span></div>
            <p class="hint">Where source files arrive, and where finished mezzanine files go.</p>
            <div class="form-row">
              <label>Watch Folder</label>
              <input v-model="editWatchFolder" class="input" style="flex:1" placeholder="e.g. D:\media\incoming" />
            </div>
            <div class="form-row">
              <label>Target Folder</label>
              <input v-model="editTargetFolder" class="input" style="flex:1" placeholder="e.g. D:\media\mezzanine" />
            </div>
          </div>

          <div class="panel config-section">
            <div class="panel-header"><span class="panel-title text-warning">QUALITY</span></div>
            <p class="hint">Higher CRF = smaller files, lower quality. Range 0-51. Defaults are broadcast-grade.</p>
            <div class="form-row">
              <label>HD Progressive CRF</label>
              <input type="range" min="18" max="51" v-model.number="editCrfA" />
              <span class="mono">{{ editCrfA }}</span>
              <label style="margin-left:16px">HD Interlaced CRF</label>
              <input type="range" min="18" max="51" v-model.number="editCrfB" />
              <span class="mono">{{ editCrfB }}</span>
              <label style="margin-left:16px">SD PAL CRF</label>
              <input type="range" min="18" max="51" v-model.number="editCrfC" />
              <span class="mono">{{ editCrfC }}</span>
            </div>
          </div>

          <div class="panel config-section" style="background:var(--bg-primary);border:1px solid var(--accent-cyan)">
            <div class="panel-header"><span class="panel-title" style="color:var(--accent-cyan)">READY?</span></div>
            <p class="hint" style="margin-bottom:12px">Save your configuration to begin. You can change all settings later in the Configuration tab.</p>
            <div style="display:flex;gap:12px;align-items:center">
              <button class="btn btn-primary" style="padding:12px 32px;font-size:15px;font-weight:700" @click="saveConfigAndStart">
                Configure &amp; Start
              </button>
              <span v-if="saveMsg" :class="['save-msg', saveOk ? 'save-ok' : 'save-err']">{{ saveMsg }}</span>
            </div>
          </div>
        </div>
      </div>

      <template v-else>
        <div v-if="activeTab === 'dashboard'" class="tab-panel">
          <IngestQueuePanel
            ref="ingestPanelRef"
            :jobs="jobs"
            @retry="onRetryJob"
            @cancel="onCancelJob"
            @retry-all="onRetryAll"
          />
          <AssetRegistryGrid :assets="assets" />

          <div v-if="!serviceRunning && !stats.total" class="empty-state">
            <div style="font-size:18px;color:var(--text-secondary);margin-bottom:12px">Service is stopped</div>
            <button class="btn btn-play" @click="onStart">&#9654;  Start Service</button>
          </div>
        </div>

        <div v-if="activeTab === 'config'" class="tab-panel">
          <div class="panel config-section">
            <div class="panel-header"><span class="panel-title text-accent">FILE PATHS</span></div>
            <div class="form-row">
              <label>Watch Folder</label>
              <input v-model="editWatchFolder" class="input" style="flex:1" />
            </div>
            <div class="form-row">
              <label>Target Folder</label>
              <input v-model="editTargetFolder" class="input" style="flex:1" />
            </div>
          </div>

          <div class="panel config-section">
            <div class="panel-header"><span class="panel-title text-warning">ENCODING</span></div>
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
              <label>Audio Bitrate</label>
              <input v-model="editAudioBitrate" class="input" style="width:80px" />
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
              <label>CPU cores budget</label>
              <input type="number" min="0" max="128" v-model.number="editCpuCores" class="input" style="width:70px" />
              <span class="text-muted" style="font-size:11px">0 = auto (half of available cores). Split across concurrent encodes.</span>
            </div>
            <div class="form-row">
              <label>Threads per encode</label>
              <input type="number" min="0" max="128" v-model.number="editThreads" class="input" style="width:70px" />
              <span class="text-muted" style="font-size:11px">0 = auto (cores ÷ max_concurrency). Non-zero overrides.</span>
            </div>
            <div class="form-row" v-if="effectiveThreadsDisplay" style="font-size:11px;color:var(--accent-cyan)">
              <span class="mono">{{ effectiveThreadsDisplay }}</span>
              <span v-if="oversubscribed" style="color:var(--accent-amber);margin-left:12px">⚠ oversubscribed vs {{ availableCores }} logical cores</span>
            </div>
            <div class="form-row">
              <label>HD Maxrate</label>
              <input v-model="editMaxrateAB" class="input" style="width:80px" />
              <label style="margin-left:16px">HD Bufsize</label>
              <input v-model="editBufsizeAB" class="input" style="width:80px" />
              <label style="margin-left:16px">SD Maxrate</label>
              <input v-model="editMaxrateC" class="input" style="width:80px" />
              <label style="margin-left:16px">SD Bufsize</label>
              <input v-model="editBufsizeC" class="input" style="width:80px" />
            </div>
          </div>

          <div class="panel config-section">
            <div class="panel-header"><span class="panel-title" style="color:var(--accent-emerald)">AUDIO NORMALIZATION &amp; QC</span></div>
            <div class="form-row">
              <label>Loudness Mode</label>
              <select v-model="editAudioMode">
                <option value="legacy_v1_encode">Legacy (Preserve / Pass-through)</option>
                <option value="ebu_r128">EBU R128 (-23 LUFS / -1 dBTP / 7 LRA)</option>
                <option value="atsc_a85">ATSC A/85 (-24 LUFS / -2 dBTP / 7 LRA)</option>
                <option value="passthrough_validate">Passthrough &amp; Validate Only</option>
                <option value="analyze_only">Analyze &amp; Report Only</option>
              </select>
            </div>
            <div class="form-row" v-if="editAudioMode === 'ebu_r128' || editAudioMode === 'atsc_a85'">
              <label>Target LUFS</label>
              <input type="number" step="0.5" v-model.number="editAudioTargetLufs" class="input" style="width:80px" :placeholder="editAudioMode === 'ebu_r128' ? '-23.0' : '-24.0'" />
              <label style="margin-left:16px">True Peak (dBTP)</label>
              <input type="number" step="0.5" v-model.number="editAudioTruePeak" class="input" style="width:80px" :placeholder="editAudioMode === 'ebu_r128' ? '-1.0' : '-2.0'" />
              <label style="margin-left:16px">LRA Target</label>
              <input type="number" step="0.5" v-model.number="editAudioLra" class="input" style="width:80px" placeholder="7.0" />
            </div>
            <div class="form-row">
              <label class="checkbox-label">
                <input type="checkbox" v-model="editAudioDualMono" />
                Mono to Dual-Mono Channel Expansion (Stereo Track)
              </label>
            </div>
          </div>

          <div class="panel config-section">
            <div class="panel-header"><span class="panel-title" style="color:var(--text-secondary)">SERVICE</span></div>
            <div class="form-row">
              <label>Max concurrent</label>
              <input type="number" min="1" max="16" v-model.number="editConcurrency" class="input" style="width:70px" />
              <label style="margin-left:16px">Poll interval (s)</label>
              <input v-model="editPollSecs" class="input" style="width:70px" />
              <label style="margin-left:16px">Settle time (s)</label>
              <input v-model="editSettleSecs" class="input" style="width:70px" />
            </div>
            <div class="form-row">
              <label>Stable polls</label>
              <input type="number" min="1" max="20" v-model.number="editStablePolls" class="input" style="width:70px" />
              <label style="margin-left:16px">Retry policy</label>
              <select v-model="editRetryPolicy">
                <option v-for="r in RETRY_POLICIES" :key="r" :value="r">{{ r }}</option>
              </select>
            </div>
            <div class="form-row">
              <label class="checkbox-label">
                <input type="checkbox" v-model="editAutoRetryOnStart" />
                Auto-purge &amp; retry failed jobs on startup
              </label>
              <span class="text-muted" style="font-size:11px">Purges error rows whose source is still in the watch folder; the watcher re-queues them.</span>
            </div>
            <div class="form-row">
              <label>Max attempts</label>
              <input type="number" min="1" max="10" v-model.number="editMaxAttempts" class="input" style="width:70px" />
              <label style="margin-left:16px">Retry delay (ms)</label>
              <input type="number" min="0" max="60000" v-model.number="editRetryDelayMs" class="input" style="width:90px" />
            </div>
          </div>

          <div style="display:flex;gap:12px;align-items:center;margin-top:16px">
            <button class="btn btn-primary" style="padding:10px 32px;font-size:14px" @click="saveConfig">Save Configuration</button>
            <span v-if="saveMsg" :class="['save-msg', saveOk ? 'save-ok' : 'save-err']">{{ saveMsg }}</span>
          </div>
        </div>

        <div v-if="activeTab === 'logs'" class="tab-panel">
          <div class="panel" style="padding:12px;display:flex;flex-direction:column;height:calc(100vh - 220px)">
            <div style="display:flex;gap:8px;margin-bottom:8px">
              <button class="btn" style="font-size:12px" @click="clearLogs">Clear</button>
              <span class="text-muted" style="font-size:12px;line-height:28px">{{ logs.length }} entries</span>
            </div>
            <div class="log-viewer" ref="logViewerRef">
              <div v-if="!logs.length" class="text-muted" style="padding:20px;text-align:center">No log entries</div>
              <div v-for="(line, i) in logs" :key="i" class="log-line" :class="logLevel(line)">{{ line }}</div>
            </div>
          </div>
        </div>
      </template>
    </main>
  </div>
</template>

<script setup lang="ts">
import { ref, watch, nextTick, onMounted, computed } from 'vue'
import { useEventStream, type ConfigPayload } from './composables/useEventStream'
import BroadcastTopBar from './components/BroadcastTopBar.vue'
import IngestQueuePanel from './components/IngestQueuePanel.vue'
import AssetRegistryGrid from './components/AssetRegistryGrid.vue'

const PRESETS = ['ultrafast', 'veryfast', 'faster', 'fast', 'medium', 'slow', 'slower', 'veryslow']
const AUDIO_CODECS = ['aac', 'pcm_s16le', 'libmp3lame']
const TUNES = ['film', 'grain', 'animation', 'none']
const RETRY_POLICIES = ['never', 'once', 'always']

const tabs = [
  { id: 'dashboard', label: 'Dashboard' },
  { id: 'config', label: 'Configuration' },
  { id: 'logs', label: 'Logs' },
]
const activeTab = ref('dashboard')

const {
  jobs, assets, watchfolder, stats, config, toolchain,
  serviceRunning, downloading, logs, uptimeMs,
  fetchConfig, putConfig, startService, stopService, downloadFFmpeg,
  installService, uninstallService, clearLogs, retryJob, cancelJob, retryAllFailed,
} = useEventStream()

const configStatus = ref<'loading' | 'ready'>('loading')
const showWizard = ref(false)
const saveMsg = ref('')
const saveOk = ref(false)

const editWatchFolder = ref('')
const editTargetFolder = ref('')
const editPreset = ref('medium')
const editTune = ref('film')
const editAudioCodec = ref('aac')
const editAudioBitrate = ref('320k')
const editCrfA = ref(24)
const editCrfB = ref(23)
const editCrfC = ref(20)
const editMaxrateAB = ref('15M')
const editBufsizeAB = ref('16M')
const editMaxrateC = ref('5M')
const editBufsizeC = ref('6M')
const editConcurrency = ref(2)
const editPollSecs = ref('10')
const editSettleSecs = ref('5')
const editStablePolls = ref(2)
const editRetryPolicy = ref('once')
const editThreads = ref(0)
const editCpuCores = ref(0)
const editAutoRetryOnStart = ref(true)
const editMaxAttempts = ref(2)
const editRetryDelayMs = ref(2000)

const editAudioMode = ref<'legacy_v1_encode' | 'ebu_r128' | 'atsc_a85' | 'passthrough_validate' | 'analyze_only'>('legacy_v1_encode')
const editAudioTargetLufs = ref<number | undefined>(undefined)
const editAudioTruePeak = ref<number | undefined>(undefined)
const editAudioLra = ref<number | undefined>(undefined)
const editAudioDualMono = ref(false)

const logViewerRef = ref<HTMLElement | null>(null)
const ingestPanelRef = ref<InstanceType<typeof IngestQueuePanel> | null>(null)

const availableCores = ref(0)
const effectiveThreadsDisplay = ref('')
const oversubscribed = ref(false)

function recomputeThreads() {
  const cores = availableCores.value
  const conc = editConcurrency.value || 1
  let perEncode: number
  if (editThreads.value > 0) {
    perEncode = editThreads.value
  } else {
    const budget = editCpuCores.value > 0 ? editCpuCores.value : Math.max(1, Math.floor((cores || 4) / 2))
    perEncode = Math.max(1, Math.floor(budget / conc))
  }
  const total = perEncode * conc
  const coresLabel = editCpuCores.value > 0 ? `${editCpuCores.value} cores` : (cores ? `auto (${Math.max(1, Math.floor(cores / 2))} cores)` : 'auto')
  effectiveThreadsDisplay.value =
    `${perEncode} threads/encode × ${conc} concurrent = ${total} total (budget: ${coresLabel})`
  oversubscribed.value = cores > 0 && total > cores
}

async function onRetryJob(id: string) {
  const r = await retryJob(id)
  const ok = !!r?.success
  const msg = ok ? 'Retrying job…' : (r?.error || 'Retry failed')
  ingestPanelRef.value?.showRetryMsg(msg, ok)
  if (ok) { /* SSE will refresh state via fetchAll */ }
}

async function onCancelJob(id: string) {
  const r = await cancelJob(id)
  const ok = !!r?.success
  const msg = ok ? 'Cancelling job…' : (r?.error || 'Cancel failed')
  ingestPanelRef.value?.showRetryMsg(msg, ok)
}

async function onRetryAll() {
  const r = await retryAllFailed()
  const submitted = r?.submitted ?? 0
  const missing = r?.source_missing ?? 0
  const errors = r?.errors ?? 0
  const ok = submitted > 0 && errors === 0
  const msg = submitted > 0
    ? `Re-queued ${submitted} job${submitted === 1 ? '' : 's'}${missing ? `, ${missing} source missing` : ''}`
    : (errors ? 'Retry failed' : (missing ? `${missing} source missing` : 'Nothing to retry'))
  ingestPanelRef.value?.showRetryMsg(msg, ok)
}

function logLevel(line: string): string {
  if (line.includes('[ERROR]') || line.includes('error:')) return 'error'
  if (line.includes('[WARN]')) return 'warn'
  if (line.includes('Completed')) return 'success'
  return 'info'
}

function populateFromConfig(cfg: ConfigPayload) {
  editWatchFolder.value = cfg.paths.watch_folder
  editTargetFolder.value = cfg.paths.target_folder
  editPreset.value = cfg.encoding.preset
  editTune.value = cfg.encoding.tune || 'film'
  editAudioCodec.value = cfg.encoding.audio_codec
  editAudioBitrate.value = cfg.encoding.audio_bitrate || '320k'
  editCrfA.value = cfg.profiles.a.crf
  editCrfB.value = cfg.profiles.b.crf
  editCrfC.value = cfg.profiles.c.crf
  editMaxrateAB.value = cfg.profiles.a.maxrate || '15M'
  editBufsizeAB.value = cfg.profiles.a.bufsize || '16M'
  editMaxrateC.value = cfg.profiles.c.maxrate || '5M'
  editBufsizeC.value = cfg.profiles.c.bufsize || '6M'
  editConcurrency.value = cfg.ingestion.max_concurrency
  editPollSecs.value = String(cfg.ingestion.poll_secs)
  editSettleSecs.value = String(cfg.ingestion.settle_secs)
  editStablePolls.value = cfg.ingestion.stable_polls_min
  editRetryPolicy.value = cfg.ingestion.retry_policy
  editThreads.value = cfg.encoding.ffmpeg_threads
  editCpuCores.value = cfg.encoding.cpu_cores ?? 0
  editAutoRetryOnStart.value = cfg.ingestion.auto_retry_on_start ?? true
  editMaxAttempts.value = cfg.ingestion.max_attempts ?? 2
  editRetryDelayMs.value = cfg.ingestion.retry_delay_ms ?? 2000
  if (cfg.audio_policy) {
    editAudioMode.value = cfg.audio_policy.mode || 'legacy_v1_encode'
    editAudioTargetLufs.value = cfg.audio_policy.target_lufs
    editAudioTruePeak.value = cfg.audio_policy.true_peak_dbtp
    editAudioLra.value = cfg.audio_policy.lra_target
    editAudioDualMono.value = !!cfg.audio_policy.dual_mono
  }
  availableCores.value = cfg.system?.available_logical_cores ?? 0
  recomputeThreads()
}

async function loadAndDecideWizard() {
  const cfg = await fetchConfig()
  if (cfg) {
    populateFromConfig(cfg)
    showWizard.value = !cfg.initialized
  } else {
    showWizard.value = true
  }
  configStatus.value = 'ready'
}

async function saveConfig() {
  saveMsg.value = ''
  saveOk.value = false
  try {
    await putConfig({
      paths: { watch_folder: editWatchFolder.value, target_folder: editTargetFolder.value },
      encoding: {
        preset: editPreset.value,
        ffmpeg_threads: editThreads.value,
        cpu_cores: editCpuCores.value,
        audio_codec: editAudioCodec.value,
        audio_bitrate: editAudioBitrate.value,
        tune: editTune.value,
      },
      audio_policy: {
        mode: editAudioMode.value,
        codec: editAudioCodec.value,
        bitrate: editAudioBitrate.value,
        sample_rate_hz: 48000,
        channels: 2,
        target_lufs: editAudioTargetLufs.value,
        true_peak_dbtp: editAudioTruePeak.value,
        lra_target: editAudioLra.value,
        dual_mono: editAudioDualMono.value,
        preserve_original: false,
      },
      profile_a: { enabled: true, crf: editCrfA.value, maxrate: editMaxrateAB.value, bufsize: editBufsizeAB.value },
      profile_b: { enabled: true, crf: editCrfB.value, maxrate: editMaxrateAB.value, bufsize: editBufsizeAB.value },
      profile_c: { enabled: true, crf: editCrfC.value, maxrate: editMaxrateC.value, bufsize: editBufsizeC.value },
      ingestion: {
        settle_secs: Number(editSettleSecs.value) || 5,
        poll_secs: Number(editPollSecs.value) || 10,
        max_concurrency: editConcurrency.value,
        stable_polls_min: editStablePolls.value,
        retry_policy: editRetryPolicy.value,
        auto_retry_on_start: editAutoRetryOnStart.value,
        max_attempts: editMaxAttempts.value,
        retry_delay_ms: editRetryDelayMs.value,
      },
    } as unknown as Partial<ConfigPayload>)
    saveMsg.value = 'Configuration saved successfully'
    saveOk.value = true
    showWizard.value = false
    setTimeout(() => { saveMsg.value = '' }, 4000)
  } catch (e: unknown) {
    const msg = e instanceof Error ? e.message : String(e)
    saveMsg.value = `Failed to save: ${msg}`
    saveOk.value = false
  }
}

async function saveConfigAndStart() {
  await saveConfig()
  if (saveOk.value) {
    await onStart()
  }
}

async function onStart() {
  const r = await startService()
  if (r && !r.success) alert(r.error || 'Failed to start service')
}

async function onInstall() {
  const r = await installService()
  alert(r?.message || r?.error || 'Done')
}

async function onUninstall() {
  const r = await uninstallService()
  alert(r?.message || r?.error || 'Done')
}

onMounted(loadAndDecideWizard)

watch(activeTab, () => {
  if (activeTab.value === 'config') fetchConfig()
})

watch([editThreads, editCpuCores, editConcurrency], recomputeThreads)

watch(config, (cfg) => {
  if (cfg) populateFromConfig(cfg)
})

watch(logs, async (newLogs) => {
  await nextTick()
  if (newLogs.length && logViewerRef.value) {
    logViewerRef.value.scrollTop = logViewerRef.value.scrollHeight
  }
})
</script>

<style scoped>
.app-shell {
  min-height: 100vh;
  display: flex;
  flex-direction: column;
}

.tab-bar {
  display: flex;
  gap: 2px;
  padding: 6px 20px;
  background: var(--bg-primary);
  border-bottom: 1px solid var(--border-subtle);
  flex-shrink: 0;
}

.tab-btn {
  border: 1px solid transparent;
  background: none;
  color: var(--text-secondary);
  padding: 6px 18px;
  border-radius: 6px;
  cursor: pointer;
  font-size: 13px;
  font-weight: 500;
  transition: all 0.15s;
}

.tab-btn:hover {
  color: var(--text-primary);
  background: var(--bg-surface);
}

.tab-btn.active {
  color: #000;
  background: var(--accent-cyan);
  border-color: var(--accent-cyan);
  font-weight: 700;
}

.main-content {
  flex: 1;
  overflow-y: auto;
  padding: 20px;
}

.tab-panel {
  max-width: 1440px;
  margin: 0 auto;
}

.loading-splash {
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  gap: 16px;
  padding: 80px 20px;
  color: var(--text-secondary);
}

.spinner {
  width: 32px;
  height: 32px;
  border: 3px solid var(--border-subtle);
  border-top-color: var(--accent-cyan);
  border-radius: 50%;
  animation: spin 0.8s linear infinite;
}

@keyframes spin {
  to { transform: rotate(360deg); }
}

.wizard-card {
  max-width: 700px;
  margin: 0 auto;
}

.wizard-header {
  text-align: center;
  padding: 32px 20px 20px;
}

.wizard-header h2 {
  font-size: 24px;
  font-weight: 700;
  margin-bottom: 8px;
  color: var(--text-primary);
}

.wizard-header p {
  font-size: 14px;
  color: var(--text-secondary);
}

.empty-state {
  text-align: center;
  padding: 60px 20px;
}

.config-section {
  padding: 16px;
  margin-bottom: 10px;
}

.form-row {
  display: flex;
  align-items: center;
  gap: 8px;
  margin-bottom: 8px;
  flex-wrap: wrap;
}

.form-row label {
  font-size: 13px;
  color: var(--text-primary);
  white-space: nowrap;
  min-width: 80px;
}

.checkbox-label {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  cursor: pointer;
  min-width: auto !important;
}
.checkbox-label input[type="checkbox"] {
  cursor: pointer;
}

.hint {
  font-size: 12px;
  color: var(--text-secondary);
  margin-bottom: 10px;
}

.save-msg {
  font-size: 13px;
  font-weight: 500;
  padding: 6px 12px;
  border-radius: 4px;
  transition: opacity 0.3s;
}

.save-ok {
  color: var(--accent-emerald);
  background: rgba(0,200,100,0.1);
}

.save-err {
  color: var(--accent-crimson);
  background: rgba(220,50,50,0.1);
}

.log-viewer {
  flex: 1;
  overflow-y: auto;
  font-family: 'Cascadia Code', 'Consolas', monospace;
  font-size: 11px;
  line-height: 1.6;
}

.log-line {
  padding: 2px 4px;
  border-bottom: 1px solid rgba(255,255,255,0.03);
  color: var(--text-secondary);
}

.log-line.error { color: var(--accent-crimson); }
.log-line.warn { color: var(--accent-amber); }
.log-line.success { color: var(--accent-emerald); }
</style>

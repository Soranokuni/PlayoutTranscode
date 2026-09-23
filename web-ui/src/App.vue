<template>
  <div class="app-shell">
    <BroadcastTopBar
      :watch="watchfolder"
      :tool="toolchain"
      :running="serviceRunning"
      :service-state="serviceStatus?.state"
      :link="linkState"
      :dl="downloading"
      :uptime="uptimeMs"
      :busy="serviceBusy"
      @start="onStart"
      @stop="askStop"
      @download="onDownload"
    />

    <!-- F-23 was fixed server-side: after a save the running loop keeps its old
         clone and /api/service/status reports restart_required. Nothing in the
         UI ever said so, so an operator who lowered max_concurrency saw
         "saved successfully" and nothing happened (UX-01). -->
    <div v-if="serviceStatus?.restart_required" class="restart-banner" role="status">
      <span class="restart-glyph" aria-hidden="true">⚠</span>
      <span>
        Configuration saved, but the running ingest loop is still using the
        previous values. Restart processing to apply them.
      </span>
      <button class="btn restart-btn" :disabled="serviceBusy" @click="askRestart">
        Restart processing
      </button>
    </div>

    <nav class="tab-bar" role="tablist" aria-label="Sections" @keydown="onTabKeydown">
      <button
        v-for="t in tabs"
        :id="`tab-${t.id}`"
        :key="t.id"
        role="tab"
        :aria-selected="activeTab === t.id"
        :tabindex="activeTab === t.id ? 0 : -1"
        :class="['tab-btn', { active: activeTab === t.id }]"
        @click="activeTab = t.id"
      >
        {{ t.label }}
      </button>
    </nav>

    <main class="main-content">
      <!-- The service requires an API token (server.api_token). Nothing below
           can load until the operator provides it. -->
      <div v-if="authRequired" class="tab-panel">
        <div class="wizard-card">
          <div class="wizard-header">
            <h2>API token required</h2>
          </div>
          <p class="text-muted" style="margin-bottom:14px">
            This service is configured with an API token. Paste the value printed by
            <code>PlayoutTranscode gen-token</code> (also stored in <code>config.toml</code> as
            <code>server.api_token</code>). It is kept for this browser tab only.
          </p>
          <form @submit.prevent="onSubmitToken">
            <input
              v-model="tokenInput"
              type="password"
              class="input"
              autocomplete="off"
              placeholder="API token"
              style="width:100%;margin-bottom:12px"
            />
            <p v-if="tokenError" class="token-error" role="alert">{{ tokenError }}</p>
            <button class="btn btn-primary" type="submit" :disabled="!tokenInput.trim() || tokenChecking">
              {{ tokenChecking ? 'Checking…' : 'Connect' }}
            </button>
            <p class="text-muted" style="font-size:12px;margin-top:10px">
              The token is kept for this browser tab only — closing the tab forgets it.
            </p>
          </form>
        </div>
      </div>

      <div v-else-if="configStatus === 'loading'" class="loading-splash">
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
              <label for="f-watch-folder">Watch Folder</label>
              <input id="f-watch-folder" v-model="editWatchFolder" class="input" style="flex:1" placeholder="e.g. D:\media\incoming" />
            </div>
            <div class="form-row">
              <label for="f-target-folder">Target Folder</label>
              <input id="f-target-folder" v-model="editTargetFolder" class="input" style="flex:1" placeholder="e.g. D:\media\mezzanine" />
            </div>
          </div>

          <div class="panel config-section">
            <div class="panel-header"><span class="panel-title text-warning">QUALITY</span></div>
            <p class="hint">Higher CRF = smaller files, lower quality. Range 0-51. Defaults are broadcast-grade.</p>
            <div class="form-row">
              <label for="f-hd-progressive-crf">HD Progressive CRF</label>
              <input id="f-hd-progressive-crf" type="range" min="18" max="51" v-model.number="editCrfA" />
              <span class="mono">{{ editCrfA }}</span>
              <label style="margin-left:16px" for="f-hd-interlaced-crf">HD Interlaced CRF</label>
              <input id="f-hd-interlaced-crf" type="range" min="18" max="51" v-model.number="editCrfB" />
              <span class="mono">{{ editCrfB }}</span>
              <label style="margin-left:16px" for="f-sd-pal-crf">SD PAL CRF</label>
              <input id="f-sd-pal-crf" type="range" min="18" max="51" v-model.number="editCrfC" />
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
            :retry="onRetryJob"
            :cancel="onCancelJob"
            :dismiss="onDismissJob"
            :clear-all="onClearAllFailed"
            @retry-all="askRetryAll"
          />
          <AssetRegistryGrid
            :assets="assets"
            :trash-asset="trashAsset"
            :purge-asset="purgeAsset"
            :clear-asset-verdict="clearAssetVerdict"
            @changed="refreshAsset"
          />

          <div v-if="!serviceRunning && !stats.total" class="empty-state">
            <div style="font-size:18px;color:var(--text-secondary);margin-bottom:12px">Service is stopped</div>
            <button class="btn btn-play" @click="onStart">&#9654;  Start Service</button>
          </div>
        </div>

        <div v-if="activeTab === 'config'" class="tab-panel">
          <div class="panel config-section">
            <div class="panel-header"><span class="panel-title text-accent">FILE PATHS</span></div>
            <div class="form-row">
              <label for="f-watch-folder-2">Watch Folder</label>
              <input id="f-watch-folder-2" v-model="editWatchFolder" class="input" style="flex:1" />
            </div>
            <div class="form-row">
              <label for="f-target-folder-2">Target Folder</label>
              <input id="f-target-folder-2" v-model="editTargetFolder" class="input" style="flex:1" />
            </div>
          </div>

          <div class="panel config-section">
            <div class="panel-header"><span class="panel-title text-warning">ENCODING</span></div>
            <div class="form-row">
              <label for="f-x264-preset">x264 Preset</label>
              <select id="f-x264-preset" v-model="editPreset">
                <option v-for="p in PRESETS" :key="p" :value="p">{{ p }}</option>
              </select>
              <label style="margin-left:16px" for="f-tune">Tune</label>
              <select id="f-tune" v-model="editTune">
                <option v-for="t in TUNES" :key="t" :value="t">{{ t }}</option>
              </select>
              <label style="margin-left:16px" for="f-audio">Audio</label>
              <select id="f-audio" v-model="editAudioCodec">
                <option v-for="a in AUDIO_CODECS" :key="a" :value="a">{{ a }}</option>
              </select>
            </div>
            <div class="form-row">
              <label for="f-audio-bitrate">Audio Bitrate</label>
              <input id="f-audio-bitrate" v-model="editAudioBitrate" class="input" style="width:80px" />
            </div>
            <div class="form-row">
              <label for="f-profile-a-crf">Profile A CRF</label>
              <input id="f-profile-a-crf" type="range" min="0" max="51" v-model.number="editCrfA" />
              <span class="mono">{{ editCrfA }}</span>
              <label style="margin-left:16px" for="f-profile-b-crf">Profile B CRF</label>
              <input id="f-profile-b-crf" type="range" min="0" max="51" v-model.number="editCrfB" />
              <span class="mono">{{ editCrfB }}</span>
              <label style="margin-left:16px" for="f-profile-c-crf">Profile C CRF</label>
              <input id="f-profile-c-crf" type="range" min="0" max="51" v-model.number="editCrfC" />
              <span class="mono">{{ editCrfC }}</span>
            </div>
            <div class="form-row">
              <label for="f-cpu-cores-budget">CPU cores budget</label>
              <input id="f-cpu-cores-budget" type="number" min="0" max="128" v-model.number="editCpuCores" class="input" style="width:70px" />
              <span class="text-muted field-note">0 = auto (half of available cores). Split across concurrent encodes.</span>
            </div>
            <div class="form-row">
              <label for="f-threads-per-encode">Threads per encode</label>
              <input id="f-threads-per-encode" type="number" min="0" max="128" v-model.number="editThreads" class="input" style="width:70px" />
              <span class="text-muted field-note">0 = auto (cores ÷ max_concurrency). Non-zero overrides.</span>
            </div>
            <div class="form-row thread-summary" v-if="effectiveThreadsDisplay">
              <span class="mono">{{ effectiveThreadsDisplay }}</span>
              <span v-if="oversubscribed" style="color:var(--accent-amber);margin-left:12px">⚠ oversubscribed vs {{ availableCores }} logical cores</span>
            </div>
            <div class="form-row">
              <label for="f-hd-maxrate">HD Maxrate</label>
              <input id="f-hd-maxrate" v-model="editMaxrateAB" class="input" style="width:80px" />
              <label style="margin-left:16px" for="f-hd-bufsize">HD Bufsize</label>
              <input id="f-hd-bufsize" v-model="editBufsizeAB" class="input" style="width:80px" />
              <label style="margin-left:16px" for="f-sd-maxrate">SD Maxrate</label>
              <input id="f-sd-maxrate" v-model="editMaxrateC" class="input" style="width:80px" />
              <label style="margin-left:16px" for="f-sd-bufsize">SD Bufsize</label>
              <input id="f-sd-bufsize" v-model="editBufsizeC" class="input" style="width:80px" />
            </div>
          </div>

          <div class="panel config-section">
            <div class="panel-header"><span class="panel-title" style="color:var(--accent-emerald)">AUDIO NORMALIZATION &amp; QC</span></div>
            <div class="form-row">
              <label for="f-loudness-mode">Loudness Mode</label>
              <select id="f-loudness-mode" v-model="editAudioMode">
                <option value="legacy_v1_encode">Legacy (Preserve / Pass-through)</option>
                <option value="ebu_r128">EBU R128 (-23 LUFS / -1 dBTP / 7 LRA)</option>
                <option value="atsc_a85">ATSC A/85 (-24 LUFS / -2 dBTP / 7 LRA)</option>
                <option value="passthrough_validate">Passthrough &amp; Validate Only</option>
                <option value="analyze_only">Analyze &amp; Report Only</option>
              </select>
            </div>
            <div class="form-row" v-if="editAudioMode === 'ebu_r128' || editAudioMode === 'atsc_a85'">
              <label for="f-target-lufs">Target LUFS</label>
              <input id="f-target-lufs" type="number" step="0.5" v-model.number="editAudioTargetLufs" class="input" style="width:80px" :placeholder="editAudioMode === 'ebu_r128' ? '-23.0' : '-24.0'" />
              <label style="margin-left:16px" for="f-true-peak-dbtp">True Peak (dBTP)</label>
              <input id="f-true-peak-dbtp" type="number" step="0.5" v-model.number="editAudioTruePeak" class="input" style="width:80px" :placeholder="editAudioMode === 'ebu_r128' ? '-1.0' : '-2.0'" />
              <label style="margin-left:16px" for="f-lra-target">LRA Target</label>
              <input id="f-lra-target" type="number" step="0.5" v-model.number="editAudioLra" class="input" style="width:80px" placeholder="7.0" />
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
              <label for="f-max-concurrent">Max concurrent</label>
              <input id="f-max-concurrent" type="number" min="1" max="16" v-model.number="editConcurrency" class="input" style="width:70px" />
              <label style="margin-left:16px" for="f-poll-interval-s">Poll interval (s)</label>
              <input id="f-poll-interval-s" v-model="editPollSecs" class="input" style="width:70px" />
              <label style="margin-left:16px" for="f-settle-time-s">Settle time (s)</label>
              <input id="f-settle-time-s" v-model="editSettleSecs" class="input" style="width:70px" />
            </div>
            <div class="form-row">
              <label for="f-stable-polls">Stable polls</label>
              <input id="f-stable-polls" type="number" min="1" max="20" v-model.number="editStablePolls" class="input" style="width:70px" />
              <label style="margin-left:16px" for="f-retry-policy">Retry policy</label>
              <select id="f-retry-policy" v-model="editRetryPolicy">
                <option v-for="r in RETRY_POLICIES" :key="r" :value="r">{{ r }}</option>
              </select>
            </div>
            <div class="form-row">
              <label class="checkbox-label">
                <input type="checkbox" v-model="editAutoRetryOnStart" />
                Auto-purge &amp; retry failed jobs on startup
              </label>
              <span class="text-muted field-note">Purges error rows whose source is still in the watch folder; the watcher re-queues them.</span>
            </div>
            <div class="form-row">
              <label for="f-max-attempts">Max attempts</label>
              <input id="f-max-attempts" type="number" min="1" max="10" v-model.number="editMaxAttempts" class="input" style="width:70px" />
              <label style="margin-left:16px" for="f-retry-delay-ms">Retry delay (ms)</label>
              <input id="f-retry-delay-ms" type="number" min="0" max="60000" v-model.number="editRetryDelayMs" class="input" style="width:90px" />
            </div>
            <div class="form-row" style="margin-top:8px">
              <label class="checkbox-label">
                <input type="checkbox" v-model="editCleanSourceAfterSuccess" />
                Delete source file after verified transcode &amp; publication
              </label>
              <span class="text-muted field-note">Destructive opt-in: safely removed from watch folder only after QC pass and DB mark_ready.</span>
            </div>
          </div>

          <div class="config-actions">
            <button
              class="btn btn-primary btn-save"
              :disabled="saving"
              @click="saveConfig"
            >{{ saving ? 'Saving…' : 'Save Configuration' }}</button>
            <button v-if="configDirty" class="btn btn-discard" :disabled="saving" @click="discardChanges">
              Discard changes
            </button>
            <span v-if="configDirty" class="dirty-chip">Unsaved changes</span>
            <span class="text-muted config-hint">Ctrl+S saves</span>
            <!-- The result used to vanish after 4 s even if the operator had
                 looked away; it now stays until the next edit (UX-06). -->
            <span
              v-if="saveMsg"
              :class="['save-msg', saveOk ? 'save-ok' : 'save-err']"
              role="status"
              aria-live="polite"
            >{{ saveMsg }}</span>
          </div>
        </div>

        <div v-if="activeTab === 'database'" class="tab-panel">
          <DbViewer />
        </div>

        <div v-if="activeTab === 'logs'" class="tab-panel">
          <div class="panel log-panel">
            <div class="log-toolbar">
              <div class="log-filters" role="group" aria-label="Log level filter">
                <button
                  v-for="f in LOG_FILTERS"
                  :key="f.id"
                  class="btn log-chip"
                  :class="{ active: logFilter === f.id }"
                  :aria-pressed="logFilter === f.id"
                  @click="logFilter = f.id"
                >{{ f.label }}</button>
              </div>
              <button
                class="btn log-chip"
                :class="{ active: logPaused }"
                :aria-pressed="logPaused"
                @click="logPaused = !logPaused"
              >{{ logPaused ? '▶ Resume' : '⏸ Pause' }}</button>
              <button class="btn log-chip" @click="clearLogs">Clear</button>
              <span class="text-muted log-count mono">{{ visibleLogLines.length }} / {{ logLines.length }}</span>
            </div>
            <div class="log-viewer" ref="logViewerRef" @scroll="onLogScroll">
              <div v-if="!visibleLogLines.length" class="text-muted log-empty">No log entries</div>
              <div
                v-for="line in visibleLogLines"
                :key="line.seq"
                class="log-line"
                :class="logLevel(line.text)"
              >{{ line.text }}</div>
            </div>
            <!-- An operator scrolled up to read an error used to be yanked
                 back to the bottom every 2 s (UX-05). -->
            <button v-if="newLineCount > 0" class="log-jump" @click="jumpToLatest">
              {{ newLineCount }} new line{{ newLineCount === 1 ? '' : 's' }} ↓
            </button>
          </div>
        </div>
      </template>
    </main>

    <ConfirmDialog
      :open="confirm.kind !== null"
      :title="confirm.title"
      :body="confirm.body"
      :detail="confirm.detail"
      :confirm-label="confirm.label"
      :busy="serviceBusy"
      @confirm="runConfirmed"
      @cancel="closeConfirm"
    />
  </div>
</template>

<script setup lang="ts">
import { ref, watch, nextTick, onMounted, onUnmounted, computed, defineAsyncComponent } from 'vue'
import { useEventStream, type ConfigPayload } from './composables/useEventStream'
import BroadcastTopBar from './components/BroadcastTopBar.vue'
import IngestQueuePanel from './components/IngestQueuePanel.vue'
import AssetRegistryGrid from './components/AssetRegistryGrid.vue'
import ConfirmDialog from './components/ConfirmDialog.vue'
// The largest component in the UI by a wide margin, and the Database tab is
// almost never the first screen an operator opens. Loading it on demand keeps
// it out of the dashboard's bundle.
const DbViewer = defineAsyncComponent(() => import('./components/DbViewer.vue'))

const PRESETS = ['ultrafast', 'veryfast', 'faster', 'fast', 'medium', 'slow', 'slower', 'veryslow']
const AUDIO_CODECS = ['aac', 'pcm_s16le', 'libmp3lame']
const TUNES = ['film', 'grain', 'animation', 'none']
const RETRY_POLICIES = ['never', 'once', 'always']

const tabs = [
  { id: 'dashboard', label: 'Dashboard' },
  { id: 'database', label: 'Database' },
  { id: 'config', label: 'Configuration' },
  { id: 'logs', label: 'Logs' },
]
const activeTab = ref('dashboard')

const {
  jobs, assets, watchfolder, stats, config, toolchain,
  serviceRunning, serviceStatus, downloading, logs, logLines, linkState, uptimeMs,
  fetchConfig, putConfig, startService, stopService, waitForStopped, downloadFFmpeg,
  setLogPolling, clearLogs, retryJob, cancelJob, retryAllFailed, dismissJob, dismissFinishedJobs,
  trashAsset, purgeAsset, clearAssetVerdict,
  fetchServiceStatus, refreshAsset,
  authRequired, applyApiToken,
} = useEventStream()

const tokenInput = ref('')
const tokenError = ref('')
const tokenChecking = ref(false)

async function onSubmitToken() {
  const value = tokenInput.value.trim()
  if (!value || tokenChecking.value) return
  tokenError.value = ''
  tokenChecking.value = true
  try {
    const result = await applyApiToken(value)
    if (!result.ok) {
      // Keep what they typed: it is usually a paste that lost a character,
      // not a value they want to retype from scratch (UX-08).
      tokenError.value = result.error || 'Token rejected by the service'
      return
    }
    tokenInput.value = ''
    await loadAndDecideWizard()
  } finally {
    tokenChecking.value = false
  }
}

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
const editCleanSourceAfterSuccess = ref(false)

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
}

async function onCancelJob(id: string) {
  const r = await cancelJob(id)
  const ok = !!r?.success
  const msg = ok ? 'Cancelling job…' : (r?.error || 'Cancel failed')
  ingestPanelRef.value?.showRetryMsg(msg, ok)
}

/**
 * Dismissing a job clears a *record*, never media, so it is not put behind the
 * confirm dialog -- an × that opens a modal is an × nobody presses twice.
 */
async function onDismissJob(id: string) {
  const r = await dismissJob(id)
  if (!r.success) ingestPanelRef.value?.showRetryMsg(r.error || 'Could not dismiss', false)
}

async function onClearAllFailed() {
  const r = await dismissFinishedJobs('failed')
  const ok = !r.error
  ingestPanelRef.value?.showRetryMsg(
    ok ? `Cleared ${r.dismissed} failed job${r.dismissed === 1 ? '' : 's'}` : (r.error as string),
    ok,
  )
}

type ConfirmKind = 'stop' | 'restart' | 'retryAll'

const serviceBusy = ref(false)
const confirm = ref<{
  kind: ConfirmKind | null
  title: string
  body: string
  detail?: string
  label: string
}>({ kind: null, title: '', body: '', label: '' })

function closeConfirm() {
  confirm.value = { kind: null, title: '', body: '', label: '' }
}

const runningCount = computed(
  () => Array.from(jobs.value.values()).filter((j) => j.state === 'Processing').length,
)
const failedCount = computed(
  () => Array.from(jobs.value.values()).filter((j) => j.state === 'Failed').length,
)

function askStop() {
  const n = runningCount.value
  confirm.value = {
    kind: 'stop',
    title: 'Stop ingest?',
    body: n
      ? `${n} encode${n === 1 ? '' : 's'} in progress will be cancelled and restarted from scratch on the next start.`
      : 'Nothing is encoding right now, so no work will be lost.',
    detail: 'The web UI and the API stay up; only the ingest loop stops.',
    label: 'Stop ingest',
  }
}

function askRestart() {
  const n = runningCount.value
  confirm.value = {
    kind: 'restart',
    title: 'Restart processing?',
    body: n
      ? `${n} encode${n === 1 ? '' : 's'} in progress will be cancelled and restarted from scratch.`
      : 'Nothing is encoding right now, so no work will be lost.',
    detail: 'The loop restarts with the configuration you just saved.',
    label: 'Restart processing',
  }
}

function askRetryAll() {
  const n = failedCount.value
  confirm.value = {
    kind: 'retryAll',
    title: 'Retry all failed jobs?',
    body: `${n} failed job${n === 1 ? '' : 's'} will be re-queued.`,
    detail: 'Jobs whose source file is no longer in the watch folder are reported as source-missing rather than retried.',
    label: 'Retry all',
  }
}

async function runConfirmed() {
  const kind = confirm.value.kind
  if (!kind || serviceBusy.value) return
  serviceBusy.value = true
  try {
    if (kind === 'stop') {
      const r = await stopService()
      const err = (r as { error?: string } | null)?.error
      // A 409/503 refusal (T2-5) carries a reason; it used to reach only the
      // console.
      if (err) ingestPanelRef.value?.showRetryMsg(err, false)
    } else if (kind === 'restart') {
      const stopped = await stopService()
      const stopErr = (stopped as { error?: string } | null)?.error
      if (stopErr) {
        ingestPanelRef.value?.showRetryMsg(stopErr, false)
      } else if (!(await waitForStopped())) {
        ingestPanelRef.value?.showRetryMsg('The service is still stopping; start it again once it has stopped', false)
      } else {
        const started = await startService()
        if (started && !started.success) {
          ingestPanelRef.value?.showRetryMsg(started.error || 'Failed to start service', false)
        }
      }
      await fetchServiceStatus()
    } else {
      await onRetryAll()
    }
  } finally {
    serviceBusy.value = false
    closeConfirm()
  }
}

async function onRetryAll() {
  ingestPanelRef.value?.setRetryingAll(true)
  const r = await retryAllFailed().finally(() => ingestPanelRef.value?.setRetryingAll(false))
  const submitted = r?.submitted ?? 0
  const missing = r?.source_missing ?? 0
  const errors = r?.errors ?? 0
  const ok = submitted > 0 && errors === 0
  const msg = submitted > 0
    ? `Re-queued ${submitted} job${submitted === 1 ? '' : 's'}${missing ? `, ${missing} source missing` : ''}`
    : (errors ? 'Retry failed' : (missing ? `${missing} source missing` : 'Nothing to retry'))
  ingestPanelRef.value?.showRetryMsg(msg, ok)
}

const LOG_FILTERS = [
  { id: 'all', label: 'All' },
  { id: 'warn', label: 'Warn' },
  { id: 'error', label: 'Error' },
  { id: 'audit', label: 'Audit' },
] as const
type LogFilter = (typeof LOG_FILTERS)[number]['id']

const logFilter = ref<LogFilter>('all')
const logPaused = ref(false)
/** Snapshot held while paused, so the view does not move under the operator. */
const frozenLines = ref<{ seq: number; text: string }[]>([])
const stickToBottom = ref(true)
const newLineCount = ref(0)

const visibleLogLines = computed(() => {
  const source = logPaused.value ? frozenLines.value : logLines.value
  if (logFilter.value === 'all') return source
  if (logFilter.value === 'audit') {
    return source.filter((l) => l.text.includes('[AUDIT]') || l.text.includes('audit'))
  }
  return source.filter((l) => logLevel(l.text) === logFilter.value)
})

watch(logPaused, (paused) => {
  frozenLines.value = paused ? logLines.value.slice() : []
  if (!paused) {
    newLineCount.value = 0
    void nextTick(scrollLogsToBottom)
  }
})

/** Within a couple of lines of the bottom counts as "following". */
const STICK_THRESHOLD_PX = 24

function onLogScroll() {
  const el = logViewerRef.value
  if (!el) return
  const distance = el.scrollHeight - el.scrollTop - el.clientHeight
  stickToBottom.value = distance <= STICK_THRESHOLD_PX
  if (stickToBottom.value) newLineCount.value = 0
}

function scrollLogsToBottom() {
  const el = logViewerRef.value
  if (el) el.scrollTop = el.scrollHeight
}

function jumpToLatest() {
  logPaused.value = false
  stickToBottom.value = true
  newLineCount.value = 0
  void nextTick(scrollLogsToBottom)
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
  editCleanSourceAfterSuccess.value = cfg.ingestion.clean_source_after_success ?? false
  if (cfg.audio_policy) {
    editAudioMode.value = cfg.audio_policy.mode || 'legacy_v1_encode'
    editAudioTargetLufs.value = cfg.audio_policy.target_lufs
    editAudioTruePeak.value = cfg.audio_policy.true_peak_dbtp
    editAudioLra.value = cfg.audio_policy.lra_target
    editAudioDualMono.value = !!cfg.audio_policy.dual_mono
  }
  availableCores.value = cfg.system?.available_logical_cores ?? 0
  recomputeThreads()
  loadedSnapshot.value = editSnapshot()
}

/**
 * The values as last loaded from the service, for comparison.
 *
 * `watch(config, populateFromConfig)` overwrote every edit field whenever
 * `config` changed -- a tab switch, any future refresh -- silently discarding
 * unsaved work (UX-06).
 */
const loadedSnapshot = ref('')

/** Everything the Configuration tab can change, in a stable order. */
function editSnapshot(): string {
  return JSON.stringify([
    editWatchFolder.value, editTargetFolder.value, editPreset.value, editTune.value,
    editAudioCodec.value, editAudioBitrate.value,
    editCrfA.value, editCrfB.value, editCrfC.value,
    editMaxrateAB.value, editBufsizeAB.value, editMaxrateC.value, editBufsizeC.value,
    editConcurrency.value, editPollSecs.value, editSettleSecs.value, editStablePolls.value,
    editRetryPolicy.value, editThreads.value, editCpuCores.value,
    editAutoRetryOnStart.value, editMaxAttempts.value, editRetryDelayMs.value,
    editCleanSourceAfterSuccess.value,
    editAudioMode.value, editAudioTargetLufs.value, editAudioTruePeak.value,
    editAudioLra.value, editAudioDualMono.value,
  ])
}

const configDirty = computed(() => editSnapshot() !== loadedSnapshot.value)
const saving = ref(false)

function discardChanges() {
  if (config.value) populateFromConfig(config.value)
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
  if (saving.value) return
  saving.value = true
  saveMsg.value = ''
  saveOk.value = false
  // The form edits a subset of the config. Everything it does not show has to
  // go back exactly as loaded: `enabled: true` was hard-coded for all three
  // profiles, so saving from the UI silently re-enabled a disabled profile,
  // and the audio policy's sample rate, channels, layout and preserve flag
  // were overwritten with constants. `audio_policy` is replaced as a whole on
  // the server, so it is sent as the loaded policy with the edited fields on
  // top.
  const loaded = config.value
  const loadedAudio = loaded?.audio_policy
  // One "HD" rate field drives A and B. Leave B alone unless it was edited, so
  // a B that was tuned separately in config.toml keeps its own values.
  const hdRateEdited = !loaded
    || editMaxrateAB.value !== loaded.profiles.a.maxrate
    || editBufsizeAB.value !== loaded.profiles.a.bufsize
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
        sample_rate_hz: 48000,
        channels: 2,
        preserve_original: false,
        ...loadedAudio,
        mode: editAudioMode.value,
        codec: editAudioCodec.value,
        bitrate: editAudioBitrate.value,
        target_lufs: editAudioTargetLufs.value,
        true_peak_dbtp: editAudioTruePeak.value,
        lra_target: editAudioLra.value,
        dual_mono: editAudioDualMono.value,
      },
      profile_a: { crf: editCrfA.value, maxrate: editMaxrateAB.value, bufsize: editBufsizeAB.value },
      profile_b: hdRateEdited
        ? { crf: editCrfB.value, maxrate: editMaxrateAB.value, bufsize: editBufsizeAB.value }
        : { crf: editCrfB.value },
      profile_c: { crf: editCrfC.value, maxrate: editMaxrateC.value, bufsize: editBufsizeC.value },
      ingestion: {
        settle_secs: Number(editSettleSecs.value) || 5,
        poll_secs: Number(editPollSecs.value) || 10,
        max_concurrency: editConcurrency.value,
        stable_polls_min: editStablePolls.value,
        retry_policy: editRetryPolicy.value,
        auto_retry_on_start: editAutoRetryOnStart.value,
        max_attempts: editMaxAttempts.value,
        retry_delay_ms: editRetryDelayMs.value,
        clean_source_after_success: editCleanSourceAfterSuccess.value,
      },
    } as unknown as Partial<ConfigPayload>)
    saveMsg.value = 'Configuration saved'
    saveOk.value = true
    showWizard.value = false
    loadedSnapshot.value = editSnapshot()
  } catch (e: unknown) {
    // A 422 from put_config carries `{"error": …}`; apiPut already unwraps it.
    const msg = e instanceof Error ? e.message : String(e)
    saveMsg.value = `Failed to save: ${msg}`
    saveOk.value = false
  } finally {
    saving.value = false
  }
}

async function saveConfigAndStart() {
  await saveConfig()
  if (saveOk.value) {
    await onStart()
  }
}

async function onStart() {
  if (serviceBusy.value) return
  serviceBusy.value = true
  try {
    const r = await startService()
    // The server's refusal reason (T2-5) belongs on screen, not in an alert
    // that discards it as soon as it is dismissed.
    if (r && !r.success) {
      ingestPanelRef.value?.showRetryMsg(r.error || 'Failed to start service', false)
    }
  } finally {
    serviceBusy.value = false
  }
}

onMounted(loadAndDecideWizard)

watch(activeTab, (tab) => {
  if (tab === 'config') fetchConfig()
  // The log ring is only polled while the tab that shows it is on screen
  // (SF-01): it was 500 lines every 2 s from every open tab, forever.
  setLogPolling(tab === 'logs')
}, { immediate: true })

watch([editThreads, editCpuCores, editConcurrency], recomputeThreads)

watch(config, (cfg) => {
  // Never overwrite edits that have not been saved. "Discard changes" is how
  // an operator asks for the loaded values back.
  if (cfg && !configDirty.value) populateFromConfig(cfg)
})

// Keyboard-first: Ctrl+S saves the Configuration tab.
function onGlobalKeydown(e: KeyboardEvent) {
  if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === 's') {
    if (activeTab.value !== 'config' || showWizard.value) return
    e.preventDefault()
    void saveConfig()
  }
}

/** A refused download used to be ignored: the button did nothing, silently. */
async function onDownload() {
  const r = await downloadFFmpeg()
  if (!r?.success) {
    ingestPanelRef.value?.showRetryMsg('Could not start the FFmpeg download (one may already be running)', false)
  }
}

function onBeforeUnload(e: BeforeUnloadEvent) {
  if (!configDirty.value) return
  e.preventDefault()
  // Chrome ignores the string but needs returnValue set to show its own prompt.
  e.returnValue = ''
}

/** Arrow-key navigation across the tab bar, as a tablist is expected to have. */
function onTabKeydown(e: KeyboardEvent) {
  const index = tabs.findIndex((t) => t.id === activeTab.value)
  let next = index
  if (e.key === 'ArrowRight') next = (index + 1) % tabs.length
  else if (e.key === 'ArrowLeft') next = (index - 1 + tabs.length) % tabs.length
  else if (e.key === 'Home') next = 0
  else if (e.key === 'End') next = tabs.length - 1
  else return
  e.preventDefault()
  const target = tabs[next]
  if (!target) return
  activeTab.value = target.id
  void nextTick(() => {
    document.getElementById(`tab-${target.id}`)?.focus()
  })
}

onMounted(() => {
  window.addEventListener('keydown', onGlobalKeydown)
  window.addEventListener('beforeunload', onBeforeUnload)
})

onUnmounted(() => {
  window.removeEventListener('keydown', onGlobalKeydown)
  window.removeEventListener('beforeunload', onBeforeUnload)
})

// Follow the tail only if the operator was already at the tail. Forcing
// scrollTop = scrollHeight on every update yanked anyone reading an error back
// to the bottom every 2 s (UX-05).
watch(logLines, async (lines, previous) => {
  // Counted by sequence number, not length: once the ring is full at 500 lines
  // every poll trims as many as it adds, so the length difference was always 0.
  const lastSeq = lines[lines.length - 1]?.seq ?? 0
  const prevSeq = previous?.[previous.length - 1]?.seq ?? 0
  const added = prevSeq && lastSeq > prevSeq
    ? lines.filter((l) => l.seq > prevSeq).length
    : Math.max(0, lines.length - (previous?.length ?? 0))
  if (logPaused.value || !stickToBottom.value) {
    newLineCount.value += added
    return
  }
  await nextTick()
  scrollLogsToBottom()
})
</script>

<style scoped>
.restart-banner {
  display: flex;
  align-items: center;
  gap: 10px;
  margin: 0 16px 12px;
  padding: 10px 14px;
  font-size: 13px;
  color: var(--accent-amber);
  background: rgba(245, 166, 35, 0.08);
  border: 1px solid rgba(245, 166, 35, 0.35);
  border-radius: var(--radius-base);
}

.restart-glyph {
  font-size: 14px;
}

.restart-btn {
  margin-left: auto;
  padding: 4px 14px;
  font-size: 12px;
  border-color: var(--accent-amber);
  color: var(--accent-amber);
  white-space: nowrap;
}

.token-error {
  color: var(--accent-crimson);
  font-size: 12px;
  margin-bottom: 10px;
}

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
  font-size: 12.5px;
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

.log-panel {
  padding: 12px;
  display: flex;
  flex-direction: column;
  height: calc(100vh - 220px);
  position: relative;
}

.log-toolbar {
  display: flex;
  align-items: center;
  gap: 8px;
  margin-bottom: 8px;
  flex-wrap: wrap;
}

.log-filters {
  display: flex;
  gap: 4px;
}

.log-chip {
  font-size: 12px;
  padding: 4px 12px;
}

.log-chip.active {
  border-color: var(--accent-cyan);
  color: var(--accent-cyan);
}

.log-count {
  margin-left: auto;
  font-size: 12px;
}

.log-empty {
  padding: 20px;
  text-align: center;
}

.log-jump {
  position: absolute;
  right: 24px;
  bottom: 22px;
  padding: 5px 14px;
  font-size: 12px;
  font-weight: 600;
  color: var(--bg-primary);
  background: var(--accent-cyan);
  border: none;
  border-radius: 14px;
  cursor: pointer;
  box-shadow: 0 4px 14px rgba(0, 0, 0, 0.4);
}

.config-actions {
  display: flex;
  gap: 12px;
  align-items: center;
  margin-top: 16px;
  flex-wrap: wrap;
}

.btn-save {
  padding: 10px 32px;
  font-size: 14px;
}

.btn-discard {
  padding: 8px 18px;
  font-size: 13px;
}

.dirty-chip {
  font-size: 12px;
  font-weight: 600;
  color: var(--accent-amber);
  background: rgba(245, 166, 35, 0.1);
  padding: 4px 10px;
  border-radius: 4px;
}

/* Explanatory text beside a field. 11px was unreadable on an MCR monitor at
   arm's length; 12px keeps it secondary without being a squint. */
.field-note {
  font-size: 12px;
}

.thread-summary {
  font-size: 12px;
  color: var(--accent-cyan);
  font-variant-numeric: tabular-nums;
}

.config-hint {
  /* 11px was too small to read on an MCR monitor at arm's length. */
  font-size: 12px;
}
</style>

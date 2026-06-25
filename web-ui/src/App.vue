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
      <button
        v-for="t in tabs"
        :key="t.id"
        :class="['tab-btn', { active: activeTab === t.id }]"
        @click="activeTab = t.id"
      >
        {{ t.label }}
      </button>
    </nav>

    <main class="main-content">
      <div v-if="activeTab === 'dashboard'" class="tab-panel">
        <IngestQueuePanel :jobs="jobs" />
        <AssetRegistryGrid :assets="assets" />

        <div v-if="!serviceRunning && !stats.total" class="empty-state">
          <div style="font-size:18px;color:var(--text-secondary);margin-bottom:12px">Service is stopped</div>
          <button class="btn btn-play" @click="onStart">&#9654;  Start Service</button>
        </div>
      </div>

      <div v-if="activeTab === 'config'" class="tab-panel">
        <div class="panel config-section">
          <div class="panel-header"><span class="panel-title">FILE PATHS</span></div>
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
            <input type="number" min="0" max="128" v-model.number="editThreads" class="input" style="width:70px" />
            <span class="text-muted" style="font-size:11px">0 = auto (all cores)</span>
          </div>
          <div class="form-row">
            <label>HD Maxrate</label>
            <input v-model="editMaxrateA" class="input" style="width:80px" />
            <label style="margin-left:16px">SD Maxrate</label>
            <input v-model="editMaxrateC" class="input" style="width:80px" />
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
        </div>

        <button class="btn btn-primary" style="margin-top:16px;padding:10px 32px;font-size:14px" @click="saveConfig">Save Configuration</button>
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
    </main>
  </div>
</template>

<script setup lang="ts">
import { ref, watch, nextTick } from 'vue'
import { useEventStream } from './composables/useEventStream'
import BroadcastTopBar from './components/BroadcastTopBar.vue'
import IngestQueuePanel from './components/IngestQueuePanel.vue'
import AssetRegistryGrid from './components/AssetRegistryGrid.vue'

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

const {
  jobs, assets, watchfolder, stats, config, toolchain,
  serviceRunning, downloading, logs, uptimeMs,
  fetchConfig, startService, stopService, downloadFFmpeg,
  installService, uninstallService, clearLogs,
} = useEventStream()

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

function logLevel(line: string): string {
  if (line.includes('[ERROR]') || line.includes('error:')) return 'error'
  if (line.includes('[WARN]')) return 'warn'
  if (line.includes('Completed')) return 'success'
  return 'info'
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

function saveConfig() {
  alert('Configuration save is not yet implemented via the API. Use the wizard or edit config.toml directly.')
}

watch(activeTab, () => {
  if (activeTab.value === 'config') fetchConfig()
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

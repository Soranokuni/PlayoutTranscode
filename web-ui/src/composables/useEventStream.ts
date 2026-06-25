import { ref, onMounted, onUnmounted } from 'vue'

export interface JobRecord {
  id: string
  input_path: string
  output_path?: string
  profile: string
  uuid?: string
  state: 'Pending' | 'Processing' | 'Completed' | 'Failed'
  progress: number
  current_stage: string
  duration_secs: number
  error?: string
  created_at: string
  finished_at?: string
  source_frame_count: number
  current_frame: number
  encode_fps: number
  encode_bitrate: string
  encode_speed: string
  current_time_ms: number
  duration_ms: number
}

export interface ProgressPayload {
  id: string
  percent: number
  current_time_ms: number
  duration_ms: number
  determinate: boolean
  fps: number
  bitrate: string
  speed: string
  stage: string
}

export interface AssetRecord {
  uuid: string
  current_path: string
  duration_ms: number
  trim_in_ms: number
  trim_out_ms: number
  rating: string
  status: string
  display_name: string
  virtual_folder: string
}

export interface HealthPayload {
  status: string
  service: string
  version: string
  toolchain_ready: boolean
  service_running: boolean
  uptime_ms: number
}

export interface WatchfolderPayload {
  watch_folder: string
  target_folder: string
  settle_secs: number
  poll_secs: number
  stable_polls_min: number
  retry_policy: string
  max_concurrency: number
}

export interface StatsPayload {
  pending: number
  active: number
  completed: number
  failed: number
  total: number
}

export interface ConfigPayload {
  paths: { watch_folder: string; target_folder: string }
  encoding: { preset: string; ffmpeg_threads: number; audio_codec: string; audio_bitrate: string; tune: string }
  profiles: { a: { enabled: boolean; crf: number; maxrate: string; bufsize: string }; b: { enabled: boolean; crf: number; maxrate: string; bufsize: string }; c: { enabled: boolean; crf: number; maxrate: string; bufsize: string } }
  ingestion: { settle_secs: number; poll_secs: number; max_concurrency: number; stable_polls_min: number; retry_policy: string; clean_source_after_success: boolean }
  logging: { level: string }
}

export interface ToolchainPayload {
  ffmpeg_found: boolean
  ffprobe_found: boolean
  ffmpeg_version: string | null
  ffprobe_version: string | null
  bin_dir: string
}

function shortFileName(path: string) {
  return path?.split('\\').pop()?.split('/').pop() || path
}

export function useEventStream() {
  const jobs = ref<Map<string, JobRecord>>(new Map())
  const assets = ref<AssetRecord[]>([])
  const health = ref<HealthPayload | null>(null)
  const watchfolder = ref<WatchfolderPayload | null>(null)
  const stats = ref<StatsPayload>({ pending: 0, active: 0, completed: 0, failed: 0, total: 0 })
  const config = ref<ConfigPayload | null>(null)
  const toolchain = ref<ToolchainPayload>({ ffmpeg_found: false, ffprobe_found: false, ffmpeg_version: null, ffprobe_version: null, bin_dir: '' })
  const serviceRunning = ref(false)
  const downloading = ref(false)
  const logs = ref<string[]>([])
  const uptimeMs = ref(0)

  let sseConnection: EventSource | null = null
  let pollInterval: number = 0
  let reconnectDelay = 500

  async function apiGet<T = unknown>(path: string): Promise<T | null> {
    try {
      const r = await fetch('/api' + path)
      if (!r.ok) return null
      return await r.json()
    } catch {
      return null
    }
  }

  async function apiPost<T = unknown>(path: string): Promise<T | null> {
    try {
      const r = await fetch('/api' + path, { method: 'POST' })
      if (!r.ok) return null
      return await r.json()
    } catch {
      return null
    }
  }

  async function fetchAll() {
    const [h, t, s, j, l, w] = await Promise.all([
      apiGet<HealthPayload>('/health'),
      apiGet<ToolchainPayload>('/toolchain'),
      apiGet<StatsPayload>('/stats'),
      apiGet<JobRecord[]>('/jobs'),
      apiGet<string[]>('/logs'),
      apiGet<WatchfolderPayload>('/watchfolder'),
    ])
    if (h) {
      serviceRunning.value = h.service_running
      uptimeMs.value = h.uptime_ms
      health.value = h
    }
    if (t) toolchain.value = t
    if (s) stats.value = s
    if (j) {
      const map = new Map<string, JobRecord>()
      for (const job of j) map.set(job.id, job)
      jobs.value = map
    }
    if (l) logs.value = l
    if (w) watchfolder.value = w
  }

  async function fetchConfig() {
    const c = await apiGet<ConfigPayload>('/config')
    if (c) config.value = c
  }

  async function fetchAssets(statusFilter?: string) {
    const url = statusFilter ? `/assets?status=${encodeURIComponent(statusFilter)}` : '/assets'
    const list = await apiGet<AssetRecord[]>(url)
    if (list) assets.value = list
  }

  function handleSSEEvent(eventType: string, data: unknown) {
    switch (eventType) {
      case 'progress': {
        const p = data as ProgressPayload
        const map = new Map(jobs.value)
        const existing = map.get(p.id)
        if (existing) {
          map.set(p.id, {
            ...existing,
            progress: p.percent,
            current_stage: p.stage,
            current_frame: 0,
            encode_fps: p.fps,
            encode_bitrate: p.bitrate,
            encode_speed: p.speed,
            current_time_ms: p.current_time_ms,
            duration_ms: p.duration_ms,
          })
          jobs.value = map
        }
        break
      }
      case 'completed':
      case 'failed': {
        fetchAll()
        fetchAssets()
        break
      }
      case 'connected': {
        fetchAll()
        break
      }
    }
  }

  function connectSSE() {
    if (sseConnection) sseConnection.close()
    sseConnection = new EventSource('/api/events')

    sseConnection.addEventListener('progress', (e) => {
      try { handleSSEEvent('progress', JSON.parse(e.data)) } catch { /* ignore parse errors */ }
    })
    sseConnection.addEventListener('completed', (e) => {
      try { handleSSEEvent('completed', JSON.parse(e.data)) } catch { /* ignore */ }
    })
    sseConnection.addEventListener('failed', (e) => {
      try { handleSSEEvent('failed', JSON.parse(e.data)) } catch { /* ignore */ }
    })
    sseConnection.addEventListener('connected', () => {
      handleSSEEvent('connected', {})
    })

    sseConnection.onopen = () => {
      reconnectDelay = 500
    }

    sseConnection.onerror = () => {
      sseConnection?.close()
      reconnectDelay = Math.min(reconnectDelay * 2, 5000)
      setTimeout(connectSSE, reconnectDelay)
    }
  }

  async function startService() {
    const r = await apiPost<{ success: boolean; error?: string }>('/service/start')
    if (r?.success) {
      serviceRunning.value = true
      await fetchAll()
    }
    return r
  }

  async function stopService() {
    await apiPost('/service/stop')
    serviceRunning.value = false
  }

  async function downloadFFmpeg() {
    const r = await apiPost<{ success: boolean }>('/download/start')
    if (r?.success) downloading.value = true
    return r
  }

  async function installService() {
    return apiPost<{ success?: boolean; message?: string; error?: string }>('/service/install')
  }

  async function uninstallService() {
    return apiPost<{ success?: boolean; message?: string; error?: string }>('/service/uninstall')
  }

  function clearLogs() {
    logs.value = []
  }

  onMounted(() => {
    fetchAll()
    fetchConfig()
    fetchAssets()
    connectSSE()

    pollInterval = window.setInterval(async () => {
      await fetchAll()
      const ds = await apiGet<{ status: string }>('/download/status')
      downloading.value = ds?.status === 'downloading'
    }, 2000)
  })

  onUnmounted(() => {
    clearInterval(pollInterval)
    sseConnection?.close()
  })

  return {
    jobs,
    assets,
    health,
    watchfolder,
    stats,
    config,
    toolchain,
    serviceRunning,
    downloading,
    logs,
    uptimeMs,
    fetchAll,
    fetchConfig,
    fetchAssets,
    startService,
    stopService,
    downloadFFmpeg,
    installService,
    uninstallService,
    clearLogs,
    shortFileName,
  }
}

import { ref, onMounted, onUnmounted } from 'vue'

export interface JobRecord {
  id: string
  input_path: string
  output_path?: string
  profile: string
  uuid?: string
  state: 'Pending' | 'Processing' | 'Completed' | 'Failed' | 'Cancelled'
  phase?: string
  progress: number
  current_stage: string
  duration_secs: number
  error?: string
  error_category?: string
  /** Verbose diagnostic tail (ffmpeg stderr). Rendered inside a collapsible widget in the UI. */
  stderr_log?: string[]
  /** Retry attempt counter (0 = first try, 1 = first retry, ...). */
  attempt?: number
  max_attempts?: number
  worker_id?: string
  cancel_requested?: boolean
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

export interface AudioPolicyPayload {
  mode: 'legacy_v1_encode' | 'ebu_r128' | 'atsc_a85' | 'passthrough_validate' | 'analyze_only'
  codec: string
  bitrate: string
  sample_rate_hz: number
  channels: number
  channel_layout?: string
  target_lufs?: number
  true_peak_dbtp?: number
  lra_target?: number
  dual_mono: boolean
  preserve_original: boolean
}

export interface ConfigPayload {
  paths: { watch_folder: string; target_folder: string }
  encoding: {
    preset: string
    ffmpeg_threads: number
    cpu_cores: number
    audio_codec: string
    audio_bitrate: string
    tune: string
    probesize: string
    analyzeduration: string
    effective_threads_per_encode?: number
    effective_total_threads?: number
  }
  audio_policy?: AudioPolicyPayload
  profiles: { a: { enabled: boolean; crf: number; maxrate: string; bufsize: string }; b: { enabled: boolean; crf: number; maxrate: string; bufsize: string }; c: { enabled: boolean; crf: number; maxrate: string; bufsize: string } }
  ingestion: {
    settle_secs: number
    poll_secs: number
    max_concurrency: number
    stable_polls_min: number
    retry_policy: string
    auto_retry_on_start: boolean
    max_attempts: number
    retry_delay_ms: number
    clean_source_after_success: boolean
  }
  logging: { level: string }
  system?: { available_logical_cores?: number }
  initialized: boolean
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
      const text = await r.text()
      if (!text || text.trim() === '') {
        return null
      }
      try {
        return JSON.parse(text) as T
      } catch (parseError) {
        console.error('[useEventStream] apiGet JSON parse failed:', parseError, '\nBody:', text)
        return null
      }
    } catch (error) {
      console.error('[useEventStream] apiGet request failed:', error)
      return null
    }
  }

  async function apiPost<T = unknown>(path: string): Promise<T | null> {
    try {
      const r = await fetch('/api' + path, { method: 'POST' })
      if (!r.ok) return null
      const text = await r.text()
      if (!text || text.trim() === '') {
        return null
      }
      try {
        return JSON.parse(text) as T
      } catch (parseError) {
        console.error('[useEventStream] apiPost JSON parse failed:', parseError, '\nBody:', text)
        return null
      }
    } catch (error) {
      console.error('[useEventStream] apiPost request failed:', error)
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

  async function apiPut<T = unknown>(path: string, body: unknown): Promise<T | null> {
    try {
      const r = await fetch('/api' + path, {
        method: 'PUT',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(body),
      })
      if (!r.ok) {
        const err = await r.json().catch(() => ({ error: r.statusText }))
        throw new Error((err as { error?: string }).error || r.statusText)
      }
      const text = await r.text()
      if (!text || text.trim() === '') return { success: true } as T
      try {
        return JSON.parse(text) as T
      } catch {
        return { success: true } as T
      }
    } catch (error) {
      console.error('[useEventStream] apiPut failed:', error)
      throw error
    }
  }

  async function putConfig(body: Partial<ConfigPayload>) {
    await apiPut('/config', body)
    await fetchConfig()
  }
  async function fetchConfig() {
    const c = await apiGet<ConfigPayload>('/config')
    if (c) config.value = c
    return c
  }

  /** Manually re-queue one failed job for immediate reprocessing. */
  async function retryJob(id: string): Promise<{ success: boolean; error?: string }> {
    try {
      const r = await fetch('/api/jobs/' + encodeURIComponent(id) + '/retry', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({}),
      })
      const text = await r.text()
      if (!text) return { success: r.ok }
      const parsed = JSON.parse(text) as { success?: boolean; error?: string }
      return { success: !!parsed.success, error: parsed.error }
    } catch (e) {
      console.error('[useEventStream] retryJob failed:', e)
      return { success: false, error: String(e) }
    }
  }

  /** Re-queue all currently-failed jobs in one shot. */
  async function retryAllFailed(): Promise<{ submitted: number; source_missing: number; errors: number }> {
    try {
      const r = await fetch('/api/jobs/retry-failed', { method: 'POST' })
      const text = await r.text()
      if (!text) return { submitted: 0, source_missing: 0, errors: 0 }
      const parsed = JSON.parse(text) as { submitted?: number; source_missing?: number; errors?: number }
      return {
        submitted: parsed.submitted ?? 0,
        source_missing: parsed.source_missing ?? 0,
        errors: parsed.errors ?? 0,
      }
    } catch (e) {
      console.error('[useEventStream] retryAllFailed failed:', e)
      return { submitted: 0, source_missing: 0, errors: 1 }
    }
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

  async function cancelJob(id: string): Promise<{ success: boolean; error?: string }> {
    try {
      const r = await fetch('/api/jobs/' + encodeURIComponent(id) + '/cancel', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({}),
      })
      const text = await r.text()
      if (!text) return { success: r.ok }
      const parsed = JSON.parse(text) as { success?: boolean; error?: string }
      if (parsed.success) {
        await fetchAll()
      }
      return { success: !!parsed.success, error: parsed.error }
    } catch (e) {
      console.error('[useEventStream] cancelJob failed:', e)
      return { success: false, error: String(e) }
    }
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
    putConfig,
    fetchAssets,
    startService,
    stopService,
    downloadFFmpeg,
    installService,
    uninstallService,
    clearLogs,
    retryJob,
    cancelJob,
    retryAllFailed,
    shortFileName,
  }
}

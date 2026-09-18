import { ref, shallowRef, computed, triggerRef, onMounted, onUnmounted } from 'vue'
import {
  apiFetch,
  apiFetchDestructive,
  eventSourceUrl,
  onAuthRequired,
  setApiToken,
  getApiToken,
} from '../api/auth'

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

/** `GET /api/service/status`. `state` distinguishes the transitional states
 *  that the boolean `running` collapses; `restart_required` is true when the
 *  processing loop predates the saved configuration. */
export interface ServiceStatusPayload {
  running: boolean
  state: 'running' | 'starting' | 'stopping' | 'stopped' | string
  generation: number
  restart_required: boolean
}

/** One line of the log ring, keyed by the server's monotonic cursor. */
export interface LogLine {
  seq: number
  text: string
}

/** How the UI is currently getting its data. Shown in the top bar, because a
 *  dead stream behind a working poll used to look exactly like a live one. */
export type LinkState = 'live' | 'reconnecting' | 'offline'

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
  // `shallowRef` + explicit `triggerRef`: a progress event mutates the record
  // in place instead of rebuilding the whole Map, so a tick no longer
  // invalidates every computed that reads `jobs` and re-renders every row.
  const jobs = shallowRef<Map<string, JobRecord>>(new Map())
  const assets = ref<AssetRecord[]>([])
  const health = ref<HealthPayload | null>(null)
  const watchfolder = ref<WatchfolderPayload | null>(null)
  const config = ref<ConfigPayload | null>(null)
  const toolchain = ref<ToolchainPayload>({ ffmpeg_found: false, ffprobe_found: false, ffmpeg_version: null, ffprobe_version: null, bin_dir: '' })
  const serviceStatus = ref<ServiceStatusPayload | null>(null)
  const serviceRunning = ref(false)
  const downloading = ref(false)
  const logLines = shallowRef<LogLine[]>([])
  const uptimeMs = ref(0)
  const linkState = ref<LinkState>('reconnecting')

  /** The five counters `/api/stats` reports, derived from the job list the UI
   *  already holds. Both are `state.jobs` on the server, so polling the second
   *  endpoint only ever confirmed the first. */
  const stats = computed<StatsPayload>(() => {
    const counts: StatsPayload = { pending: 0, active: 0, completed: 0, failed: 0, total: 0 }
    for (const job of jobs.value.values()) {
      counts.total++
      switch (job.state) {
        case 'Pending': counts.pending++; break
        case 'Processing': counts.active++; break
        case 'Completed': counts.completed++; break
        case 'Failed': counts.failed++; break
      }
    }
    return counts
  })

  /** Kept as plain strings for every existing consumer. */
  const logs = computed<string[]>(() => logLines.value.map((l) => l.text))

  // True once the service has answered 401: the operator must enter the token
  // configured in `server.api_token` (T1-1).
  const authRequired = ref(false)
  const apiToken = ref(getApiToken())
  onAuthRequired((required) => {
    authRequired.value = required
  })

  /**
   * Try a token and say whether the service accepted it.
   *
   * This used to apply the token, reconnect and refetch without waiting: a
   * wrong token produced a 401 that set `authRequired` back to true, so the
   * same empty form reappeared with no message and the operator could not tell
   * "rejected" from "nothing happened" (UX-08).
   */
  async function applyApiToken(value: string): Promise<{ ok: boolean; error?: string }> {
    const previous = getApiToken()
    setApiToken(value)
    apiToken.value = getApiToken()

    let accepted = false
    try {
      const r = await apiFetch('/api/stats')
      accepted = r.ok
      if (!accepted && r.status !== 401) {
        setApiToken(previous ?? '')
        apiToken.value = getApiToken()
        return { ok: false, error: `Service answered ${r.status}` }
      }
    } catch {
      setApiToken(previous ?? '')
      apiToken.value = getApiToken()
      return { ok: false, error: 'Could not reach the service' }
    }

    if (!accepted) {
      setApiToken(previous ?? '')
      apiToken.value = getApiToken()
      return { ok: false, error: 'Token rejected by the service' }
    }

    // The SSE stream carries the token in its URL, so it has to be rebuilt.
    connectSSE()
    await fetchAll()
    return { ok: true }
  }

  let sseConnection: EventSource | null = null
  let liveTimer = 0
  let staticTimer = 0
  let downloadTimer = 0
  let logTimer = 0
  let reconnectDelay = 500
  let logCursor = 0
  let logPollingEnabled = false

  async function apiGet<T = unknown>(path: string): Promise<T | null> {
    try {
      const r = await apiFetch('/api' + path)
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

  async function apiPost<T = unknown>(path: string, destructive = false): Promise<T | null> {
    try {
      const send = destructive ? apiFetchDestructive : apiFetch
      const r = await send('/api' + path, { method: 'POST' })
      // A refused POST still carries `{ success: false, error }`. Since T2-5
      // `/api/service/start` answers 409/503/400 instead of a 200 with that
      // body, so discarding non-2xx here would have turned every refusal into a
      // silent no-op button.
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

  /** Everything that changes while work is running. */
  async function fetchLive() {
    const [h, j, st] = await Promise.all([
      apiGet<HealthPayload>('/health'),
      apiGet<JobRecord[]>('/jobs'),
      apiGet<ServiceStatusPayload>('/service/status'),
    ])
    if (h) {
      serviceRunning.value = h.service_running
      uptimeMs.value = h.uptime_ms
      health.value = h
    }
    if (j) {
      const map = new Map<string, JobRecord>()
      for (const job of j) map.set(job.id, job)
      jobs.value = map
    }
    if (st) {
      serviceStatus.value = st
      serviceRunning.value = st.running
    }
  }

  /** Everything that only changes when an operator changes it. `/toolchain` is
   *  cached server-side for 60 s, and `/watchfolder` only moves on a save. */
  async function fetchStatic() {
    const [t, w] = await Promise.all([
      apiGet<ToolchainPayload>('/toolchain'),
      apiGet<WatchfolderPayload>('/watchfolder'),
    ])
    if (t) toolchain.value = t
    if (w) watchfolder.value = w
  }

  /** Just the service lifecycle, for right after a save/start/stop. */
  async function fetchServiceStatus() {
    const st = await apiGet<ServiceStatusPayload>('/service/status')
    if (st) {
      serviceStatus.value = st
      serviceRunning.value = st.running
    }
    return st
  }

  async function fetchAll() {
    await Promise.all([fetchLive(), fetchStatic()])
  }

  /**
   * Pull only the log lines added since the last poll.
   *
   * The ring shifts by one on every line, so refetching all 500 on every poll changed
   * the content at every index and made the viewer repaint every row (UX-05).
   */
  async function fetchLogs(force = false) {
    if (force) logCursor = 0
    const page = await apiGet<{ lines: LogLine[]; next: number; dropped: boolean }>(
      `/logs?since=${logCursor}`,
    )
    if (!page) return
    if (page.dropped || logCursor === 0) {
      logLines.value = page.lines
    } else if (page.lines.length > 0) {
      const merged = logLines.value.concat(page.lines)
      logLines.value = merged.length > 500 ? merged.slice(merged.length - 500) : merged
    }
    logCursor = page.next
  }

  /** Called by the Logs tab so the ring is only polled while it is on screen. */
  function setLogPolling(enabled: boolean) {
    logPollingEnabled = enabled
    if (enabled) void fetchLogs(true)
  }

  async function apiPut<T = unknown>(
    path: string,
    body: unknown,
    destructive = false,
  ): Promise<T | null> {
    try {
      const send = destructive ? apiFetchDestructive : apiFetch
      const r = await send('/api' + path, {
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
    // PUT /config requires X-Confirm-Destructive (T1-5).
    await apiPut('/config', body, true)
    await fetchConfig()
    // The running loop kept its old clone of the config (F-23), so the save
    // may have left the service running on stale values. UX-01 turns this into
    // a visible banner instead of nothing happening.
    await fetchServiceStatus()
  }
  async function fetchConfig() {
    const c = await apiGet<ConfigPayload>('/config')
    if (c) config.value = c
    return c
  }

  /** Manually re-queue one failed job for immediate reprocessing. */
  async function retryJob(id: string): Promise<{ success: boolean; error?: string }> {
    try {
      const r = await apiFetch('/api/jobs/' + encodeURIComponent(id) + '/retry', {
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
      const r = await apiFetchDestructive('/api/jobs/retry-failed', { method: 'POST' })
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

  /**
   * Fetch the whole library, a page at a time.
   *
   * Since T2-7 `GET /api/assets` is capped (1000 by default, 5000 maximum) and
   * reports the real total in `X-Total-Count`. Asking once and keeping whatever
   * came back would silently truncate the library at 1000 assets, which looks
   * exactly like assets having gone missing.
   */
  async function fetchAssets(statusFilter?: string) {
    const PAGE = 1000
    const base = statusFilter ? `/assets?status=${encodeURIComponent(statusFilter)}&` : '/assets?'
    const collected: AssetRecord[] = []
    let offset = 0
    let total = Infinity

    // Bounded, so a server that keeps reporting a total it never delivers
    // cannot spin here forever.
    for (let page = 0; page < 100 && offset < total; page++) {
      let r: Response
      try {
        r = await apiFetch(`/api${base}limit=${PAGE}&offset=${offset}`)
      } catch (error) {
        console.error('[useEventStream] fetchAssets request failed:', error)
        return
      }
      if (!r.ok) return

      const header = r.headers.get('X-Total-Count')
      if (header !== null) {
        const parsed = Number(header)
        if (Number.isFinite(parsed)) total = parsed
      }

      let batch: AssetRecord[]
      try {
        batch = (await r.json()) as AssetRecord[]
      } catch (error) {
        console.error('[useEventStream] fetchAssets JSON parse failed:', error)
        return
      }
      collected.push(...batch)

      // A server that predates T2-7 sends no header and no limit, so the first
      // response is already the whole library.
      if (header === null || batch.length < PAGE) break
      offset += batch.length
    }

    assets.value = collected
  }

  /**
   * Replace one asset in the library instead of re-paging all of it.
   *
   * A `completed` or `skipped` payload already carries the uuid, so a bulk drop
   * of 30 files no longer reloads a 5 000-asset library 30 times (SF-02).
   */
  async function refreshAsset(uuid: string) {
    const a = await apiGet<AssetRecord>(`/assets/${encodeURIComponent(uuid)}`)
    if (!a) return
    const index = assets.value.findIndex((x) => x.uuid === a.uuid)
    if (index >= 0) {
      assets.value.splice(index, 1, a)
    } else {
      assets.value.push(a)
    }
  }

  // Terminal events arrive in bursts -- several encodes finishing within a
  // second used to fire several concurrent full refreshes. Collect them and do
  // one pass on the trailing edge.
  let terminalTimer = 0
  const pendingUuids = new Set<string>()
  const TERMINAL_DEBOUNCE_MS = 750

  function scheduleTerminalRefresh(uuid?: string) {
    if (uuid) pendingUuids.add(uuid)
    if (terminalTimer) return
    terminalTimer = window.setTimeout(async () => {
      terminalTimer = 0
      const uuids = Array.from(pendingUuids)
      pendingUuids.clear()
      await fetchLive()
      if (uuids.length === 0) {
        // No uuid to aim at (a plain `failed`), so nothing in the library
        // changed shape -- the status came down with fetchLive.
        return
      }
      await Promise.all(uuids.map((u) => refreshAsset(u)))
    }, TERMINAL_DEBOUNCE_MS)
  }

  function handleSSEEvent(eventType: string, data: unknown) {
    switch (eventType) {
      case 'progress': {
        const p = data as ProgressPayload
        const existing = jobs.value.get(p.id)
        if (existing) {
          // In place: rebuilding the Map on every tick invalidated every
          // computed reading `jobs` and re-rendered every row, including the
          // failed list with its <pre> stderr blocks (SF-03).
          Object.assign(existing, {
            progress: p.percent,
            current_stage: p.stage,
            current_frame: 0,
            encode_fps: p.fps,
            encode_bitrate: p.bitrate,
            encode_speed: p.speed,
            current_time_ms: p.current_time_ms,
            duration_ms: p.duration_ms,
          })
          triggerRef(jobs)
        }
        break
      }
      case 'completed':
      case 'skipped': {
        const payload = data as { uuid?: string }
        scheduleTerminalRefresh(payload?.uuid)
        break
      }
      case 'failed': {
        // The asset is `error`; nothing to fetch unless the payload names one.
        const payload = data as { uuid?: string }
        scheduleTerminalRefresh(payload?.uuid)
        break
      }
      case 'connected': {
        linkState.value = 'live'
        fetchAll()
        fetchAssets()
        break
      }
      // The server dropped events for this subscriber -- a throttled background
      // tab, a laptop that slept (T2-10). Whatever is on screen is stale, so
      // refetch rather than keep applying deltas to a wrong baseline.
      case 'resync': {
        const r = data as { dropped?: number }
        console.warn('[useEventStream] missed', r?.dropped ?? '?', 'event(s); resynchronising')
        fetchAll()
        fetchAssets()
        if (logPollingEnabled) void fetchLogs(true)
        break
      }
    }
  }

  function connectSSE() {
    if (sseConnection) sseConnection.close()
    sseConnection = new EventSource(eventSourceUrl('/api/events'))

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
    // T2-10. Without this the UI keeps applying progress deltas to a job list
    // it has already lost events for, and silently shows a stale queue.
    sseConnection.addEventListener('resync', (e) => {
      try { handleSSEEvent('resync', JSON.parse(e.data)) } catch { handleSSEEvent('resync', {}) }
    })
    // Emitted when an ingest was skipped as a confirmed duplicate (T2-6). It is
    // a terminal outcome, so the queue and the library both need refreshing.
    sseConnection.addEventListener('skipped', (e) => {
      try { handleSSEEvent('skipped', JSON.parse(e.data)) } catch { handleSSEEvent('skipped', {}) }
    })

    sseConnection.onopen = () => {
      reconnectDelay = 500
      linkState.value = 'live'
      schedulePolling()
    }

    sseConnection.onerror = () => {
      sseConnection?.close()
      // The stream is the primary channel, so losing it is what makes the poll
      // speed up -- and it is what the top bar reports, because a dead stream
      // behind a working poll used to look identical to a live one.
      linkState.value = 'reconnecting'
      schedulePolling()
      reconnectDelay = Math.min(reconnectDelay * 2, 5000)
      setTimeout(connectSSE, reconnectDelay)
    }
  }

  // With the stream healthy the poll is only a safety net; without it, it is
  // the data path.
  const LIVE_MS_SSE_OK = 15_000
  const LIVE_MS_SSE_DOWN = 2_000
  const STATIC_MS = 60_000
  const LOG_MS = 2_000

  function stopPolling() {
    window.clearInterval(liveTimer)
    window.clearInterval(staticTimer)
    window.clearInterval(logTimer)
    liveTimer = 0
    staticTimer = 0
    logTimer = 0
  }

  function schedulePolling() {
    stopPolling()
    // A hidden tab has nobody looking at it; `connected`/`resync` cover
    // whatever it missed when it comes back.
    if (typeof document !== 'undefined' && document.visibilityState === 'hidden') return

    const liveMs = linkState.value === 'live' ? LIVE_MS_SSE_OK : LIVE_MS_SSE_DOWN
    liveTimer = window.setInterval(() => { void fetchLive() }, liveMs)
    staticTimer = window.setInterval(() => { void fetchStatic() }, STATIC_MS)
    logTimer = window.setInterval(() => {
      if (logPollingEnabled) void fetchLogs()
    }, LOG_MS)
  }

  /** Poll `/download/status` only while a download is actually running. */
  function startDownloadPolling() {
    if (downloadTimer) return
    downloadTimer = window.setInterval(async () => {
      const ds = await apiGet<{ status: string }>('/download/status')
      downloading.value = ds?.status === 'downloading'
      if (!downloading.value) {
        window.clearInterval(downloadTimer)
        downloadTimer = 0
        // A finished download changes the toolchain.
        void fetchStatic()
      }
    }, 2000)
  }

  function onVisibilityChange() {
    if (document.visibilityState === 'visible') {
      void fetchLive()
      if (logPollingEnabled) void fetchLogs()
    }
    schedulePolling()
  }

  async function startService() {
    const r = await apiPost<{ success: boolean; error?: string }>('/service/start')
    if (r?.success) {
      serviceRunning.value = true
      await fetchAll()
    }
    await fetchServiceStatus()
    return r
  }

  async function stopService() {
    const r = await apiPost<{ success?: boolean; error?: string }>('/service/stop', true)
    serviceRunning.value = false
    await fetchServiceStatus()
    return r
  }

  async function downloadFFmpeg() {
    const r = await apiPost<{ success: boolean }>('/download/start')
    if (r?.success) {
      downloading.value = true
      startDownloadPolling()
    }
    return r
  }

  async function cancelJob(id: string): Promise<{ success: boolean; error?: string }> {
    try {
      const r = await apiFetch('/api/jobs/' + encodeURIComponent(id) + '/cancel', {
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
    logLines.value = []
  }

  onMounted(() => {
    fetchAll()
    fetchConfig()
    fetchAssets()
    connectSSE()
    schedulePolling()
    document.addEventListener('visibilitychange', onVisibilityChange)

    // A download already in flight when the tab opened still has to be
    // followed to its end.
    void apiGet<{ status: string }>('/download/status').then((ds) => {
      if (ds?.status === 'downloading') {
        downloading.value = true
        startDownloadPolling()
      }
    })
  })

  onUnmounted(() => {
    stopPolling()
    window.clearInterval(downloadTimer)
    window.clearTimeout(terminalTimer)
    document.removeEventListener('visibilitychange', onVisibilityChange)
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
    serviceStatus,
    serviceRunning,
    downloading,
    logs,
    logLines,
    linkState,
    uptimeMs,
    fetchAll,
    fetchLive,
    fetchServiceStatus,
    fetchLogs,
    setLogPolling,
    fetchConfig,
    putConfig,
    fetchAssets,
    startService,
    stopService,
    downloadFFmpeg,
    clearLogs,
    retryJob,
    cancelJob,
    retryAllFailed,
    shortFileName,
    authRequired,
    apiToken,
    applyApiToken,
  }
}

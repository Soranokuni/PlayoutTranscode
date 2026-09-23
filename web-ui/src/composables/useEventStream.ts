import { ref, shallowRef, computed, onMounted, onUnmounted } from 'vue'
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
  /** Absent on servers that predate UI-01. */
  current_frame?: number
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
  /** QC findings. Present on the v1 asset payload; absent on older servers. */
  warnings?: string[]
  /**
   * The service has recorded that this media already failed under the current
   * settings and will skip it rather than encode it again. Absent on older
   * servers, which never skipped anything.
   */
  retry_suppressed?: boolean
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
  // Every change assigns a *new* Map (UI-01). This used to mutate the Map in
  // place and call `triggerRef`, which re-rendered App -- but App hands
  // `jobs.value` to IngestQueuePanel as a prop, the prop was the same Map
  // reference, and Vue skipped the child. So a cancel, a dismiss, a new job and
  // every progress tick stayed invisible until the 15 s poll swapped the Map:
  // the "press ✕ and nothing happens until I refresh" bug. Copying a few
  // hundred entries per 250 ms tick is cheap, and it is reactive end to end.
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

  /** Replace (or add) one job record. */
  function putJob(job: JobRecord) {
    const next = new Map(jobs.value)
    next.set(job.id, job)
    jobs.value = next
    jobsEpoch++
  }

  /** Merge fields into one known job. Returns false if the id is unknown. */
  function patchJob(id: string, patch: Partial<JobRecord>): boolean {
    const existing = jobs.value.get(id)
    if (!existing) return false
    const next = new Map(jobs.value)
    next.set(id, { ...existing, ...patch })
    jobs.value = next
    return true
  }

  function dropJobs(ids: Iterable<string>) {
    let next: Map<string, JobRecord> | null = null
    for (const id of ids) {
      if (!(next ?? jobs.value).has(id)) continue
      next ??= new Map(jobs.value)
      next.delete(id)
    }
    if (next) {
      jobs.value = next
      jobsEpoch++
    }
  }

  // A `GET /jobs` sent before a structural change (a dismiss, a cancel, a
  // `job_update`) and answered after it describes the past; applying it
  // brought a dismissed row back for up to 15 s. `jobsEpoch` counts structural
  // changes -- not progress ticks, which would starve the poll -- and `liveSeq`
  // drops answers overtaken by a newer request.
  let jobsEpoch = 0
  let liveSeq = 0
  let staleJobsRetries = 0
  let liveRefreshTimer = 0

  /** Coalesce "something changed that I can't apply directly" into one poll. */
  function scheduleLiveRefresh(delayMs = 300) {
    if (liveRefreshTimer) return
    liveRefreshTimer = window.setTimeout(() => {
      liveRefreshTimer = 0
      void fetchLive()
    }, delayMs)
  }

  /**
   * Read a JSON body without trusting it to be JSON. A proxy error page or an
   * axum rejection is plain text, and `JSON.parse` on it used to reach the
   * operator as "SyntaxError: Unexpected token <".
   */
  async function readJson<T extends object>(r: Response): Promise<Partial<T> & { error?: string }> {
    const text = await r.text().catch(() => '')
    if (text.trim()) {
      try {
        const parsed = JSON.parse(text) as Partial<T> & { error?: string; detail?: string }
        if (!r.ok && !parsed.error) parsed.error = parsed.detail || `HTTP ${r.status}`
        return parsed
      } catch {
        /* not JSON: fall through */
      }
    }
    return (r.ok ? {} : { error: text.trim().slice(0, 200) || `HTTP ${r.status}` }) as Partial<T> & {
      error?: string
    }
  }

  let sseConnection: EventSource | null = null
  let connectedOnce = false
  let reconnectTimer = 0
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
    const seq = ++liveSeq
    const epoch = jobsEpoch
    const [h, j, st] = await Promise.all([
      apiGet<HealthPayload>('/health'),
      apiGet<JobRecord[]>('/jobs'),
      apiGet<ServiceStatusPayload>('/service/status'),
    ])
    // A newer request is in flight; its answer is the one worth keeping.
    if (seq !== liveSeq) return
    if (h) {
      serviceRunning.value = h.service_running
      uptimeMs.value = h.uptime_ms
      health.value = h
      if (linkState.value === 'offline') linkState.value = 'reconnecting'
    } else if (linkState.value !== 'live') {
      // Stream down *and* the poll cannot reach the service. Say so, rather
      // than "Reconnecting" forever over data that has stopped updating.
      linkState.value = 'offline'
    }
    if (j) {
      if (epoch !== jobsEpoch && staleJobsRetries < 3) {
        // Overtaken by a live change while in flight. Ask again rather than
        // roll the list back; bounded so a busy queue cannot starve the poll.
        staleJobsRetries++
        scheduleLiveRefresh(500)
      } else {
        staleJobsRetries = 0
        const map = new Map<string, JobRecord>()
        for (const job of j) map.set(job.id, job)
        jobs.value = map
      }
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
    // The top bar reads folders and concurrency from `/watchfolder`, which
    // otherwise only refreshed on the 60 s static poll.
    void fetchStatic()
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
      const parsed = await readJson<{ success: boolean }>(r)
      const success = r.ok && parsed.success !== false
      // `job_update` normally lands first; this covers a stream that is down.
      if (success) scheduleLiveRefresh()
      return { success, error: parsed.error }
    } catch (e) {
      console.error('[useEventStream] retryJob failed:', e)
      return { success: false, error: String(e) }
    }
  }

  /**
   * Take one finished job off the queue.
   *
   * The record only: the asset row, the mezzanine and the source file are all
   * untouched. The server refuses (409) for a job that is still running, so a
   * stray click cannot orphan a live encode.
   */
  async function dismissJob(id: string): Promise<{ success: boolean; error?: string }> {
    try {
      const r = await apiFetch('/api/jobs/' + encodeURIComponent(id), { method: 'DELETE' })
      if (r.ok) {
        // Drop it locally rather than waiting for the round trip: the × should
        // feel instant, and the `job_removed` event reconciles every other tab.
        dropJobs([id])
        return { success: true }
      }
      const parsed = await readJson(r)
      return { success: false, error: parsed.error }
    } catch (e) {
      console.error('[useEventStream] dismissJob failed:', e)
      return { success: false, error: String(e) }
    }
  }

  /** Clear every finished job in the given states. Defaults to the failed ones. */
  async function dismissFinishedJobs(
    state = 'failed',
  ): Promise<{ dismissed: number; error?: string }> {
    try {
      const r = await apiFetch('/api/jobs/finished?state=' + encodeURIComponent(state), {
        method: 'DELETE',
      })
      const parsed = await readJson<{ dismissed: number; ids: string[] }>(r)
      if (!r.ok) return { dismissed: 0, error: parsed.error }
      dropJobs(parsed.ids ?? [])
      return { dismissed: parsed.dismissed ?? 0 }
    } catch (e) {
      console.error('[useEventStream] dismissFinishedJobs failed:', e)
      return { dismissed: 0, error: String(e) }
    }
  }

  /**
   * Take an asset out of the library, reversibly.
   *
   * Soft delete: the row moves to the recycle bin and the media file is not
   * touched, so a mis-click costs a Restore rather than a re-ingest.
   */
  async function trashAsset(uuid: string): Promise<{ success: boolean; error?: string }> {
    try {
      const r = await apiFetch('/api/assets/' + encodeURIComponent(uuid) + '/trash', {
        method: 'POST',
      })
      if (r.ok) return { success: true }
      const parsed = await readJson(r)
      return { success: false, error: parsed.error }
    } catch (e) {
      console.error('[useEventStream] trashAsset failed:', e)
      return { success: false, error: String(e) }
    }
  }

  /**
   * Delete an asset permanently.
   *
   * `deleteFile` is the difference between "take it off the list" and "and bin
   * the mezzanine too". The server refuses to remove a file that a sub-clip
   * still plays regardless of what is asked for.
   */
  async function purgeAsset(
    uuid: string,
    deleteFile: boolean,
  ): Promise<{ success: boolean; mediaRemoved: boolean; warnings: string[]; error?: string }> {
    try {
      const r = await apiFetchDestructive(
        '/api/assets/' + encodeURIComponent(uuid) + '/purge?delete_file=' + (deleteFile ? 'true' : 'false'),
        { method: 'DELETE' },
      )
      const parsed = await readJson<{ media_removed: boolean; warnings: string[] }>(r)
      if (!r.ok) {
        return {
          success: false,
          mediaRemoved: false,
          warnings: parsed.warnings ?? [],
          error: parsed.error,
        }
      }
      return {
        success: true,
        mediaRemoved: !!parsed.media_removed,
        warnings: parsed.warnings ?? [],
      }
    } catch (e) {
      console.error('[useEventStream] purgeAsset failed:', e)
      return { success: false, mediaRemoved: false, warnings: [], error: String(e) }
    }
  }

  /**
   * Let the service reconsider media it has given up on.
   *
   * Clears the recorded QC verdict, so the next time this source is offered it
   * is judged from scratch instead of skipped.
   */
  async function clearAssetVerdict(
    uuid: string,
  ): Promise<{ success: boolean; error?: string }> {
    try {
      const r = await apiFetch(
        '/api/assets/' + encodeURIComponent(uuid) + '/clear-verdict',
        { method: 'POST' },
      )
      if (r.ok) return { success: true }
      const parsed = await readJson(r)
      return { success: false, error: parsed.error }
    } catch (e) {
      console.error('[useEventStream] clearAssetVerdict failed:', e)
      return { success: false, error: String(e) }
    }
  }

  /** Re-queue all currently-failed jobs in one shot. */
  async function retryAllFailed(): Promise<{ submitted: number; source_missing: number; errors: number }> {
    try {
      const r = await apiFetchDestructive('/api/jobs/retry-failed', { method: 'POST' })
      const parsed = await readJson<{ submitted: number; source_missing: number; errors: number }>(r)
      if (!r.ok) return { submitted: 0, source_missing: 0, errors: 1 }
      if ((parsed.submitted ?? 0) > 0) scheduleLiveRefresh()
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
  let assetsSeq = 0
  let assetsEpoch = 0
  async function fetchAssets(statusFilter?: string) {
    const seq = ++assetsSeq
    const epoch = assetsEpoch
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

    // Superseded by a newer full fetch: drop this one.
    if (seq !== assetsSeq) return
    if (epoch !== assetsEpoch) {
      // A single-asset refresh landed while this was paging; keep its copy.
      const fresh = new Map(assets.value.map((a) => [a.uuid, a]))
      collected.forEach((a, i) => {
        const f = fresh.get(a.uuid)
        if (f) collected[i] = f
      })
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
    let r: Response
    try {
      r = await apiFetch(`/api/assets/${encodeURIComponent(uuid)}`)
    } catch {
      return
    }
    if (r.status === 404) {
      // Trashed or purged: the single-asset route no longer serves it.
      assetsEpoch++
      assets.value = assets.value.filter((x) => x.uuid !== uuid)
      return
    }
    if (!r.ok) return
    let a: AssetRecord
    try {
      a = (await r.json()) as AssetRecord
    } catch {
      return
    }
    // A full re-page that started before this and lands after it would
    // otherwise overwrite the fresher record with its older copy.
    assetsEpoch++
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

  const assetUuidsToRefresh = new Set<string>()
  let assetUuidTimer = 0
  /** Refresh named assets once each, however many job updates named them. */
  function scheduleAssetRefreshFor(uuid: string) {
    assetUuidsToRefresh.add(uuid)
    if (assetUuidTimer) return
    assetUuidTimer = window.setTimeout(() => {
      assetUuidTimer = 0
      const uuids = Array.from(assetUuidsToRefresh)
      assetUuidsToRefresh.clear()
      for (const u of uuids) void refreshAsset(u)
    }, 300)
  }

  let assetsRefreshTimer = 0
  /** A folder operation can emit a burst; page the library once for all of it. */
  function scheduleAssetsRefresh() {
    if (assetsRefreshTimer) return
    assetsRefreshTimer = window.setTimeout(() => {
      assetsRefreshTimer = 0
      void fetchAssets()
    }, TERMINAL_DEBOUNCE_MS)
  }

  function handleSSEEvent(eventType: string, data: unknown) {
    switch (eventType) {
      case 'progress': {
        const p = data as ProgressPayload
        const patch: Partial<JobRecord> = {
          progress: p.percent,
          current_stage: p.stage,
          encode_fps: p.fps,
          encode_bitrate: p.bitrate,
          encode_speed: p.speed,
          current_time_ms: p.current_time_ms,
          duration_ms: p.duration_ms,
        }
        if (typeof p.current_frame === 'number') patch.current_frame = p.current_frame
        // A retry-progress frame carries only `{ id, stage }`; don't blank
        // the numbers it does not mention.
        for (const k of Object.keys(patch) as (keyof JobRecord)[]) {
          if (patch[k] === undefined) delete patch[k]
        }
        // Progress for a job this tab has never seen: it started between
        // polls. Fetch it instead of dropping every tick until the next poll.
        if (!patchJob(p.id, patch)) scheduleLiveRefresh()
        break
      }
      // Sent on every push, transition and cancel request (UI-01), with the
      // whole record, so it can be applied directly.
      case 'job_update': {
        const payload = data as { id?: string; job?: JobRecord }
        if (payload?.job?.id) {
          putJob(payload.job)
          // The registry row a job works on appears at Probing and goes
          // away on a cancel; neither reached the library until a reload.
          const u = payload.job.uuid
          const terminal = ['Completed', 'Failed', 'Cancelled'].includes(payload.job.state)
          if (u && (terminal || !assets.value.some((a) => a.uuid === u))) scheduleAssetRefreshFor(u)
        } else if (payload?.id) {
          // A server that predates UI-01 sends only `{ id, stage }`.
          scheduleLiveRefresh()
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
      // A library mutation succeeded somewhere -- this tab, another tab, or
      // PlayOut editing trim/rating/name (UI-02). One uuid is refreshed on its
      // own; a folder, batch or recycle-bin change re-pages the library once.
      case 'assets_changed': {
        const payload = data as { uuid?: string | null }
        if (payload?.uuid) {
          void refreshAsset(payload.uuid)
        } else {
          scheduleAssetsRefresh()
        }
        break
      }
      // Another tab (or this one) dismissed job records. Drop them rather than
      // refetching the whole list for a removal we can apply directly.
      case 'job_removed': {
        const payload = data as { ids?: string[] }
        dropJobs(payload?.ids ?? [])
        break
      }
      case 'connected': {
        linkState.value = 'live'
        fetchAll()
        // `onMounted` already paged the library for the first connection;
        // doing it again here fetched up to 5 000 assets twice on every load.
        // A *re*connect may have missed changes, so that one still re-pages.
        if (connectedOnce) fetchAssets()
        connectedOnce = true
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
    window.clearTimeout(reconnectTimer)
    reconnectTimer = 0
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
    sseConnection.addEventListener('job_update', (e) => {
      try { handleSSEEvent('job_update', JSON.parse(e.data)) } catch { /* ignore */ }
    })
    sseConnection.addEventListener('assets_changed', (e) => {
      try { handleSSEEvent('assets_changed', JSON.parse(e.data)) } catch { handleSSEEvent('assets_changed', {}) }
    })
    sseConnection.addEventListener('connected', () => {
      handleSSEEvent('connected', {})
    })
    // T2-10. Without this the UI keeps applying progress deltas to a job list
    // it has already lost events for, and silently shows a stale queue.
    sseConnection.addEventListener('resync', (e) => {
      try { handleSSEEvent('resync', JSON.parse(e.data)) } catch { handleSSEEvent('resync', {}) }
    })
    // Another tab (or this one) dismissed job records.
    sseConnection.addEventListener('job_removed', (e) => {
      try { handleSSEEvent('job_removed', JSON.parse(e.data)) } catch { /* nothing to remove */ }
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
      window.clearTimeout(reconnectTimer)
      reconnectTimer = window.setTimeout(connectSSE, reconnectDelay)
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

  /**
   * Wait for the ingest loop to actually reach `stopped`.
   *
   * `POST /service/stop` answers while the state is still `stopping` -- the
   * worker has to unwind its encodes first -- and a start in that window is
   * refused with 409 "Service is stopping". Restart fired the start straight
   * after the stop, so it failed every time a job was running.
   */
  async function waitForStopped(timeoutMs = 120_000): Promise<boolean> {
    const deadline = Date.now() + timeoutMs
    while (Date.now() < deadline) {
      const st = await fetchServiceStatus()
      if (st?.state === 'stopped') return true
      await new Promise((resolve) => window.setTimeout(resolve, 500))
    }
    return false
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
      const parsed = await readJson<{ success: boolean }>(r)
      const success = r.ok && parsed.success !== false
      // `job_update` carries the new phase; this covers a stream that is down.
      if (success) scheduleLiveRefresh()
      return { success, error: parsed.error }
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
    window.clearTimeout(liveRefreshTimer)
    window.clearTimeout(assetsRefreshTimer)
    window.clearTimeout(assetUuidTimer)
    window.clearTimeout(reconnectTimer)
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
    refreshAsset,
    startService,
    stopService,
    waitForStopped,
    downloadFFmpeg,
    clearLogs,
    retryJob,
    cancelJob,
    retryAllFailed,
    dismissJob,
    dismissFinishedJobs,
    trashAsset,
    purgeAsset,
    clearAssetVerdict,
    shortFileName,
    authRequired,
    apiToken,
    applyApiToken,
  }
}

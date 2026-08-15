<template>
  <div class="db-viewer">
    <!-- Sub-navigation header & Controls -->
    <div class="db-header-bar">
      <div class="db-tabs">
        <button
          v-for="sub in subTabs"
          :key="sub.id"
          :class="['db-tab-btn', { active: activeSubTab === sub.id }]"
          @click="activeSubTab = sub.id"
        >
          {{ sub.label }}
          <span v-if="sub.count !== undefined" class="count-badge">{{ sub.count }}</span>
        </button>
      </div>

      <div class="db-actions">
        <button class="btn btn-sm" :disabled="loading" @click="refreshCurrent">
          <span v-if="loading" class="spinner-sm"></span>
          <span v-else>↻</span>
          Refresh
        </button>
      </div>
    </div>

    <!-- 1. CLIPS & SUBCLIPS TAB -->
    <div v-if="activeSubTab === 'assets'" class="db-tab-content">
      <div class="filter-toolbar">
        <div class="filter-pills">
          <button
            v-for="f in assetFilters"
            :key="f.id"
            :class="['filter-pill', { active: assetFilter === f.id }]"
            @click="setAssetFilter(f.id)"
          >
            {{ f.label }}
          </button>
        </div>

        <div class="search-box">
          <input
            v-model="assetSearch"
            type="text"
            class="input search-input"
            placeholder="Search by name, UUID, folder, path..."
            @input="debouncedFetchAssets"
          />
          <button v-if="assetSearch" class="search-clear" @click="clearAssetSearch">✕</button>
        </div>
      </div>

      <div class="table-container panel">
        <table class="db-table">
          <thead>
            <tr>
              <th>Clip / Subclip</th>
              <th>Folder</th>
              <th>Type</th>
              <th>Duration / Trims</th>
              <th>Rating &amp; TP</th>
              <th>Status</th>
              <th>FPS Rational</th>
              <th>Mezzanine</th>
              <th>Keyframes</th>
              <th>Sidecar</th>
              <th class="text-right">Actions</th>
            </tr>
          </thead>
          <tbody>
            <tr v-if="loadingAssets && !assetsPage.items.length">
              <td colspan="11" class="text-center py-4 text-muted">Loading assets...</td>
            </tr>
            <tr v-else-if="!assetsPage.items.length">
              <td colspan="11" class="text-center py-4 text-muted">No database asset records match criteria</td>
            </tr>
            <tr v-for="a in assetsPage.items" :key="a.uuid" :class="{ 'row-trashed': a.deleted_at }">
              <td>
                <div class="asset-title-cell">
                  <span class="asset-name" :title="a.display_name">{{ a.display_name || 'Untitled' }}</span>
                  <span class="mono uuid-text" :title="a.uuid">{{ a.uuid }}</span>
                </div>
              </td>
              <td>
                <span class="folder-badge">{{ a.virtual_folder }}</span>
              </td>
              <td>
                <span v-if="a.is_subclip" class="badge badge-subclip" title="Virtual Subclip">✂ Subclip</span>
                <span v-else class="badge badge-master" title="Master Mezzanine">🎬 Master</span>
              </td>
              <td class="mono font-sm">
                <div>{{ formatMs(a.duration_ms) }}</div>
                <div v-if="a.trim_in_ms > 0 || a.trim_out_ms < a.duration_ms" class="trim-range text-muted">
                  [{{ formatMs(a.trim_in_ms) }} ➔ {{ formatMs(a.trim_out_ms) }}]
                </div>
              </td>
              <td>
                <span class="badge badge-rating">{{ a.rating || 'K' }}</span>
                <span v-if="a.tp && a.tp !== 'None'" class="badge badge-tp">TP</span>
              </td>
              <td>
                <span :class="['badge', statusBadgeClass(a.status, a.deleted_at)]">
                  {{ a.deleted_at ? 'Trashed' : a.status }}
                </span>
              </td>
              <td class="mono font-sm">
                <span v-if="a.fps_num > 0">{{ a.fps_num }}/{{ a.fps_den }}</span>
                <span v-else>{{ a.fps?.toFixed(2) || '0.00' }}</span>
              </td>
              <td>
                <span v-if="a.mezzanine_ok" class="badge badge-success" title="Closed GOP, 48kHz Audio, Faststart OK">✓ OK</span>
                <span v-else class="badge badge-warning" title="Not strictly compliant broadcast mezzanine">⚠ Non-Mezz</span>
              </td>
              <td class="mono font-sm">
                {{ a.keyframe_count }} pts
              </td>
              <td>
                <span v-if="a.sidecar_exists" class="badge badge-success" title="JSON sidecar exists">.json ✓</span>
                <span v-else class="badge badge-muted">—</span>
              </td>
              <td class="text-right">
                <button class="btn btn-xs" @click="inspectAsset(a.uuid)">Inspect</button>
              </td>
            </tr>
          </tbody>
        </table>
      </div>

      <!-- Pagination -->
      <div class="pagination-bar">
        <div class="pagination-info text-muted">
          Showing {{ assetStartRecord }} - {{ assetEndRecord }} of {{ assetsPage.total }} records
        </div>
        <div class="pagination-controls">
          <label class="font-sm text-muted">Rows:</label>
          <select v-model.number="assetLimit" class="select-sm" @change="fetchAssets">
            <option :value="15">15</option>
            <option :value="25">25</option>
            <option :value="50">50</option>
            <option :value="100">100</option>
          </select>
          <button class="btn btn-xs" :disabled="assetOffset <= 0" @click="prevAssetPage">‹ Prev</button>
          <button class="btn btn-xs" :disabled="assetOffset + assetLimit >= assetsPage.total" @click="nextAssetPage">Next ›</button>
        </div>
      </div>
    </div>

    <!-- 2. JOBS HISTORY TAB -->
    <div v-if="activeSubTab === 'jobs'" class="db-tab-content">
      <div class="filter-toolbar">
        <div class="filter-pills">
          <button
            v-for="s in jobStateFilters"
            :key="s.id"
            :class="['filter-pill', { active: jobStateFilter === s.id }]"
            @click="setJobStateFilter(s.id)"
          >
            {{ s.label }}
          </button>
        </div>

        <div class="search-box">
          <input
            v-model="jobSearch"
            type="text"
            class="input search-input"
            placeholder="Search by Job ID, UUID, Path, Error..."
            @input="debouncedFetchJobs"
          />
          <button v-if="jobSearch" class="search-clear" @click="clearJobSearch">✕</button>
        </div>
      </div>

      <div class="table-container panel">
        <table class="db-table">
          <thead>
            <tr>
              <th>Job ID &amp; Target UUID</th>
              <th>Input &amp; Output Mezzanine</th>
              <th>Profile</th>
              <th>State &amp; Progress</th>
              <th>Phase &amp; Stage</th>
              <th>Attempts</th>
              <th>Created &amp; Runtime</th>
              <th>Encode Metrics</th>
              <th>Error</th>
              <th class="text-right">Actions</th>
            </tr>
          </thead>
          <tbody>
            <tr v-if="loadingJobs && !jobsPage.items.length">
              <td colspan="10" class="text-center py-4 text-muted">Loading jobs...</td>
            </tr>
            <tr v-else-if="!jobsPage.items.length">
              <td colspan="10" class="text-center py-4 text-muted">No database job records match criteria</td>
            </tr>
            <tr v-for="j in jobsPage.items" :key="j.id">
              <td>
                <div class="asset-title-cell">
                  <span class="mono font-sm font-bold">{{ j.id.slice(0, 8) }}...</span>
                  <span class="mono font-xs text-muted" :title="j.uuid">{{ j.uuid ? j.uuid.slice(0, 8) + '...' : '—' }}</span>
                </div>
              </td>
              <td>
                <div class="path-cell">
                  <div class="input-path" :title="j.input_path">📥 {{ j.input_path_display }}</div>
                  <div v-if="j.output_path_display" class="output-path text-muted font-xs" :title="j.output_path || ''">
                    📤 {{ j.output_path_display }}
                  </div>
                </div>
              </td>
              <td>
                <span class="badge badge-profile">Profile {{ j.profile }}</span>
              </td>
              <td>
                <div class="state-progress-cell">
                  <span :class="['badge', jobStateBadgeClass(j.state)]">{{ j.state }}</span>
                  <div class="mini-progress-track">
                    <div class="mini-progress-fill" :style="{ width: `${Math.round(j.progress * 100)}%` }"></div>
                  </div>
                  <span class="mono font-xs">{{ Math.round(j.progress * 100) }}%</span>
                </div>
              </td>
              <td class="font-sm">
                <div>{{ j.phase }}</div>
                <div class="text-muted font-xs">{{ j.current_stage }}</div>
              </td>
              <td class="font-sm mono">
                {{ j.attempt }}/{{ j.max_attempts }}
              </td>
              <td class="font-sm mono">
                <div>{{ formatIso(j.created_at) }}</div>
                <div v-if="j.duration_secs > 0" class="text-muted font-xs">⏱ {{ j.duration_secs.toFixed(1) }}s</div>
              </td>
              <td class="font-xs mono">
                <div v-if="j.encode_fps > 0">{{ j.encode_fps.toFixed(1) }} fps</div>
                <div v-if="j.encode_speed" class="text-muted">{{ j.encode_speed }} | {{ j.encode_bitrate }}</div>
                <div v-else class="text-muted">—</div>
              </td>
              <td>
                <div v-if="j.error" class="error-cell" :title="j.error">
                  <span class="badge badge-danger">{{ j.error_category || 'Error' }}</span>
                  <span class="error-text font-xs">{{ j.error }}</span>
                </div>
                <span v-else class="text-muted font-xs">—</span>
              </td>
              <td class="text-right">
                <button class="btn btn-xs" @click="inspectJob(j.id)">Inspect</button>
              </td>
            </tr>
          </tbody>
        </table>
      </div>

      <!-- Pagination -->
      <div class="pagination-bar">
        <div class="pagination-info text-muted">
          Showing {{ jobStartRecord }} - {{ jobEndRecord }} of {{ jobsPage.total }} records
        </div>
        <div class="pagination-controls">
          <label class="font-sm text-muted">Rows:</label>
          <select v-model.number="jobLimit" class="select-sm" @change="fetchJobs">
            <option :value="15">15</option>
            <option :value="25">25</option>
            <option :value="50">50</option>
            <option :value="100">100</option>
          </select>
          <button class="btn btn-xs" :disabled="jobOffset <= 0" @click="prevJobPage">‹ Prev</button>
          <button class="btn btn-xs" :disabled="jobOffset + jobLimit >= jobsPage.total" @click="nextJobPage">Next ›</button>
        </div>
      </div>
    </div>

    <!-- 3. VIRTUAL FOLDERS TAB -->
    <div v-if="activeSubTab === 'folders'" class="db-tab-content">
      <div class="table-container panel">
        <table class="db-table">
          <thead>
            <tr>
              <th>Virtual Folder Path</th>
              <th>Color Preview</th>
              <th>Active Assets</th>
              <th>Ready Assets</th>
              <th>Trashed Assets</th>
            </tr>
          </thead>
          <tbody>
            <tr v-if="loadingFolders && !folders.length">
              <td colspan="5" class="text-center py-4 text-muted">Loading folders...</td>
            </tr>
            <tr v-else-if="!folders.length">
              <td colspan="5" class="text-center py-4 text-muted">No virtual folder entries found</td>
            </tr>
            <tr v-for="f in folders" :key="f.virtual_folder">
              <td>
                <span class="folder-path-cell font-bold mono">📁 {{ f.virtual_folder }}</span>
              </td>
              <td>
                <div v-if="f.color" class="color-badge-pill">
                  <span class="color-dot" :style="{ backgroundColor: f.color }"></span>
                  <span class="mono font-xs">{{ f.color }}</span>
                </div>
                <span v-else class="text-muted font-xs">Default</span>
              </td>
              <td class="mono font-sm">{{ f.asset_count }}</td>
              <td class="mono font-sm text-success">{{ f.ready_count }}</td>
              <td class="mono font-sm" :class="{ 'text-danger': f.trashed_count > 0 }">{{ f.trashed_count }}</td>
            </tr>
          </tbody>
        </table>
      </div>
    </div>

    <!-- 4. SCHEMA & STORAGE TAB -->
    <div v-if="activeSubTab === 'schema'" class="db-tab-content">
      <!-- Database Overview Cards -->
      <div class="overview-grid">
        <div class="stat-card panel">
          <div class="stat-label">TOTAL ASSETS</div>
          <div class="stat-value text-accent">{{ overview?.total_assets ?? '—' }}</div>
          <div class="stat-sub">{{ overview?.master_clips ?? 0 }} masters, {{ overview?.subclips ?? 0 }} subclips</div>
        </div>
        <div class="stat-card panel">
          <div class="stat-label">RECYCLE BIN</div>
          <div class="stat-value text-warning">{{ overview?.trashed_assets ?? '—' }}</div>
          <div class="stat-sub">Soft-deleted items</div>
        </div>
        <div class="stat-card panel">
          <div class="stat-label">DURABLE JOBS</div>
          <div class="stat-value">{{ overview?.total_jobs ?? '—' }}</div>
          <div class="stat-sub text-success">{{ overview?.completed_jobs ?? 0 }} ok, {{ overview?.failed_jobs ?? 0 }} failed</div>
        </div>
        <div class="stat-card panel">
          <div class="stat-label">DATABASE SIZE</div>
          <div class="stat-value mono">{{ formatBytes(overview?.db_size_bytes ?? 0) }}</div>
          <div class="stat-sub">Journal: <span class="badge badge-success">{{ overview?.wal_mode ? 'WAL' : 'Default' }}</span></div>
        </div>
      </div>

      <!-- Tables Schema Introspection -->
      <div class="schema-section">
        <div class="schema-tabs">
          <button
            v-for="t in schemaTables"
            :key="t.table_name"
            :class="['schema-tab-btn', { active: activeTable === t.table_name }]"
            @click="activeTable = t.table_name"
          >
            📋 {{ t.table_name }}
            <span class="count-badge">{{ t.row_count }} rows</span>
          </button>
        </div>

        <div v-if="selectedTableSchema" class="table-container panel">
          <table class="db-table">
            <thead>
              <tr>
                <th>Column Name</th>
                <th>SQLite Type</th>
                <th>Primary Key</th>
                <th>Not Null</th>
              </tr>
            </thead>
            <tbody>
              <tr v-for="c in selectedTableSchema.columns" :key="c.name">
                <td class="mono font-bold">{{ c.name }}</td>
                <td class="mono font-sm text-accent">{{ c.col_type || 'ANY' }}</td>
                <td>
                  <span v-if="c.is_pk" class="badge badge-warning">PK</span>
                  <span v-else class="text-muted">—</span>
                </td>
                <td>
                  <span v-if="c.not_null" class="badge badge-muted">NOT NULL</span>
                  <span v-else class="text-muted">NULL</span>
                </td>
              </tr>
            </tbody>
          </table>
        </div>
      </div>
    </div>

    <!-- DETAIL INSPECTION MODAL -->
    <div v-if="detailModalOpen" class="modal-overlay" @click.self="closeDetailModal">
      <div class="modal-card panel">
        <div class="modal-header">
          <h3>{{ detailModalTitle }}</h3>
          <button class="modal-close" @click="closeDetailModal">✕</button>
        </div>

        <div class="modal-body">
          <!-- Asset Inspection -->
          <template v-if="inspectingAsset">
            <div class="detail-grid">
              <div class="detail-field">
                <label>UUID</label>
                <div class="mono font-sm">{{ inspectingAsset.summary.uuid }}</div>
              </div>
              <div class="detail-field">
                <label>Display Name</label>
                <div class="font-sm">{{ inspectingAsset.summary.display_name }}</div>
              </div>
              <div class="detail-field">
                <label>Current Path</label>
                <div class="mono font-xs break-all">{{ inspectingAsset.summary.current_path }}</div>
              </div>
              <div class="detail-field">
                <label>Virtual Folder</label>
                <div class="font-sm">{{ inspectingAsset.summary.virtual_folder }}</div>
              </div>
              <div class="detail-field">
                <label>Duration &amp; Frames</label>
                <div class="mono font-sm">{{ inspectingAsset.summary.duration_ms }} ms ({{ inspectingAsset.summary.total_frames }} frames)</div>
              </div>
              <div class="detail-field">
                <label>FPS Rational</label>
                <div class="mono font-sm">{{ inspectingAsset.summary.fps_num }} / {{ inspectingAsset.summary.fps_den }} ({{ inspectingAsset.summary.fps }})</div>
              </div>
              <div class="detail-field">
                <label>GOP Frames &amp; Safe Start</label>
                <div class="mono font-sm">GOP: {{ inspectingAsset.summary.gop_frames }} | Keyframe Safe: {{ inspectingAsset.summary.keyframe_safe_start_ms }} ms</div>
              </div>
              <div class="detail-field">
                <label>Mezzanine Verified</label>
                <div>
                  <span v-if="inspectingAsset.summary.mezzanine_ok" class="badge badge-success">✓ Verified Compliant</span>
                  <span v-else class="badge badge-warning">⚠ Non-Mezzanine</span>
                </div>
              </div>
            </div>

            <!-- Warnings section -->
            <div class="detail-block">
              <label>Validation Warnings</label>
              <div v-if="inspectingAsset.summary.warnings.length" class="warnings-box">
                <div v-for="(w, idx) in inspectingAsset.summary.warnings" :key="idx" class="warning-item">
                  ⚠ {{ w }}
                </div>
              </div>
              <div v-else class="text-muted font-sm">No validation warnings recorded</div>
            </div>

            <!-- Keyframes sample -->
            <div class="detail-block">
              <label>Keyframe PTS Sample (First {{ inspectingAsset.keyframe_sample.length }} points)</label>
              <div class="mono font-xs keyframe-box">
                {{ inspectingAsset.keyframe_sample.join(', ') || 'No keyframes recorded' }}
              </div>
            </div>
          </template>

          <!-- Job Inspection -->
          <template v-if="inspectingJob">
            <div class="detail-grid">
              <div class="detail-field">
                <label>Job ID</label>
                <div class="mono font-sm">{{ inspectingJob.summary.id }}</div>
              </div>
              <div class="detail-field">
                <label>Target Asset UUID</label>
                <div class="mono font-sm">{{ inspectingJob.summary.uuid || '—' }}</div>
              </div>
              <div class="detail-field">
                <label>Input File</label>
                <div class="mono font-xs break-all">{{ inspectingJob.summary.input_path }}</div>
              </div>
              <div class="detail-field">
                <label>Output File</label>
                <div class="mono font-xs break-all">{{ inspectingJob.summary.output_path || '—' }}</div>
              </div>
              <div class="detail-field">
                <label>State / Phase / Stage</label>
                <div class="font-sm">{{ inspectingJob.summary.state }} / {{ inspectingJob.summary.phase }} / {{ inspectingJob.summary.current_stage }}</div>
              </div>
              <div class="detail-field">
                <label>Attempts</label>
                <div class="mono font-sm">{{ inspectingJob.summary.attempt }} of {{ inspectingJob.summary.max_attempts }}</div>
              </div>
              <div class="detail-field">
                <label>Error Category</label>
                <div class="font-sm" :class="{ 'text-danger': inspectingJob.summary.error_category }">
                  {{ inspectingJob.summary.error_category || 'None' }}
                </div>
              </div>
              <div class="detail-field">
                <label>Lease / Worker ID</label>
                <div class="mono font-xs">{{ inspectingJob.summary.worker_id || 'unassigned' }}</div>
              </div>
            </div>

            <div v-if="inspectingJob.summary.error" class="detail-block">
              <label class="text-danger font-bold">Error Details</label>
              <div class="error-detail-box mono font-xs">{{ inspectingJob.summary.error }}</div>
            </div>

            <div class="detail-block">
              <label>FFmpeg STDERR Log Tail (Last {{ inspectingJob.stderr_log_tail.length }} lines)</label>
              <div class="terminal-log-box mono font-xs">
                <div v-if="!inspectingJob.stderr_log_tail.length" class="text-muted">No stderr log tail available</div>
                <div v-for="(line, idx) in inspectingJob.stderr_log_tail" :key="idx" class="log-line">{{ line }}</div>
              </div>
            </div>
          </template>
        </div>
      </div>
    </div>
  </div>
</template>

<script setup lang="ts">
import { ref, computed, onMounted } from 'vue'

interface DbOverview {
  total_assets: number
  master_clips: number
  subclips: number
  trashed_assets: number
  ready_assets: number
  processing_assets: number
  error_assets: number
  total_jobs: number
  pending_jobs: number
  processing_jobs: number
  completed_jobs: number
  failed_jobs: number
  cancelled_jobs: number
  db_size_bytes: number
  wal_mode: boolean
}

interface DbAssetSummary {
  uuid: string
  fingerprint: number
  current_path: string
  display_path: string
  duration_ms: number
  trim_in_ms: number
  trim_out_ms: number
  rating: string
  tp: string
  status: string
  display_name: string
  virtual_folder: string
  original_virtual_folder?: string
  mezzanine_ok: boolean
  fps: number
  fps_num: number
  fps_den: number
  total_frames: number
  gop_frames: number
  keyframe_safe_start_ms: number
  keyframe_count: number
  warnings: string[]
  is_subclip: boolean
  parent_uuid?: string
  deleted_at?: string
  sidecar_exists: boolean
}

interface DbAssetsPage {
  items: DbAssetSummary[]
  total: number
  limit: number
  offset: number
}

interface DbAssetDetail {
  summary: DbAssetSummary
  keyframe_sample: number[]
  warnings_json: string
  keyframe_offsets_json: string
}

interface DbJobSummary {
  id: string
  input_path: string
  input_path_display: string
  output_path?: string
  output_path_display?: string
  profile: string
  uuid?: string
  state: string
  phase: string
  progress: number
  current_stage: string
  duration_secs: number
  error?: string
  error_category?: string
  attempt: number
  max_attempts: number
  created_at: string
  started_at?: string
  finished_at?: string
  worker_id?: string
  encode_fps: number
  encode_bitrate: string
  encode_speed: string
  stderr_lines_count: number
}

interface DbJobsPage {
  items: DbJobSummary[]
  total: number
  limit: number
  offset: number
}

interface DbJobDetail {
  summary: DbJobSummary
  stderr_log_tail: string[]
  fingerprint?: number
  request_hash?: string
  leased_until?: string
  heartbeat_at?: string
  cancel_requested: boolean
}

interface DbFolderItem {
  virtual_folder: string
  color?: string
  asset_count: number
  ready_count: number
  trashed_count: number
}

interface DbTableColumnInfo {
  cid: number
  name: string
  col_type: string
  not_null: boolean
  is_pk: boolean
}

interface DbTableSchema {
  table_name: string
  row_count: number
  columns: DbTableColumnInfo[]
}

const activeSubTab = ref<'assets' | 'jobs' | 'folders' | 'schema'>('assets')
const loading = ref(false)

const overview = ref<DbOverview | null>(null)

// 1. Assets
const assetFilter = ref('all')
const assetSearch = ref('')
const assetLimit = ref(25)
const assetOffset = ref(0)
const loadingAssets = ref(false)
const assetsPage = ref<DbAssetsPage>({ items: [], total: 0, limit: 25, offset: 0 })

const assetFilters = [
  { id: 'all', label: 'All' },
  { id: 'master', label: 'Master Clips' },
  { id: 'subclip', label: 'Subclips' },
  { id: 'ready', label: 'Ready' },
  { id: 'processing', label: 'Processing' },
  { id: 'error', label: 'Error' },
  { id: 'trashed', label: 'Recycle Bin' },
]

// 2. Jobs
const jobStateFilter = ref('all')
const jobSearch = ref('')
const jobLimit = ref(25)
const jobOffset = ref(0)
const loadingJobs = ref(false)
const jobsPage = ref<DbJobsPage>({ items: [], total: 0, limit: 25, offset: 0 })

const jobStateFilters = [
  { id: 'all', label: 'All Jobs' },
  { id: 'Pending', label: 'Pending' },
  { id: 'Processing', label: 'Processing' },
  { id: 'Completed', label: 'Completed' },
  { id: 'Failed', label: 'Failed' },
  { id: 'Cancelled', label: 'Cancelled' },
]

// 3. Folders
const loadingFolders = ref(false)
const folders = ref<DbFolderItem[]>([])

// 4. Schema
const schemaTables = ref<DbTableSchema[]>([])
const activeTable = ref('media_assets')

// Inspection modal
const detailModalOpen = ref(false)
const inspectingAsset = ref<DbAssetDetail | null>(null)
const inspectingJob = ref<DbJobDetail | null>(null)

const subTabs = computed(() => [
  { id: 'assets' as const, label: 'Clips & Subclips', count: overview.value?.total_assets },
  { id: 'jobs' as const, label: 'Jobs History', count: overview.value?.total_jobs },
  { id: 'folders' as const, label: 'Virtual Folders', count: folders.value.length || undefined },
  { id: 'schema' as const, label: 'Database & Schema' },
])

const assetStartRecord = computed(() => (assetsPage.value.total === 0 ? 0 : assetOffset.value + 1))
const assetEndRecord = computed(() => Math.min(assetOffset.value + assetLimit.value, assetsPage.value.total))

const jobStartRecord = computed(() => (jobsPage.value.total === 0 ? 0 : jobOffset.value + 1))
const jobEndRecord = computed(() => Math.min(jobOffset.value + jobLimit.value, jobsPage.value.total))

const selectedTableSchema = computed(() => schemaTables.value.find((t) => t.table_name === activeTable.value))

const detailModalTitle = computed(() => {
  if (inspectingAsset.value) return `Asset Details: ${inspectingAsset.value.summary.display_name}`
  if (inspectingJob.value) return `Job Details: ${inspectingJob.value.summary.id}`
  return 'Record Inspection'
})

// Debounce timer
let searchTimeout: any = null

function debouncedFetchAssets() {
  clearTimeout(searchTimeout)
  searchTimeout = setTimeout(() => {
    assetOffset.value = 0
    fetchAssets()
  }, 300)
}

function debouncedFetchJobs() {
  clearTimeout(searchTimeout)
  searchTimeout = setTimeout(() => {
    jobOffset.value = 0
    fetchJobs()
  }, 300)
}

function clearAssetSearch() {
  assetSearch.value = ''
  assetOffset.value = 0
  fetchAssets()
}

function clearJobSearch() {
  jobSearch.value = ''
  jobOffset.value = 0
  fetchJobs()
}

function setAssetFilter(filter: string) {
  assetFilter.value = filter
  assetOffset.value = 0
  fetchAssets()
}

function setJobStateFilter(state: string) {
  jobStateFilter.value = state
  jobOffset.value = 0
  fetchJobs()
}

function prevAssetPage() {
  assetOffset.value = Math.max(0, assetOffset.value - assetLimit.value)
  fetchAssets()
}

function nextAssetPage() {
  if (assetOffset.value + assetLimit.value < assetsPage.value.total) {
    assetOffset.value += assetLimit.value
    fetchAssets()
  }
}

function prevJobPage() {
  jobOffset.value = Math.max(0, jobOffset.value - jobLimit.value)
  fetchJobs()
}

function nextJobPage() {
  if (jobOffset.value + jobLimit.value < jobsPage.value.total) {
    jobOffset.value += jobLimit.value
    fetchJobs()
  }
}

// Data Fetching
async function fetchOverview() {
  try {
    const res = await fetch('/api/v2/db/overview')
    if (res.ok) {
      overview.value = await res.json()
    }
  } catch (err) {
    console.error('Failed to fetch DB overview:', err)
  }
}

async function fetchAssets() {
  loadingAssets.value = true
  try {
    const params = new URLSearchParams({
      filter: assetFilter.value,
      search: assetSearch.value.trim(),
      limit: String(assetLimit.value),
      offset: String(assetOffset.value),
    })
    const res = await fetch(`/api/v2/db/assets?${params}`)
    if (res.ok) {
      assetsPage.value = await res.json()
    }
  } catch (err) {
    console.error('Failed to fetch DB assets:', err)
  } finally {
    loadingAssets.value = false
  }
}

async function fetchJobs() {
  loadingJobs.value = true
  try {
    const params = new URLSearchParams({
      state: jobStateFilter.value,
      search: jobSearch.value.trim(),
      limit: String(jobLimit.value),
      offset: String(jobOffset.value),
    })
    const res = await fetch(`/api/v2/db/jobs?${params}`)
    if (res.ok) {
      jobsPage.value = await res.json()
    }
  } catch (err) {
    console.error('Failed to fetch DB jobs:', err)
  } finally {
    loadingJobs.value = false
  }
}

async function fetchFolders() {
  loadingFolders.value = true
  try {
    const res = await fetch('/api/v2/db/folders')
    if (res.ok) {
      folders.value = await res.json()
    }
  } catch (err) {
    console.error('Failed to fetch DB folders:', err)
  } finally {
    loadingFolders.value = false
  }
}

async function fetchSchema() {
  try {
    const res = await fetch('/api/v2/db/schema')
    if (res.ok) {
      schemaTables.value = await res.json()
    }
  } catch (err) {
    console.error('Failed to fetch DB schema:', err)
  }
}

async function inspectAsset(uuid: string) {
  try {
    const res = await fetch(`/api/v2/db/assets/${uuid}`)
    if (res.ok) {
      inspectingAsset.value = await res.json()
      inspectingJob.value = null
      detailModalOpen.value = true
    }
  } catch (err) {
    console.error('Failed to inspect asset:', err)
  }
}

async function inspectJob(id: string) {
  try {
    const res = await fetch(`/api/v2/db/jobs/${id}`)
    if (res.ok) {
      inspectingJob.value = await res.json()
      inspectingAsset.value = null
      detailModalOpen.value = true
    }
  } catch (err) {
    console.error('Failed to inspect job:', err)
  }
}

function closeDetailModal() {
  detailModalOpen.value = false
  inspectingAsset.value = null
  inspectingJob.value = null
}

async function refreshCurrent() {
  loading.value = true
  await Promise.all([fetchOverview(), fetchAssets(), fetchJobs(), fetchFolders(), fetchSchema()])
  loading.value = false
}

// Helpers
function formatMs(ms: number): string {
  if (!ms || ms <= 0) return '00:00.000'
  const totalSecs = Math.floor(ms / 1000)
  const mins = Math.floor(totalSecs / 60)
  const secs = totalSecs % 60
  const msec = ms % 1000
  return `${String(mins).padStart(2, '0')}:${String(secs).padStart(2, '0')}.${String(msec).padStart(3, '0')}`
}

function formatIso(iso?: string): string {
  if (!iso) return '—'
  const d = new Date(iso)
  return d.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit', second: '2-digit' })
}

function formatBytes(bytes: number): string {
  if (!bytes || bytes === 0) return '0 B'
  const k = 1024
  const sizes = ['B', 'KB', 'MB', 'GB', 'TB']
  const i = Math.floor(Math.log(bytes) / Math.log(k))
  return parseFloat((bytes / Math.pow(k, i)).toFixed(2)) + ' ' + sizes[i]
}

function statusBadgeClass(status: string, deletedAt?: string): string {
  if (deletedAt) return 'badge-danger'
  switch (status.toLowerCase()) {
    case 'ready':
      return 'badge-success'
    case 'processing':
      return 'badge-warning'
    case 'error':
      return 'badge-danger'
    default:
      return 'badge-muted'
  }
}

function jobStateBadgeClass(state: string): string {
  switch (state.toLowerCase()) {
    case 'completed':
      return 'badge-success'
    case 'processing':
      return 'badge-warning'
    case 'failed':
      return 'badge-danger'
    case 'cancelled':
      return 'badge-muted'
    default:
      return 'badge-info'
  }
}

onMounted(() => {
  refreshCurrent()
})
</script>

<style scoped>
.db-viewer {
  display: flex;
  flex-direction: column;
  gap: 16px;
}

.db-header-bar {
  display: flex;
  justify-content: space-between;
  align-items: center;
  border-bottom: 1px solid var(--border-subtle);
  padding-bottom: 12px;
}

.db-tabs {
  display: flex;
  gap: 6px;
}

.db-tab-btn {
  background: rgba(255, 255, 255, 0.03);
  border: 1px solid var(--border-subtle);
  border-radius: 6px;
  color: var(--text-secondary);
  font-size: 13px;
  font-weight: 500;
  padding: 6px 14px;
  cursor: pointer;
  display: flex;
  align-items: center;
  gap: 8px;
  transition: all 0.15s ease;
}

.db-tab-btn:hover {
  background: rgba(255, 255, 255, 0.08);
  color: var(--text-primary);
}

.db-tab-btn.active {
  background: var(--accent-cyan);
  border-color: var(--accent-cyan);
  color: #000;
  font-weight: 700;
}

.count-badge {
  background: rgba(0, 0, 0, 0.25);
  padding: 1px 6px;
  border-radius: 10px;
  font-size: 11px;
}

.db-tab-btn.active .count-badge {
  background: rgba(0, 0, 0, 0.15);
  color: #000;
}

.filter-toolbar {
  display: flex;
  justify-content: space-between;
  align-items: center;
  gap: 16px;
  flex-wrap: wrap;
}

.filter-pills {
  display: flex;
  gap: 6px;
  flex-wrap: wrap;
}

.filter-pill {
  background: rgba(255, 255, 255, 0.03);
  border: 1px solid var(--border-subtle);
  border-radius: 20px;
  color: var(--text-secondary);
  font-size: 12px;
  padding: 4px 12px;
  cursor: pointer;
  transition: all 0.15s ease;
}

.filter-pill:hover {
  background: rgba(255, 255, 255, 0.07);
  color: var(--text-primary);
}

.filter-pill.active {
  background: rgba(45, 212, 191, 0.15);
  border-color: var(--accent-cyan);
  color: var(--accent-cyan);
  font-weight: 600;
}

.search-box {
  position: relative;
  display: flex;
  align-items: center;
  min-width: 280px;
}

.search-input {
  width: 100%;
  padding-right: 28px;
}

.search-clear {
  position: absolute;
  right: 8px;
  background: none;
  border: none;
  color: var(--text-secondary);
  cursor: pointer;
  font-size: 12px;
}

.table-container {
  overflow-x: auto;
  border-radius: 8px;
}

.db-table {
  width: 100%;
  border-collapse: collapse;
  text-align: left;
  font-size: 13px;
}

.db-table th {
  background: rgba(255, 255, 255, 0.03);
  border-bottom: 1px solid var(--border-subtle);
  padding: 10px 12px;
  font-weight: 600;
  font-size: 11px;
  text-transform: uppercase;
  color: var(--text-secondary);
  letter-spacing: 0.5px;
}

.db-table td {
  padding: 8px 12px;
  border-bottom: 1px solid var(--border-subtle);
  vertical-align: middle;
}

.db-table tr:hover td {
  background: rgba(255, 255, 255, 0.02);
}

.row-trashed td {
  opacity: 0.65;
  background: rgba(229, 72, 77, 0.03);
}

.asset-title-cell {
  display: flex;
  flex-direction: column;
  max-width: 260px;
}

.asset-name {
  font-weight: 600;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}

.uuid-text {
  font-size: 10px;
  color: var(--text-secondary);
}

.path-cell {
  max-width: 240px;
}

.input-path,
.output-path {
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}

.badge {
  display: inline-block;
  padding: 2px 7px;
  border-radius: 4px;
  font-size: 11px;
  font-weight: 600;
}

.badge-master {
  background: rgba(45, 212, 191, 0.15);
  color: var(--accent-cyan);
  border: 1px solid rgba(45, 212, 191, 0.3);
}

.badge-subclip {
  background: rgba(245, 166, 35, 0.15);
  color: var(--accent-amber);
  border: 1px solid rgba(245, 166, 35, 0.3);
}

.badge-rating {
  background: rgba(255, 255, 255, 0.08);
  color: var(--text-primary);
  margin-right: 4px;
}

.badge-tp {
  background: rgba(229, 72, 77, 0.2);
  color: var(--accent-crimson);
}

.badge-profile {
  background: rgba(255, 255, 255, 0.06);
  color: var(--text-secondary);
}

.badge-success {
  background: rgba(63, 185, 80, 0.15);
  color: var(--accent-emerald);
}

.badge-warning {
  background: rgba(245, 166, 35, 0.15);
  color: var(--accent-amber);
}

.badge-danger {
  background: rgba(229, 72, 77, 0.15);
  color: var(--accent-crimson);
}

.badge-info {
  background: rgba(45, 212, 191, 0.15);
  color: var(--accent-cyan);
}

.badge-muted {
  background: rgba(255, 255, 255, 0.05);
  color: var(--text-secondary);
}

.folder-badge {
  background: rgba(255, 255, 255, 0.04);
  padding: 2px 6px;
  border-radius: 4px;
  font-size: 11px;
  font-family: monospace;
}

.state-progress-cell {
  display: flex;
  align-items: center;
  gap: 6px;
}

.mini-progress-track {
  width: 50px;
  height: 6px;
  background: rgba(255, 255, 255, 0.08);
  border-radius: 3px;
  overflow: hidden;
}

.mini-progress-fill {
  height: 100%;
  background: var(--accent-cyan);
}

.error-cell {
  display: flex;
  flex-direction: column;
  gap: 2px;
  max-width: 200px;
}

.error-text {
  color: var(--accent-crimson);
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}

.pagination-bar {
  display: flex;
  justify-content: space-between;
  align-items: center;
  padding: 8px 4px;
}

.pagination-controls {
  display: flex;
  align-items: center;
  gap: 8px;
}

.select-sm {
  background: rgba(255, 255, 255, 0.04);
  border: 1px solid var(--border-subtle);
  border-radius: 4px;
  color: var(--text-primary);
  font-size: 12px;
  padding: 3px 6px;
}

.overview-grid {
  display: grid;
  grid-template-columns: repeat(auto-fit, minmax(220px, 1fr));
  gap: 16px;
}

.stat-card {
  padding: 16px;
  display: flex;
  flex-direction: column;
  gap: 6px;
}

.stat-label {
  font-size: 11px;
  text-transform: uppercase;
  color: var(--text-secondary);
  letter-spacing: 0.5px;
}

.stat-value {
  font-size: 26px;
  font-weight: 700;
  line-height: 1.2;
}

.stat-sub {
  font-size: 12px;
  color: var(--text-secondary);
}

.schema-section {
  display: flex;
  flex-direction: column;
  gap: 12px;
  margin-top: 16px;
}

.schema-tabs {
  display: flex;
  gap: 8px;
}

.schema-tab-btn {
  background: rgba(255, 255, 255, 0.03);
  border: 1px solid var(--border-subtle);
  border-radius: 6px;
  color: var(--text-secondary);
  font-size: 13px;
  padding: 6px 14px;
  cursor: pointer;
  display: flex;
  align-items: center;
  gap: 8px;
}

.schema-tab-btn.active {
  background: rgba(45, 212, 191, 0.15);
  border-color: var(--accent-cyan);
  color: var(--accent-cyan);
  font-weight: 600;
}

.color-badge-pill {
  display: flex;
  align-items: center;
  gap: 6px;
}

.color-dot {
  width: 10px;
  height: 10px;
  border-radius: 50%;
}

.modal-overlay {
  position: fixed;
  top: 0;
  left: 0;
  width: 100vw;
  height: 100vh;
  background: rgba(0, 0, 0, 0.75);
  backdrop-filter: blur(4px);
  display: flex;
  justify-content: center;
  align-items: center;
  z-index: 1000;
}

.modal-card {
  width: 90%;
  max-width: 800px;
  max-height: 85vh;
  display: flex;
  flex-direction: column;
  border-radius: 8px;
  background: var(--bg-panel);
  border: 1px solid var(--border-base);
  box-shadow: 0 16px 40px rgba(0, 0, 0, 0.6);
}

.modal-header {
  display: flex;
  justify-content: space-between;
  align-items: center;
  padding: 16px 20px;
  border-bottom: 1px solid var(--border-subtle);
}

.modal-close {
  background: none;
  border: none;
  color: var(--text-secondary);
  font-size: 18px;
  cursor: pointer;
}

.modal-body {
  padding: 20px;
  overflow-y: auto;
  display: flex;
  flex-direction: column;
  gap: 16px;
}

.detail-grid {
  display: grid;
  grid-template-columns: 1fr 1fr;
  gap: 12px;
}

.detail-field {
  display: flex;
  flex-direction: column;
  gap: 4px;
}

.detail-field label,
.detail-block label {
  font-size: 11px;
  color: var(--text-secondary);
  text-transform: uppercase;
  letter-spacing: 0.5px;
}

.detail-block {
  display: flex;
  flex-direction: column;
  gap: 6px;
}

.warnings-box {
  background: rgba(245, 166, 35, 0.08);
  border: 1px solid rgba(245, 166, 35, 0.2);
  border-radius: 6px;
  padding: 8px 12px;
}

.warning-item {
  color: var(--accent-amber);
  font-size: 12px;
}

.keyframe-box,
.error-detail-box,
.terminal-log-box {
  background: #08090a;
  border: 1px solid var(--border-subtle);
  border-radius: 6px;
  padding: 10px;
  max-height: 200px;
  overflow-y: auto;
}

.terminal-log-box {
  color: #a6accd;
}

.terminal-log-box .log-line {
  padding: 1px 0;
  white-space: pre-wrap;
  word-break: break-all;
}

.btn-xs {
  padding: 3px 8px;
  font-size: 11px;
}

.btn-sm {
  padding: 5px 12px;
  font-size: 12px;
}

.spinner-sm {
  display: inline-block;
  width: 12px;
  height: 12px;
  border: 2px solid rgba(255, 255, 255, 0.2);
  border-top-color: var(--text-primary);
  border-radius: 50%;
  animation: spin 0.8s linear infinite;
  margin-right: 4px;
}

@keyframes spin {
  to {
    transform: rotate(360deg);
  }
}

.mono {
  font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace;
}

.font-sm {
  font-size: 12px;
}

.font-xs {
  font-size: 11px;
}

.font-bold {
  font-weight: 700;
}

.break-all {
  word-break: break-all;
}

.text-accent {
  color: var(--accent-cyan);
}

.text-warning {
  color: var(--accent-amber);
}

.text-danger {
  color: var(--accent-crimson);
}

.text-success {
  color: var(--accent-emerald);
}

.text-muted {
  color: var(--text-secondary);
}

.text-right {
  text-align: right;
}

.text-center {
  text-align: center;
}

.py-4 {
  padding-top: 16px;
  padding-bottom: 16px;
}
</style>

// The JSON the daemon speaks, as the dashboard reads it. Written against `docs/mcp-and-api.md` and the Rust types
// they serialise (`app::StatusReport`, `domain::Job`, ...): a field the dashboard does not show is not listed.

/** `X-MemCastle-Mode`: what the dashboard may do to memory. `disabled` is for agent sessions and not offered here. */
export type MemoryMode = "full" | "read_only"

export interface DatastoreStatus {
  ok: boolean
  backend: string
  location: string
  error: string | null
  migration_version: number
  latest_version: number
  pending: string[]
}

export interface StatusReport {
  version: string
  uptime_secs: number
  palace_name: string
  drawer_count: number
  jobs_queued: number
  jobs_running: number
  jobs_paused: number
  mode: string
  pid: number
  started_at: string
  bind_addr: string
  palace_path: string
  datastore: DatastoreStatus
  auth_enabled: boolean
}

export interface ConfigReport {
  bind_addr: string
  palace_path: string
  backend: string
  location: string
  auth_enabled: boolean
  web: { enabled: boolean; built: boolean }
  assets: { source: "override" | "installed" | "embedded"; root: string | null }
  jobs: { max_concurrency: number; drain_timeout_secs: number; lease_ttl_secs: number }
  embeddings: { provider: string; model: string | null }
  extraction: { provider: string; model: string | null }
  mining: { chunk_chars: number; max_documents: number; registries: number; dedup_enabled: boolean }
}

export type JobStatus = "queued" | "running" | "paused" | "completed" | "failed" | "cancelled"

export const JOB_STATUSES: readonly JobStatus[] = ["queued", "running", "paused", "completed", "failed", "cancelled"]

/** A job's kind is the `type`-tagged object the daemon stores; the rest of its fields depend on the type. */
export interface JobKind {
  type: string
  [field: string]: unknown
}

export interface JobProgress {
  current: number
  total: number | null
  message: string | null
}

export interface Job {
  id: string
  kind: JobKind
  status: JobStatus
  priority: number
  created_at: string
  started_at: string | null
  completed_at: string | null
  requested_by: string
  progress: JobProgress
  attempt: number
  max_attempts: number
  result: unknown
  error: string | null
}

export type JobControl = "pause" | "resume" | "cancel" | "retry"

export interface Wing {
  id: string
  name: string
  description: string | null
  created_at: string
  rooms: number
  drawers: number
}

export interface Room {
  id: string
  name: string
  wing_name: string
  description: string | null
  created_at: string
  drawers: number
}

export interface WingDetail {
  wing: Wing
  rooms: Room[]
}

export interface DrawerSource {
  kind: string
  uri: string | null
  agent: string | null
  origin?: { source: string; document: string; chunk: number }
}

export interface DrawerSummary {
  id: string
  name: string | null
  chars: number
  preview: string
  source: DrawerSource
  created_at: string
}

export interface Drawer {
  id: string
  name: string | null
  content: string
  source: DrawerSource
  tags: string[]
  provenance: { requested_by: string; job_id: string | null }
  valid_from: string
  valid_to: string | null
  supersedes?: string
  superseded_by?: string
  created_at: string
  updated_at: string
}

export interface DrawerHistory {
  drawer: string
  versions: Drawer[]
}

export interface SimilarDrawer {
  drawer: string
  side: string
  kind: string
  similarity: number
}

export interface SearchHit extends Drawer {
  score: number
  signals?: { lexical?: number; semantic?: number; graph?: number }
  via?: string[]
}

export type Ranking = "auto" | "lexical" | "semantic" | "hybrid"

export interface SearchOptions {
  q: string
  limit?: number
  wing?: string
  room?: string
  ranking?: Ranking
  tags?: string
  as_of?: string
  include_historical?: boolean
  expand?: boolean
}

export interface Entity {
  id: string
  name: string
  kind: string
  aliases: string[]
}

export interface Relationship {
  id: string
  from: string
  to: string
  predicate: string
  confidence: number
  valid_from: string
  valid_to: string | null
  provenance?: { drawer: string; extractor: string; extracted_at: string; origin?: { document: string } }
}

export interface Mention {
  drawer: string
  created_at: string
  provenance?: { drawer: string; extractor: string }
  observation?: { name: string; confidence: number }
}

export interface PossibleEntity {
  entity: string
  [field: string]: unknown
}

export interface GraphView {
  nodes: Entity[]
  edges: Relationship[]
  truncated: boolean
}

export interface ErrorBody {
  code?: string
  help?: string
  error?: string
}

/** What a job submission may contain: the `type`-tagged body of `POST /api/jobs`. */
export type JobRequest =
  | { type: "mine"; path: string; wing?: string; full?: boolean }
  | { type: "mine"; source: string; locator?: string; wing?: string; full?: boolean }
  | { type: "audit"; wing?: string }
  | { type: "repair"; dry_run: boolean; based_on_job?: string }
  | { type: "embed"; wing?: string }
  | { type: "extract"; wing?: string }

export interface AdapterInfo {
  name: string
  description: string
  state: string
  version?: string
}

export interface SourcesReport {
  adapters: AdapterInfo[]
  sources: { id: string; source: string; locator: string; documents: number; last_run_at: string | null }[]
}

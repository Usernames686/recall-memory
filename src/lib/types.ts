export type Provider = "codex" | "claude-code"
export type EntryKind = "meta" | "skill" | "revision"
export type EntryStatus = "active" | "pending" | "disabled" | "rejected"
export type ContextMode = "mcp" | "guided"
export type AgentMode = "reflection" | "verification"
export type ModelProvider = "remote" | "ollama"

export interface SourceSummary {
  provider: Provider
  root: string
  available: boolean
  sessionCount: number
  activityCount: number
  error?: string
  lastScannedAt?: number
  errorCount: number
  cursorCount: number
}

export interface SessionSummary {
  id: string
  provider: Provider
  title: string
  sourcePath: string
  cwd?: string
  activityCount: number
  updatedAt: number
}

export interface Activity {
  id: string
  provider: Provider
  sessionId: string
  sourcePath: string
  kind: string
  role: string
  text: string
  occurredAt: number
  metadata: Record<string, unknown> | null
}

export interface EvolutionEntry {
  id: string
  kind: EntryKind
  title: string
  summary: string
  body: string
  status: EntryStatus
  risk: "low" | "high" | "review"
  sourceRefs: string[]
  updatedAt: number
  originRunId?: string
  targetEntryId?: string
  version: number
}

export type EvolutionRunMode = "manual" | "listener" | "scheduled"
export type EvolutionPhase = "idle" | "listening" | "queued" | "scanning" | "reading" | "analyzing" | "validating" | "persisting" | "cancelling" | "cancelled" | "interrupted" | "completed" | "failed"

export interface EvolutionSettings {
  enabled: boolean
  codexEnabled: boolean
  claudeEnabled: boolean
  lookbackDays: 1 | 7 | 30
  runMode: EvolutionRunMode
  scheduleHours: number
  listenSince?: number
  autoActivateLowRisk: boolean
  maxAgentSteps: number
  launchAtLogin: boolean
  notificationsEnabled: boolean
  agentMode: AgentMode
  codexSourcePath: string
  claudeSourcePath: string
}

export interface EvolutionRunState {
  runId: string
  mode: EvolutionRunMode
  phase: EvolutionPhase
  startedAt: number
  completedAt?: number
  scannedActivities: number
  consumedActivities: number
  generated: number
  activated: number
  pending: number
  error?: string
  model?: string
  providers: Provider[]
  lookbackDays: number
  rolledBackAt?: number
  agentMode: AgentMode
  traceCount: number
  verificationStatus: "not_run" | "passed" | "review_required" | "failed" | string
  verificationSummary?: string
  retryOfRunId?: string
  providerUsed?: string
  fallbackCount: number
  inputActivityCount: number
  inputTokens: number
  outputTokens: number
  durationMs: number
  estimatedCostUsd?: number
}

export interface AgentTraceEvent {
  id: number
  runId: string
  occurredAt: number
  phase: EvolutionPhase
  eventType: string
  toolName?: string
  summary: string
  durationMs?: number
  resultStatus: string
  errorCode?: string
}

export interface EvolutionRunDetail {
  run: EvolutionRunState
  activities: Activity[]
  entries: EvolutionEntry[]
  traces: AgentTraceEvent[]
  candidateVerifications: CandidateVerification[]
}

export interface CandidateVerification {
  runId: string
  entryId: string
  evidenceSufficient: boolean
  supportingEvidence: string[]
  contradictingEvidence: string[]
  confidence: number
  duplicate: boolean
  conflict: boolean
  recommendation: "approve" | "review" | "reject"
  rationale: string
}

export interface ReflectionConfig {
  provider: ModelProvider
  baseUrl: string
  model: string
  hasApiKey: boolean
  contextMode: ContextMode
  timeoutSeconds: number
  fallbackEnabled: boolean
  fallbackBaseUrl: string
  fallbackModel: string
  fallbackTimeoutSeconds: number
  inputPricePerMillionUsd: number
  outputPricePerMillionUsd: number
  healthStatus: "unknown" | "checking" | "ok" | "error"
  healthError?: string
  lastCheckedAt?: number
}

export interface ReflectionConfigInput {
  provider: ModelProvider
  baseUrl: string
  model: string
  apiKey?: string
  contextMode: ContextMode
  timeoutSeconds: number
  fallbackEnabled: boolean
  fallbackBaseUrl: string
  fallbackModel: string
  fallbackTimeoutSeconds: number
  inputPricePerMillionUsd: number
  outputPricePerMillionUsd: number
}

export interface Snapshot {
  consentGranted: boolean
  sources: SourceSummary[]
  sessions: SessionSummary[]
  activities: Activity[]
  runActivities: Activity[]
  entries: EvolutionEntry[]
  pendingCount: number
  activityCount: number
  dirtyCount: number
  lastReflectionAt?: number
  config: ReflectionConfig
  evolution: EvolutionSettings
  run?: EvolutionRunState
  runHistory: EvolutionRunState[]
  storeStats: StoreStats
  redactionReport: RedactionReport
  cacheCleanupPreview: CacheCleanupPreview
  backups: StoreBackup[]
  auditEvents: AuditEvent[]
  mcp: {
    codex: boolean
    claude: boolean
    lastChecked?: number
    healthStatus?: "unknown" | "ok" | "error"
    healthError?: string
    recentCalls: McpCallSummary[]
  }
  recoveryNotice?: string
}

export interface McpCallSummary {
  id: number
  occurredAt: number
  toolName: string
  action?: string
  resultStatus: string
}

export interface EntryVersion {
  id: number
  entryId: string
  version: number
  kind: EntryKind
  title: string
  summary: string
  body: string
  status: EntryStatus
  risk: "low" | "high" | "review"
  sourceRefs: string[]
  originRunId?: string
  targetEntryId?: string
  createdAt: number
  action: string
  sourceRunId?: string
  reviewer?: string
  reviewReason?: string
  reviewedAt?: number
}

export interface EntryVersionDiff {
  entryId: string
  fromVersion?: number
  toVersion: number
  oldBody: string
  newBody: string
  oldSummary: string
  newSummary: string
  changed: boolean
}

export interface StoreStats {
  databasePath: string
  databaseBytes: number
  entryCount: number
  activeCount: number
  pendingCount: number
  versionCount: number
  activityCount: number
  reflectedActivityCount: number
  runCount: number
  auditCount: number
}

export interface AuditEvent {
  id: number
  occurredAt: number
  action: string
  objectId?: string
  detail: Record<string, unknown> | null
}

export interface MaintenanceResult {
  path?: string
  affected: number
  message: string
}

export interface RedactionReport {
  processedRecords: number
  redactedRecords: number
  redactionCount: number
  categories: Array<{ category: string; count: number }>
}

export interface CacheCleanupPreview {
  reflectedActivities: number
  runActivityLinks: number
  affectedRuns: number
  preservedEntries: number
  preservedVersions: number
}

export interface RunRollbackResult {
  runId: string
  disabledEntries: number
  restoredEntries: number
  message: string
}

export interface StoreBackup {
  fileName: string
  path: string
  bytes: number
  createdAt: number
}

export interface ScanResult {
  newActivities: number
  scannedSessions: number
  scannedActivities: number
  errors: string[]
}

export interface ReflectionResult {
  runId: string
  generated: EvolutionEntry[]
  activated: number
  pending: number
  discarded: number
  message: string
  providerUsed: string
  fallbackCount: number
  inputActivityCount: number
  inputTokens: number
  outputTokens: number
  durationMs: number
  estimatedCostUsd?: number
}

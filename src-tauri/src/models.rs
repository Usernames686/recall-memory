use serde::{Deserialize, Serialize};

pub const DEFAULT_CONTEXT_MODE: &str = "guided";
pub const DEFAULT_AGENT_MODE: &str = "reflection";
pub const DEFAULT_MODEL_PROVIDER: &str = "remote";
pub const DEFAULT_MODEL_TIMEOUT_SECONDS: i64 = 90;
pub const DEFAULT_INPUT_PRICE_PER_MILLION_USD: f64 = 0.0;
pub const DEFAULT_OUTPUT_PRICE_PER_MILLION_USD: f64 = 0.0;
pub const DEFAULT_FALLBACK_BASE_URL: &str = "http://127.0.0.1:11434/v1";
pub const DEFAULT_FALLBACK_MODEL: &str = "qwen3:8b";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceSummary {
    pub provider: String,
    pub root: String,
    pub available: bool,
    pub session_count: i64,
    pub activity_count: i64,
    pub error: Option<String>,
    #[serde(default)]
    pub last_scanned_at: Option<i64>,
    #[serde(default)]
    pub error_count: i64,
    #[serde(default)]
    pub cursor_count: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionSummary {
    pub id: String,
    pub provider: String,
    pub title: String,
    pub source_path: String,
    pub cwd: Option<String>,
    pub activity_count: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanSummary {
    pub days: i64,
    pub sources: Vec<SourceSummary>,
    pub sessions: Vec<SessionSummary>,
    pub scanned_sessions: i64,
    pub scanned_activities: i64,
    pub new_activities: i64,
    pub skipped_files: i64,
    pub errors: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Activity {
    pub id: String,
    pub provider: String,
    pub session_id: String,
    pub source_path: String,
    pub kind: String,
    pub role: String,
    pub text: String,
    pub occurred_at: i64,
    pub metadata: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EvolutionEntry {
    pub id: String,
    pub kind: String,
    pub title: String,
    pub summary: String,
    pub body: String,
    pub status: String,
    pub risk: String,
    pub source_refs: Vec<String>,
    pub updated_at: i64,
    #[serde(default)]
    pub origin_run_id: Option<String>,
    #[serde(default)]
    pub target_entry_id: Option<String>,
    #[serde(default = "default_entry_version")]
    pub version: i64,
}

fn default_entry_version() -> i64 {
    1
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DashboardSnapshot {
    pub consent_granted: bool,
    pub sources: Vec<SourceSummary>,
    pub sessions: Vec<SessionSummary>,
    pub activities: Vec<Activity>,
    pub run_activities: Vec<Activity>,
    pub entries: Vec<EvolutionEntry>,
    pub pending_count: i64,
    pub activity_count: i64,
    pub dirty_count: i64,
    pub last_reflection_at: Option<i64>,
    pub config: ReflectionConfigView,
    pub evolution: EvolutionSettingsView,
    pub run: Option<EvolutionRunState>,
    pub run_history: Vec<EvolutionRunState>,
    pub store_stats: StoreStats,
    pub redaction_report: RedactionReport,
    pub cache_cleanup_preview: CacheCleanupPreview,
    pub backups: Vec<StoreBackup>,
    pub audit_events: Vec<AuditEvent>,
    pub mcp: McpStatus,
    #[serde(default)]
    pub recovery_notice: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct McpStatus {
    pub codex: bool,
    pub claude: bool,
    pub last_checked: Option<i64>,
    pub health_status: String,
    pub health_error: Option<String>,
    pub recent_calls: Vec<McpCallSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpCallSummary {
    pub id: i64,
    pub occurred_at: i64,
    pub tool_name: String,
    pub action: Option<String>,
    pub result_status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ReflectionConfigView {
    pub provider: String,
    pub base_url: String,
    pub model: String,
    pub has_api_key: bool,
    pub context_mode: String,
    pub timeout_seconds: i64,
    pub fallback_enabled: bool,
    pub fallback_base_url: String,
    pub fallback_model: String,
    pub fallback_timeout_seconds: i64,
    #[serde(default = "default_input_price_per_million_usd")]
    pub input_price_per_million_usd: f64,
    #[serde(default = "default_output_price_per_million_usd")]
    pub output_price_per_million_usd: f64,
    pub health_status: String,
    pub health_error: Option<String>,
    pub last_checked_at: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReflectionConfigInput {
    #[serde(default = "default_model_provider")]
    pub provider: String,
    pub base_url: String,
    pub model: String,
    pub api_key: Option<String>,
    #[serde(default)]
    pub context_mode: Option<String>,
    #[serde(default = "default_model_timeout_seconds")]
    pub timeout_seconds: i64,
    #[serde(default = "default_true")]
    pub fallback_enabled: bool,
    #[serde(default = "default_fallback_base_url")]
    pub fallback_base_url: String,
    #[serde(default = "default_fallback_model")]
    pub fallback_model: String,
    #[serde(default = "default_model_timeout_seconds")]
    pub fallback_timeout_seconds: i64,
    #[serde(default = "default_input_price_per_million_usd")]
    pub input_price_per_million_usd: f64,
    #[serde(default = "default_output_price_per_million_usd")]
    pub output_price_per_million_usd: f64,
}

fn default_model_provider() -> String {
    DEFAULT_MODEL_PROVIDER.to_string()
}

fn default_model_timeout_seconds() -> i64 {
    DEFAULT_MODEL_TIMEOUT_SECONDS
}

fn default_fallback_base_url() -> String {
    DEFAULT_FALLBACK_BASE_URL.to_string()
}

fn default_fallback_model() -> String {
    DEFAULT_FALLBACK_MODEL.to_string()
}

fn default_input_price_per_million_usd() -> f64 {
    DEFAULT_INPUT_PRICE_PER_MILLION_USD
}

fn default_output_price_per_million_usd() -> f64 {
    DEFAULT_OUTPUT_PRICE_PER_MILLION_USD
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunnerConfigSnapshot {
    pub provider: String,
    pub base_url: String,
    pub model: String,
    pub timeout_seconds: i64,
    pub agent_mode: String,
    pub auto_activate_low_risk: bool,
    pub max_agent_steps: i64,
    #[serde(default = "default_risk_policy_version")]
    pub risk_policy_version: String,
    pub fallback_enabled: bool,
    pub fallback_base_url: String,
    pub fallback_model: String,
    pub fallback_timeout_seconds: i64,
    #[serde(default = "default_input_price_per_million_usd")]
    pub input_price_per_million_usd: f64,
    #[serde(default = "default_output_price_per_million_usd")]
    pub output_price_per_million_usd: f64,
}

impl RunnerConfigSnapshot {
    pub fn from_views(config: &ReflectionConfigView, settings: &EvolutionSettingsView) -> Self {
        Self {
            provider: config.provider.clone(),
            base_url: config.base_url.clone(),
            model: config.model.clone(),
            timeout_seconds: config.timeout_seconds,
            agent_mode: settings.agent_mode.clone(),
            auto_activate_low_risk: settings.auto_activate_low_risk,
            max_agent_steps: settings.max_agent_steps,
            risk_policy_version: default_risk_policy_version(),
            fallback_enabled: config.fallback_enabled,
            fallback_base_url: config.fallback_base_url.clone(),
            fallback_model: config.fallback_model.clone(),
            fallback_timeout_seconds: config.fallback_timeout_seconds,
            input_price_per_million_usd: config.input_price_per_million_usd,
            output_price_per_million_usd: config.output_price_per_million_usd,
        }
    }
}

pub const RISK_POLICY_VERSION: &str = "risk-v1";

fn default_risk_policy_version() -> String {
    RISK_POLICY_VERSION.to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReflectionRunResult {
    pub run_id: String,
    pub generated: Vec<EvolutionEntry>,
    pub activated: i64,
    pub pending: i64,
    pub discarded: i64,
    pub message: String,
    #[serde(default)]
    pub verification_status: String,
    #[serde(default)]
    pub verification_summary: Option<String>,
    #[serde(default)]
    pub candidate_verifications: Vec<CandidateVerification>,
    #[serde(default)]
    pub provider_used: String,
    #[serde(default)]
    pub fallback_count: i64,
    #[serde(default)]
    pub input_activity_count: i64,
    #[serde(default)]
    pub input_tokens: i64,
    #[serde(default)]
    pub output_tokens: i64,
    #[serde(default)]
    pub duration_ms: i64,
    #[serde(default)]
    pub estimated_cost_usd: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CandidateVerification {
    pub run_id: String,
    pub entry_id: String,
    pub evidence_sufficient: bool,
    pub supporting_evidence: Vec<String>,
    pub contradicting_evidence: Vec<String>,
    pub confidence: f64,
    pub duplicate: bool,
    pub conflict: bool,
    pub recommendation: String,
    pub rationale: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EvolutionSettingsView {
    pub enabled: bool,
    pub codex_enabled: bool,
    pub claude_enabled: bool,
    pub lookback_days: i64,
    pub run_mode: String,
    pub schedule_hours: i64,
    pub listen_since: Option<i64>,
    pub auto_activate_low_risk: bool,
    pub max_agent_steps: i64,
    pub launch_at_login: bool,
    pub notifications_enabled: bool,
    pub agent_mode: String,
    pub codex_source_path: String,
    pub claude_source_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EvolutionSettingsInput {
    pub enabled: bool,
    pub codex_enabled: bool,
    pub claude_enabled: bool,
    pub lookback_days: i64,
    pub run_mode: String,
    pub schedule_hours: i64,
    pub listen_since: Option<i64>,
    pub auto_activate_low_risk: bool,
    pub max_agent_steps: i64,
    #[serde(default)]
    pub launch_at_login: bool,
    #[serde(default = "default_true")]
    pub notifications_enabled: bool,
    #[serde(default = "default_agent_mode")]
    pub agent_mode: String,
    #[serde(default)]
    pub codex_source_path: String,
    #[serde(default)]
    pub claude_source_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SchedulerState {
    pub retry_after: i64,
    pub failure_count: i64,
    pub listener_pending_count: i64,
    pub listener_last_change: i64,
}

fn default_true() -> bool {
    true
}

fn default_agent_mode() -> String {
    DEFAULT_AGENT_MODE.to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EvolutionRunState {
    pub run_id: String,
    pub mode: String,
    pub phase: String,
    pub started_at: i64,
    pub completed_at: Option<i64>,
    pub scanned_activities: i64,
    pub consumed_activities: i64,
    pub generated: i64,
    pub activated: i64,
    pub pending: i64,
    pub error: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub providers: Vec<String>,
    #[serde(default)]
    pub lookback_days: i64,
    #[serde(default)]
    pub rolled_back_at: Option<i64>,
    #[serde(default = "default_agent_mode")]
    pub agent_mode: String,
    #[serde(default)]
    pub trace_count: i64,
    #[serde(default)]
    pub verification_status: String,
    #[serde(default)]
    pub verification_summary: Option<String>,
    #[serde(default)]
    pub retry_of_run_id: Option<String>,
    #[serde(default)]
    pub provider_used: Option<String>,
    #[serde(default)]
    pub fallback_count: i64,
    #[serde(default)]
    pub input_activity_count: i64,
    #[serde(default)]
    pub input_tokens: i64,
    #[serde(default)]
    pub output_tokens: i64,
    #[serde(default)]
    pub duration_ms: i64,
    #[serde(default)]
    pub estimated_cost_usd: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentTraceEvent {
    pub id: i64,
    pub run_id: String,
    pub occurred_at: i64,
    pub phase: String,
    pub event_type: String,
    pub tool_name: Option<String>,
    pub summary: String,
    pub duration_ms: Option<i64>,
    pub result_status: String,
    pub error_code: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EvolutionRunDetail {
    pub run: EvolutionRunState,
    pub activities: Vec<Activity>,
    pub entries: Vec<EvolutionEntry>,
    pub traces: Vec<AgentTraceEvent>,
    pub candidate_verifications: Vec<CandidateVerification>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EntryVersion {
    pub id: i64,
    pub entry_id: String,
    pub version: i64,
    pub kind: String,
    pub title: String,
    pub summary: String,
    pub body: String,
    pub status: String,
    pub risk: String,
    pub source_refs: Vec<String>,
    pub origin_run_id: Option<String>,
    pub target_entry_id: Option<String>,
    pub created_at: i64,
    pub action: String,
    #[serde(default)]
    pub source_run_id: Option<String>,
    #[serde(default)]
    pub reviewer: Option<String>,
    #[serde(default)]
    pub review_reason: Option<String>,
    #[serde(default)]
    pub reviewed_at: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EntryVersionDiff {
    pub entry_id: String,
    pub from_version: Option<i64>,
    pub to_version: i64,
    pub old_body: String,
    pub new_body: String,
    pub old_summary: String,
    pub new_summary: String,
    pub changed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct StoreStats {
    pub database_path: String,
    pub database_bytes: u64,
    pub entry_count: i64,
    pub active_count: i64,
    pub pending_count: i64,
    pub version_count: i64,
    pub activity_count: i64,
    pub reflected_activity_count: i64,
    pub run_count: i64,
    pub audit_count: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuditEvent {
    pub id: i64,
    pub occurred_at: i64,
    pub action: String,
    pub object_id: Option<String>,
    pub detail: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MaintenanceResult {
    pub path: Option<String>,
    pub affected: i64,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct RedactionReport {
    pub processed_records: i64,
    pub redacted_records: i64,
    pub redaction_count: i64,
    pub categories: Vec<RedactionCategoryCount>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RedactionCategoryCount {
    pub category: String,
    pub count: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct CacheCleanupPreview {
    pub reflected_activities: i64,
    pub run_activity_links: i64,
    pub affected_runs: i64,
    pub preserved_entries: i64,
    pub preserved_versions: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunRollbackResult {
    pub run_id: String,
    pub disabled_entries: i64,
    pub restored_entries: i64,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoreBackup {
    pub file_name: String,
    pub path: String,
    pub bytes: u64,
    pub created_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpInstallResult {
    pub codex_config: String,
    pub claude_config: String,
    pub backups: Vec<String>,
    pub sidecar_path: String,
}

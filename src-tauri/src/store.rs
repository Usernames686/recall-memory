use crate::models::{
    Activity, AgentTraceEvent, AuditEvent, CacheCleanupPreview, CandidateVerification,
    EntryVersion, EntryVersionDiff, EvolutionEntry, EvolutionRunDetail, EvolutionRunState,
    EvolutionSettingsInput, EvolutionSettingsView, MaintenanceResult, McpCallSummary,
    RedactionCategoryCount, RedactionReport, ReflectionConfigView, ReflectionRunResult,
    RunRollbackResult, RunnerConfigSnapshot, SessionSummary, StoreBackup, StoreStats,
    DEFAULT_AGENT_MODE, DEFAULT_CONTEXT_MODE, DEFAULT_FALLBACK_BASE_URL, DEFAULT_FALLBACK_MODEL,
    DEFAULT_INPUT_PRICE_PER_MILLION_USD, DEFAULT_MODEL_PROVIDER, DEFAULT_MODEL_TIMEOUT_SECONDS,
    DEFAULT_OUTPUT_PRICE_PER_MILLION_USD,
};
use crate::paths;
use regex::Regex;
use rusqlite::{params, Connection, OpenFlags, OptionalExtension, Row, Transaction};
use std::collections::HashSet;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::sync::OnceLock;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("database error: {0}")]
    Sql(#[from] rusqlite::Error),
    #[error("filesystem error: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid stored JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("invalid setting: {0}")]
    InvalidSetting(String),
}

pub struct Store {
    conn: Connection,
    path: std::path::PathBuf,
    read_only: bool,
}

const CURRENT_SCHEMA_VERSION: i64 = 4;

#[derive(Debug, Clone)]
pub struct ScanCursor {
    pub size: i64,
    pub modified_at: i64,
    pub oldest_activity_at: i64,
}

impl Store {
    pub fn open(path: std::path::PathBuf) -> Result<Self, StoreError> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut conn = Connection::open(&path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "busy_timeout", 3000)?;
        conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS app_state (key TEXT PRIMARY KEY, value TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS config (key TEXT PRIMARY KEY, value TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS schema_migrations (
                version INTEGER PRIMARY KEY,
                applied_at INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS activities (
                id TEXT PRIMARY KEY,
                provider TEXT NOT NULL,
                session_id TEXT NOT NULL,
                source_path TEXT NOT NULL,
                kind TEXT NOT NULL,
                role TEXT NOT NULL,
                text TEXT NOT NULL,
                occurred_at INTEGER NOT NULL,
                metadata_json TEXT NOT NULL,
                reflected_at INTEGER
            );
            CREATE INDEX IF NOT EXISTS activities_occurred_idx ON activities(occurred_at DESC);
            CREATE TABLE IF NOT EXISTS sessions (
                id TEXT PRIMARY KEY,
                provider TEXT NOT NULL,
                title TEXT NOT NULL,
                source_path TEXT NOT NULL,
                cwd TEXT,
                activity_count INTEGER NOT NULL,
                updated_at INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS entries (
                id TEXT PRIMARY KEY,
                kind TEXT NOT NULL,
                title TEXT NOT NULL,
                summary TEXT NOT NULL,
                body TEXT NOT NULL,
                status TEXT NOT NULL,
                risk TEXT NOT NULL,
                source_refs_json TEXT NOT NULL,
                updated_at INTEGER NOT NULL,
                origin_run_id TEXT,
                target_entry_id TEXT,
                version INTEGER NOT NULL DEFAULT 1
            );
            CREATE TABLE IF NOT EXISTS entry_versions (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                entry_id TEXT NOT NULL,
                version INTEGER NOT NULL,
                kind TEXT NOT NULL,
                title TEXT NOT NULL,
                summary TEXT NOT NULL,
                body TEXT NOT NULL,
                status TEXT NOT NULL,
                risk TEXT NOT NULL,
                source_refs_json TEXT NOT NULL,
                origin_run_id TEXT,
                target_entry_id TEXT,
                created_at INTEGER NOT NULL,
                action TEXT NOT NULL,
                source_run_id TEXT,
                reviewer TEXT,
                review_reason TEXT,
                reviewed_at INTEGER,
                UNIQUE(entry_id, version)
            );
            CREATE INDEX IF NOT EXISTS entry_versions_entry_idx
                ON entry_versions(entry_id, version DESC);
            CREATE TABLE IF NOT EXISTS reflection_runs (
                id TEXT PRIMARY KEY,
                occurred_at INTEGER NOT NULL,
                generated INTEGER NOT NULL,
                activated INTEGER NOT NULL,
                pending INTEGER NOT NULL,
                discarded INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS evolution_runs (
                id TEXT PRIMARY KEY,
                mode TEXT NOT NULL,
                phase TEXT NOT NULL,
                started_at INTEGER NOT NULL,
                completed_at INTEGER,
                scanned_activities INTEGER NOT NULL DEFAULT 0,
                consumed_activities INTEGER NOT NULL DEFAULT 0,
                generated INTEGER NOT NULL DEFAULT 0,
                activated INTEGER NOT NULL DEFAULT 0,
                pending INTEGER NOT NULL DEFAULT 0,
                error TEXT,
                model TEXT,
                providers_json TEXT NOT NULL DEFAULT '[]',
                lookback_days INTEGER NOT NULL DEFAULT 30,
                rolled_back_at INTEGER,
                agent_mode TEXT NOT NULL DEFAULT 'reflection',
                trace_count INTEGER NOT NULL DEFAULT 0,
                verification_status TEXT NOT NULL DEFAULT 'not_run',
                verification_summary TEXT,
                retry_of_run_id TEXT,
                runner_config_json TEXT NOT NULL DEFAULT '{}',
                provider_used TEXT,
                fallback_count INTEGER NOT NULL DEFAULT 0,
                input_activity_count INTEGER NOT NULL DEFAULT 0,
                input_tokens INTEGER NOT NULL DEFAULT 0,
                output_tokens INTEGER NOT NULL DEFAULT 0,
                model_duration_ms INTEGER NOT NULL DEFAULT 0,
                estimated_cost_usd REAL
            );
            CREATE TABLE IF NOT EXISTS agent_trace_events (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                run_id TEXT NOT NULL,
                occurred_at INTEGER NOT NULL,
                phase TEXT NOT NULL,
                event_type TEXT NOT NULL,
                tool_name TEXT,
                summary TEXT NOT NULL,
                duration_ms INTEGER,
                result_status TEXT NOT NULL,
                error_code TEXT
            );
            CREATE INDEX IF NOT EXISTS agent_trace_events_run_idx
                ON agent_trace_events(run_id, occurred_at, id);
            CREATE TABLE IF NOT EXISTS candidate_verifications (
                run_id TEXT NOT NULL,
                entry_id TEXT NOT NULL,
                evidence_sufficient INTEGER NOT NULL,
                supporting_evidence_json TEXT NOT NULL,
                contradicting_evidence_json TEXT NOT NULL,
                confidence REAL NOT NULL,
                duplicate INTEGER NOT NULL,
                conflict INTEGER NOT NULL,
                recommendation TEXT NOT NULL,
                rationale TEXT NOT NULL,
                PRIMARY KEY(run_id, entry_id)
            );
            CREATE INDEX IF NOT EXISTS candidate_verifications_entry_idx
                ON candidate_verifications(entry_id, run_id);
            CREATE TABLE IF NOT EXISTS evolution_run_activities (
                run_id TEXT NOT NULL,
                activity_id TEXT NOT NULL,
                position INTEGER NOT NULL,
                PRIMARY KEY(run_id, activity_id)
            );
            CREATE INDEX IF NOT EXISTS evolution_run_activities_run_idx
                ON evolution_run_activities(run_id, position);
            CREATE TABLE IF NOT EXISTS scan_cursors (
                source_hash TEXT PRIMARY KEY,
                provider TEXT NOT NULL,
                size INTEGER NOT NULL,
                modified_at INTEGER NOT NULL,
                scanned_at INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS source_scan_stats (
                provider TEXT PRIMARY KEY,
                last_scanned_at INTEGER NOT NULL,
                error_count INTEGER NOT NULL DEFAULT 0
            );
            CREATE TABLE IF NOT EXISTS audit_log (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                occurred_at INTEGER NOT NULL,
                action TEXT NOT NULL,
                object_id TEXT,
                detail_json TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS mcp_call_log (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                occurred_at INTEGER NOT NULL,
                tool_name TEXT NOT NULL,
                action TEXT,
                result_status TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS mcp_call_log_time_idx
                ON mcp_call_log(occurred_at DESC, id DESC);
            ",
        )?;
        run_migrations(&mut conn)?;
        Ok(Self {
            conn,
            path,
            read_only: false,
        })
    }

    /// Open an existing Store for read-only consumers such as the MCP sidecar.
    ///
    /// This deliberately skips directory creation, schema setup, WAL changes,
    /// and migrations. A sidecar must never create or mutate the Active Store
    /// merely because a client asked for context.
    pub fn open_read_only(path: std::path::PathBuf) -> Result<Self, StoreError> {
        let conn = Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        conn.busy_timeout(std::time::Duration::from_millis(3_000))?;
        Ok(Self {
            conn,
            path,
            read_only: true,
        })
    }

    pub fn is_read_only(&self) -> bool {
        self.read_only
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn set_consent(&self, granted: bool) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT INTO app_state(key, value) VALUES ('consent_granted', ?1)
             ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            [if granted { "true" } else { "false" }],
        )?;
        Ok(())
    }

    pub fn consent_granted(&self) -> Result<bool, StoreError> {
        let value: Option<String> = self
            .conn
            .query_row(
                "SELECT value FROM app_state WHERE key='consent_granted'",
                [],
                |row| row.get(0),
            )
            .optional()?;
        Ok(value.as_deref() == Some("true"))
    }

    pub fn upsert_activity(&self, activity: &Activity) -> Result<bool, StoreError> {
        let inserted = self.conn.execute(
            "INSERT OR IGNORE INTO activities
             (id, provider, session_id, source_path, kind, role, text, occurred_at, metadata_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                activity.id,
                activity.provider,
                activity.session_id,
                activity.source_path,
                activity.kind,
                activity.role,
                activity.text,
                activity.occurred_at,
                serde_json::to_string(&activity.metadata)?
            ],
        )?;
        Ok(inserted > 0)
    }

    pub fn upsert_activities(&self, activities: &[Activity]) -> Result<i64, StoreError> {
        let tx = self.conn.unchecked_transaction()?;
        let mut inserted = 0i64;
        {
            let mut stmt = tx.prepare(
                "INSERT OR IGNORE INTO activities
                 (id, provider, session_id, source_path, kind, role, text, occurred_at, metadata_json)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            )?;
            for activity in activities {
                inserted += stmt.execute(params![
                    activity.id,
                    activity.provider,
                    activity.session_id,
                    activity.source_path,
                    activity.kind,
                    activity.role,
                    activity.text,
                    activity.occurred_at,
                    serde_json::to_string(&activity.metadata)?
                ])? as i64;
            }
        }
        tx.commit()?;
        Ok(inserted)
    }

    pub fn upsert_session(&self, session: &SessionSummary) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT INTO sessions(id, provider, title, source_path, cwd, activity_count, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(id) DO UPDATE SET provider=excluded.provider, title=excluded.title,
             source_path=excluded.source_path, cwd=excluded.cwd,
             activity_count=excluded.activity_count, updated_at=excluded.updated_at",
            params![
                session.id,
                session.provider,
                session.title,
                session.source_path,
                session.cwd,
                session.activity_count,
                session.updated_at
            ],
        )?;
        Ok(())
    }

    pub fn activities_for_reflection(&self, limit: i64) -> Result<Vec<Activity>, StoreError> {
        self.activities_for_reflection_since(limit, None)
    }

    pub fn activities_for_reflection_since(
        &self,
        limit: i64,
        since: Option<i64>,
    ) -> Result<Vec<Activity>, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, provider, session_id, source_path, kind, role, text, occurred_at, metadata_json
             FROM activities WHERE reflected_at IS NULL
             AND (?2 IS NULL OR occurred_at >= ?2)
             ORDER BY occurred_at DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit, since], |row| {
            let metadata: String = row.get(8)?;
            Ok(Activity {
                id: row.get(0)?,
                provider: row.get(1)?,
                session_id: row.get(2)?,
                source_path: row.get(3)?,
                kind: row.get(4)?,
                role: row.get(5)?,
                text: row.get(6)?,
                occurred_at: row.get(7)?,
                metadata: serde_json::from_str(&metadata).unwrap_or(serde_json::Value::Null),
            })
        })?;
        Ok(rows.filter_map(Result::ok).collect())
    }

    pub fn list_activities(&self, limit: i64) -> Result<Vec<Activity>, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, provider, session_id, source_path, kind, role, text, occurred_at, metadata_json
             FROM activities ORDER BY occurred_at DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map([limit], |row| {
            let metadata: String = row.get(8)?;
            Ok(Activity {
                id: row.get(0)?,
                provider: row.get(1)?,
                session_id: row.get(2)?,
                source_path: row.get(3)?,
                kind: row.get(4)?,
                role: row.get(5)?,
                text: row.get(6)?,
                occurred_at: row.get(7)?,
                metadata: serde_json::from_str(&metadata).unwrap_or(serde_json::Value::Null),
            })
        })?;
        Ok(rows.filter_map(Result::ok).collect())
    }

    pub fn set_evolution_run_activities(
        &self,
        run_id: &str,
        activities: &[Activity],
    ) -> Result<(), StoreError> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "DELETE FROM evolution_run_activities WHERE run_id=?1",
            [run_id],
        )?;
        for (position, activity) in activities.iter().enumerate() {
            tx.execute(
                "INSERT INTO evolution_run_activities(run_id, activity_id, position)
                 VALUES (?1, ?2, ?3)",
                params![run_id, activity.id, position as i64],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn evolution_run_activities(&self, run_id: &str) -> Result<Vec<Activity>, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT a.id, a.provider, a.session_id, a.source_path, a.kind, a.role,
                    a.text, a.occurred_at, a.metadata_json
             FROM evolution_run_activities era
             JOIN activities a ON a.id=era.activity_id
             WHERE era.run_id=?1
             ORDER BY era.position",
        )?;
        let rows = stmt.query_map([run_id], |row| {
            let metadata: String = row.get(8)?;
            Ok(Activity {
                id: row.get(0)?,
                provider: row.get(1)?,
                session_id: row.get(2)?,
                source_path: row.get(3)?,
                kind: row.get(4)?,
                role: row.get(5)?,
                text: row.get(6)?,
                occurred_at: row.get(7)?,
                metadata: serde_json::from_str(&metadata).unwrap_or(serde_json::Value::Null),
            })
        })?;
        Ok(rows.filter_map(Result::ok).collect())
    }

    pub fn mark_activities_reflected(
        &self,
        ids: &[String],
        occurred_at: i64,
    ) -> Result<(), StoreError> {
        let tx = self.conn.unchecked_transaction()?;
        for id in ids {
            tx.execute(
                "UPDATE activities SET reflected_at=?1 WHERE id=?2 AND reflected_at IS NULL",
                params![occurred_at, id],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn dirty_count(&self) -> Result<i64, StoreError> {
        Ok(self.conn.query_row(
            "SELECT COUNT(*) FROM activities WHERE reflected_at IS NULL",
            [],
            |row| row.get(0),
        )?)
    }

    pub fn dirty_count_since(&self, since: Option<i64>) -> Result<i64, StoreError> {
        Ok(self.conn.query_row(
            "SELECT COUNT(*) FROM activities
             WHERE reflected_at IS NULL AND (?1 IS NULL OR occurred_at >= ?1)",
            [since],
            |row| row.get(0),
        )?)
    }

    pub fn distinct_sessions_for_activities(&self, ids: &[String]) -> Result<usize, StoreError> {
        let mut sessions = HashSet::new();
        for id in ids {
            let session: Option<String> = self
                .conn
                .query_row(
                    "SELECT session_id FROM activities WHERE id=?1",
                    [id],
                    |row| row.get(0),
                )
                .optional()?;
            if let Some(session) = session {
                sessions.insert(session);
            }
        }
        Ok(sessions.len())
    }

    pub fn scan_cursor(&self, source_hash: &str) -> Result<Option<ScanCursor>, StoreError> {
        Ok(self
            .conn
            .query_row(
                "SELECT size, modified_at, oldest_activity_at FROM scan_cursors WHERE source_hash=?1",
                [source_hash],
                |row| {
                    Ok(ScanCursor {
                        size: row.get(0)?,
                        modified_at: row.get(1)?,
                        oldest_activity_at: row.get(2)?,
                    })
                },
            )
            .optional()?)
    }

    pub fn save_scan_cursor(
        &self,
        source_hash: &str,
        provider: &str,
        size: i64,
        modified_at: i64,
        oldest_activity_at: i64,
    ) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT INTO scan_cursors(source_hash, provider, size, modified_at, oldest_activity_at, scanned_at)
             VALUES (?1,?2,?3,?4,?5,unixepoch())
             ON CONFLICT(source_hash) DO UPDATE SET size=excluded.size,
             modified_at=excluded.modified_at, oldest_activity_at=excluded.oldest_activity_at,
             scanned_at=excluded.scanned_at",
            params![source_hash, provider, size, modified_at, oldest_activity_at],
        )?;
        Ok(())
    }

    pub fn record_source_scan(&self, provider: &str, error_count: i64) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT INTO source_scan_stats(provider, last_scanned_at, error_count)
             VALUES (?1, unixepoch(), ?2)
             ON CONFLICT(provider) DO UPDATE SET
                 last_scanned_at=excluded.last_scanned_at,
                 error_count=excluded.error_count",
            params![provider, error_count],
        )?;
        Ok(())
    }

    pub fn source_scan_health(
        &self,
        provider: &str,
    ) -> Result<(Option<i64>, i64, i64), StoreError> {
        let stats = self
            .conn
            .query_row(
                "SELECT last_scanned_at, error_count FROM source_scan_stats WHERE provider=?1",
                [provider],
                |row| Ok((Some(row.get(0)?), row.get(1)?)),
            )
            .optional()?
            .unwrap_or((None, 0));
        let cursor_count = self.conn.query_row(
            "SELECT COUNT(*) FROM scan_cursors WHERE provider=?1",
            [provider],
            |row| row.get(0),
        )?;
        Ok((stats.0, stats.1, cursor_count))
    }

    pub fn append_audit(
        &self,
        action: &str,
        object_id: Option<&str>,
        detail: &serde_json::Value,
    ) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT INTO audit_log(occurred_at, action, object_id, detail_json)
             VALUES (unixepoch(),?1,?2,?3)",
            params![action, object_id, serde_json::to_string(detail)?],
        )?;
        Ok(())
    }

    pub fn append_mcp_call(
        &self,
        tool_name: &str,
        action: Option<&str>,
        result_status: &str,
    ) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT INTO mcp_call_log(occurred_at, tool_name, action, result_status)
             VALUES (unixepoch(), ?1, ?2, ?3)",
            params![tool_name, action, result_status],
        )?;
        Ok(())
    }

    /// Keep sidecar telemetry outside the Active Store so MCP can remain
    /// read-only while the desktop UI still shows recent call summaries.
    pub fn append_mcp_telemetry(
        &self,
        tool_name: &str,
        action: Option<&str>,
        result_status: &str,
    ) -> Result<(), StoreError> {
        let path = self.mcp_telemetry_path();
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;
        let record = serde_json::json!({
            "occurred_at": chrono::Utc::now().timestamp(),
            "tool_name": crate::scanner::redact(tool_name),
            "action": action.map(crate::scanner::redact),
            "result_status": crate::scanner::redact(result_status)
        });
        writeln!(file, "{}", serde_json::to_string(&record)?)?;
        restrict_file_permissions(&path)?;
        Ok(())
    }

    pub fn mcp_telemetry_path(&self) -> std::path::PathBuf {
        self.path.with_extension("mcp-calls.jsonl")
    }

    pub fn recent_mcp_calls(&self, limit: i64) -> Result<Vec<McpCallSummary>, StoreError> {
        let limit = limit.clamp(1, 1000) as usize;
        let mut stmt = self.conn.prepare(
            "SELECT id, occurred_at, tool_name, action, result_status
             FROM mcp_call_log ORDER BY occurred_at DESC, id DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map([limit as i64], |row| {
            Ok(McpCallSummary {
                id: row.get(0)?,
                occurred_at: row.get(1)?,
                tool_name: row.get(2)?,
                action: row.get(3)?,
                result_status: row.get(4)?,
            })
        })?;
        let mut calls = rows.filter_map(Result::ok).collect::<Vec<_>>();
        if let Ok(file) = fs::File::open(self.mcp_telemetry_path()) {
            for (index, line) in BufReader::new(file).lines().enumerate() {
                let Ok(line) = line else { continue };
                let Ok(record) = serde_json::from_str::<serde_json::Value>(&line) else {
                    continue;
                };
                let Some(occurred_at) = record.get("occurred_at").and_then(|value| value.as_i64())
                else {
                    continue;
                };
                calls.push(McpCallSummary {
                    id: -((index as i64) + 1),
                    occurred_at,
                    tool_name: record
                        .get("tool_name")
                        .and_then(|value| value.as_str())
                        .unwrap_or("unknown")
                        .to_string(),
                    action: record
                        .get("action")
                        .and_then(|value| value.as_str())
                        .map(ToString::to_string),
                    result_status: record
                        .get("result_status")
                        .and_then(|value| value.as_str())
                        .unwrap_or("unknown")
                        .to_string(),
                });
            }
        }
        calls.sort_by(|left, right| {
            right
                .occurred_at
                .cmp(&left.occurred_at)
                .then_with(|| right.id.cmp(&left.id))
        });
        calls.truncate(limit);
        Ok(calls)
    }

    pub fn list_sessions(&self, limit: i64) -> Result<Vec<SessionSummary>, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, provider, title, source_path, cwd, activity_count, updated_at
             FROM sessions ORDER BY updated_at DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map([limit], |row| {
            Ok(SessionSummary {
                id: row.get(0)?,
                provider: row.get(1)?,
                title: row.get(2)?,
                source_path: row.get(3)?,
                cwd: row.get(4)?,
                activity_count: row.get(5)?,
                updated_at: row.get(6)?,
            })
        })?;
        Ok(rows.filter_map(Result::ok).collect())
    }

    pub fn provider_totals(&self, provider: &str) -> Result<(i64, i64), StoreError> {
        Ok(self.conn.query_row(
            "SELECT COUNT(*), COALESCE(SUM(activity_count), 0)
             FROM sessions WHERE provider=?1",
            [provider],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?)
    }

    pub fn list_entries(&self) -> Result<Vec<EvolutionEntry>, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, kind, title, summary, body, status, risk, source_refs_json, updated_at,
                    origin_run_id, target_entry_id, version
             FROM entries ORDER BY updated_at DESC",
        )?;
        let rows = stmt.query_map([], |row| {
            let refs: String = row.get(7)?;
            Ok(EvolutionEntry {
                id: row.get(0)?,
                kind: row.get(1)?,
                title: row.get(2)?,
                summary: row.get(3)?,
                body: row.get(4)?,
                status: row.get(5)?,
                risk: row.get(6)?,
                source_refs: serde_json::from_str(&refs).unwrap_or_default(),
                updated_at: row.get(8)?,
                origin_run_id: row.get(9)?,
                target_entry_id: row.get(10)?,
                version: row.get(11)?,
            })
        })?;
        Ok(rows.filter_map(Result::ok).collect())
    }

    pub fn insert_entry(&self, entry: &EvolutionEntry) -> Result<(), StoreError> {
        let tx = self.conn.unchecked_transaction()?;
        write_entry_snapshot(&tx, entry, "generated")?;
        tx.commit()?;
        Ok(())
    }

    pub fn set_entry_status(&self, id: &str, status: &str) -> Result<(), StoreError> {
        self.set_entry_status_with_reason(id, status, "manual action")
    }

    pub fn set_entry_status_with_reason(
        &self,
        id: &str,
        status: &str,
        reason: &str,
    ) -> Result<(), StoreError> {
        if !matches!(status, "active" | "rejected" | "disabled") {
            return Err(StoreError::InvalidSetting("invalid entry status".into()));
        }
        let tx = self.conn.unchecked_transaction()?;
        let revision_target: Option<(String, Option<String>, String)> = tx
            .query_row(
                "SELECT kind, target_entry_id, body FROM entries WHERE id=?1",
                [id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        let revision_run_id: Option<String> = tx
            .query_row(
                "SELECT origin_run_id FROM entries WHERE id=?1",
                [id],
                |row| row.get(0),
            )
            .optional()?
            .flatten();
        let changed = tx.execute(
            "UPDATE entries SET status=?1, updated_at=unixepoch(), version=version+1 WHERE id=?2",
            params![status, id],
        )?;
        if changed == 0 {
            return Err(StoreError::InvalidSetting("entry not found".into()));
        }
        snapshot_current_entry_with_context(
            &tx,
            id,
            status,
            None,
            Some("local-user"),
            Some(reason),
        )?;
        if status == "active"
            && revision_target
                .as_ref()
                .map(|(kind, target, _)| {
                    kind == "revision"
                        && target
                            .as_deref()
                            .map(|value| !value.is_empty())
                            .unwrap_or(false)
                })
                .unwrap_or(false)
        {
            let (_, target, body) = revision_target.expect("checked above");
            let target = target.expect("checked above");
            tx.execute(
                "UPDATE entries SET body=?1, summary=?2, updated_at=unixepoch(), version=version+1
                 WHERE id=?3 AND status='active'",
                params![body, "由已批准 revision 更新", target],
            )?;
            snapshot_current_entry_with_context(
                &tx,
                &target,
                "revision_applied",
                revision_run_id.as_deref(),
                Some("local-user"),
                Some(reason),
            )?;
        }
        tx.execute(
            "INSERT INTO audit_log(occurred_at, action, object_id, detail_json)
             VALUES (unixepoch(), 'evolution_entry_status_changed', ?1, ?2)",
            params![
                id,
                serde_json::to_string(&serde_json::json!({
                    "status": status,
                    "actor": "local-user",
                    "reason": reason
                }))?
            ],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn save_reflection_run(
        &self,
        id: &str,
        generated: i64,
        activated: i64,
        pending: i64,
        discarded: i64,
    ) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT INTO reflection_runs(id, occurred_at, generated, activated, pending, discarded)
             VALUES (?1, unixepoch(), ?2, ?3, ?4, ?5)",
            params![id, generated, activated, pending, discarded],
        )?;
        Ok(())
    }

    pub fn persist_reflection_result(
        &self,
        result: &ReflectionRunResult,
    ) -> Result<(), StoreError> {
        let tx = self.conn.unchecked_transaction()?;
        write_reflection_result(&tx, result)?;
        tx.commit()?;
        Ok(())
    }

    pub fn persist_evolution_result(
        &self,
        run_id: &str,
        result: &ReflectionRunResult,
        activity_ids: &[String],
        scanned_activities: i64,
    ) -> Result<(), StoreError> {
        let tx = self.conn.unchecked_transaction()?;
        write_reflection_result(&tx, result)?;
        let now = chrono::Utc::now().timestamp();
        for id in activity_ids {
            tx.execute(
                "UPDATE activities SET reflected_at=?1 WHERE id=?2 AND reflected_at IS NULL",
                params![now, id],
            )?;
        }
        tx.execute(
            "UPDATE evolution_runs SET phase='completed', scanned_activities=?2,
             consumed_activities=?3, generated=?4, activated=?5, pending=?6,
             completed_at=unixepoch(), error=NULL WHERE id=?1",
            params![
                run_id,
                scanned_activities,
                activity_ids.len() as i64,
                result.generated.len() as i64,
                result.activated,
                result.pending
            ],
        )?;
        tx.execute(
            "INSERT INTO audit_log(occurred_at, action, object_id, detail_json)
             VALUES (unixepoch(), 'evolution_agent_completed', ?1, ?2)",
            params![
                run_id,
                serde_json::to_string(&serde_json::json!({
                    "consumed": activity_ids.len(),
                    "generated": result.generated.len(),
                    "activated": result.activated,
                    "pending": result.pending
                }))?
            ],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn last_reflection_at(&self) -> Result<Option<i64>, StoreError> {
        Ok(self
            .conn
            .query_row(
                "SELECT occurred_at FROM reflection_runs ORDER BY occurred_at DESC LIMIT 1",
                [],
                |row| row.get(0),
            )
            .optional()?)
    }

    pub fn start_evolution_run(&self, run_id: &str, mode: &str) -> Result<(), StoreError> {
        self.start_evolution_run_with_context(run_id, mode, None, &[], 30)
    }

    pub fn start_evolution_run_with_context(
        &self,
        run_id: &str,
        mode: &str,
        model: Option<&str>,
        providers: &[String],
        lookback_days: i64,
    ) -> Result<(), StoreError> {
        self.start_evolution_run_with_agent_context(
            run_id,
            mode,
            DEFAULT_AGENT_MODE,
            model,
            providers,
            lookback_days,
        )
    }

    pub fn start_evolution_run_with_agent_context(
        &self,
        run_id: &str,
        mode: &str,
        agent_mode: &str,
        model: Option<&str>,
        providers: &[String],
        lookback_days: i64,
    ) -> Result<(), StoreError> {
        if !matches!(agent_mode, "reflection" | "verification") {
            return Err(StoreError::InvalidSetting("invalid agent mode".into()));
        }
        self.conn.execute(
            "INSERT INTO evolution_runs
             (id, mode, phase, started_at, model, providers_json, lookback_days, agent_mode)
             VALUES (?1, ?2, 'scanning', unixepoch(), ?3, ?4, ?5, ?6)",
            params![
                run_id,
                mode,
                model,
                serde_json::to_string(providers)?,
                lookback_days,
                agent_mode
            ],
        )?;
        Ok(())
    }

    pub fn start_evolution_run_with_snapshot(
        &self,
        run_id: &str,
        mode: &str,
        model: Option<&str>,
        providers: &[String],
        lookback_days: i64,
        snapshot: &RunnerConfigSnapshot,
        retry_of_run_id: Option<&str>,
    ) -> Result<(), StoreError> {
        if !matches!(snapshot.agent_mode.as_str(), "reflection" | "verification") {
            return Err(StoreError::InvalidSetting("invalid agent mode".into()));
        }
        self.conn.execute(
            "INSERT INTO evolution_runs
             (id, mode, phase, started_at, model, providers_json, lookback_days, agent_mode,
              retry_of_run_id, runner_config_json)
             VALUES (?1, ?2, 'scanning', unixepoch(), ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                run_id,
                mode,
                model,
                serde_json::to_string(providers)?,
                lookback_days,
                snapshot.agent_mode,
                retry_of_run_id,
                serde_json::to_string(snapshot)?,
            ],
        )?;
        Ok(())
    }

    pub fn evolution_run_config_snapshot(
        &self,
        run_id: &str,
    ) -> Result<Option<RunnerConfigSnapshot>, StoreError> {
        let raw = self
            .conn
            .query_row(
                "SELECT runner_config_json FROM evolution_runs WHERE id=?1",
                [run_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        match raw.as_deref().map(str::trim) {
            None | Some("") | Some("{}") => Ok(None),
            Some(value) => Ok(Some(serde_json::from_str(value)?)),
        }
    }

    pub fn set_run_model_usage(
        &self,
        run_id: &str,
        result: &ReflectionRunResult,
    ) -> Result<(), StoreError> {
        self.conn.execute(
            "UPDATE evolution_runs SET provider_used=?2, fallback_count=?3,
             input_activity_count=?4, input_tokens=?5, output_tokens=?6,
             model_duration_ms=?7, estimated_cost_usd=?8 WHERE id=?1",
            params![
                run_id,
                result.provider_used,
                result.fallback_count.max(0),
                result.input_activity_count.max(0),
                result.input_tokens.max(0),
                result.output_tokens.max(0),
                result.duration_ms.max(0),
                result.estimated_cost_usd,
            ],
        )?;
        Ok(())
    }

    pub fn set_run_verification(
        &self,
        run_id: &str,
        status: &str,
        summary: Option<&str>,
    ) -> Result<(), StoreError> {
        self.conn.execute(
            "UPDATE evolution_runs SET verification_status=?2, verification_summary=?3 WHERE id=?1",
            params![run_id, status, summary],
        )?;
        Ok(())
    }

    pub fn update_evolution_run(
        &self,
        run_id: &str,
        phase: &str,
        scanned: i64,
        consumed: i64,
        generated: i64,
        activated: i64,
        pending: i64,
        error: Option<&str>,
    ) -> Result<(), StoreError> {
        let sanitized_error = error.map(sanitize_stored_error);
        self.conn.execute(
            "UPDATE evolution_runs SET phase=?2, scanned_activities=?3, consumed_activities=?4,
             generated=?5, activated=?6, pending=?7,
             completed_at=CASE WHEN ?2 IN ('completed','failed','cancelled','interrupted') THEN unixepoch() ELSE completed_at END,
             error=?8 WHERE id=?1",
            params![
                run_id,
                phase,
                scanned,
                consumed,
                generated,
                activated,
                pending,
                sanitized_error
            ],
        )?;
        Ok(())
    }

    pub fn current_evolution_run(&self) -> Result<Option<EvolutionRunState>, StoreError> {
        let row = self
            .conn
            .query_row(
                "SELECT id, mode, phase, started_at, completed_at, scanned_activities,
                        consumed_activities, generated, activated, pending, error,
                        model, providers_json, lookback_days, rolled_back_at,
                        agent_mode, trace_count, verification_status, verification_summary,
                        retry_of_run_id, provider_used, fallback_count,
                        input_activity_count, input_tokens, output_tokens,
                        model_duration_ms, estimated_cost_usd
                 FROM evolution_runs ORDER BY started_at DESC LIMIT 1",
                [],
                read_evolution_run,
            )
            .optional()?;
        Ok(row)
    }

    pub fn list_evolution_runs(&self, limit: i64) -> Result<Vec<EvolutionRunState>, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, mode, phase, started_at, completed_at, scanned_activities,
                    consumed_activities, generated, activated, pending, error,
                    model, providers_json, lookback_days, rolled_back_at,
                    agent_mode, trace_count, verification_status, verification_summary,
                    retry_of_run_id, provider_used, fallback_count,
                    input_activity_count, input_tokens, output_tokens,
                    model_duration_ms, estimated_cost_usd
             FROM evolution_runs ORDER BY started_at DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map([limit], read_evolution_run)?;
        Ok(rows.filter_map(Result::ok).collect())
    }

    pub fn evolution_run_detail(&self, run_id: &str) -> Result<EvolutionRunDetail, StoreError> {
        let run = self
            .conn
            .query_row(
                "SELECT id, mode, phase, started_at, completed_at, scanned_activities,
                        consumed_activities, generated, activated, pending, error,
                        model, providers_json, lookback_days, rolled_back_at,
                        agent_mode, trace_count, verification_status, verification_summary,
                        retry_of_run_id, provider_used, fallback_count,
                        input_activity_count, input_tokens, output_tokens,
                        model_duration_ms, estimated_cost_usd
                 FROM evolution_runs WHERE id=?1",
                [run_id],
                read_evolution_run,
            )
            .optional()?
            .ok_or_else(|| StoreError::InvalidSetting("evolution run not found".into()))?;
        let entries = self
            .list_entries()?
            .into_iter()
            .filter(|entry| entry.origin_run_id.as_deref() == Some(run_id))
            .collect();
        Ok(EvolutionRunDetail {
            run,
            activities: self.evolution_run_activities(run_id)?,
            entries,
            traces: self.list_trace_events(run_id, 500)?,
            candidate_verifications: self.candidate_verifications_for_run(run_id)?,
        })
    }

    pub fn candidate_verifications_for_run(
        &self,
        run_id: &str,
    ) -> Result<Vec<CandidateVerification>, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT run_id, entry_id, evidence_sufficient, supporting_evidence_json,
                    contradicting_evidence_json, confidence, duplicate, conflict,
                    recommendation, rationale
             FROM candidate_verifications WHERE run_id=?1 ORDER BY entry_id",
        )?;
        let rows = stmt.query_map([run_id], |row| {
            let supporting: String = row.get(3)?;
            let contradicting: String = row.get(4)?;
            Ok(CandidateVerification {
                run_id: row.get(0)?,
                entry_id: row.get(1)?,
                evidence_sufficient: row.get(2)?,
                supporting_evidence: serde_json::from_str(&supporting).unwrap_or_default(),
                contradicting_evidence: serde_json::from_str(&contradicting).unwrap_or_default(),
                confidence: row.get(5)?,
                duplicate: row.get(6)?,
                conflict: row.get(7)?,
                recommendation: row.get(8)?,
                rationale: row.get(9)?,
            })
        })?;
        Ok(rows.filter_map(Result::ok).collect())
    }

    pub fn append_trace_event(
        &self,
        run_id: &str,
        phase: &str,
        event_type: &str,
        tool_name: Option<&str>,
        summary: &str,
        duration_ms: Option<i64>,
        result_status: &str,
        error_code: Option<&str>,
    ) -> Result<AgentTraceEvent, StoreError> {
        let summary: String = crate::scanner::redact(summary).chars().take(500).collect();
        self.conn.execute(
            "INSERT INTO agent_trace_events
             (run_id, occurred_at, phase, event_type, tool_name, summary, duration_ms, result_status, error_code)
             VALUES (?1, unixepoch(), ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![run_id, phase, event_type, tool_name, summary, duration_ms, result_status, error_code],
        )?;
        let id = self.conn.last_insert_rowid();
        self.conn.execute(
            "UPDATE evolution_runs SET trace_count=trace_count+1 WHERE id=?1",
            [run_id],
        )?;
        Ok(AgentTraceEvent {
            id,
            run_id: run_id.to_string(),
            occurred_at: chrono::Utc::now().timestamp(),
            phase: phase.to_string(),
            event_type: event_type.to_string(),
            tool_name: tool_name.map(str::to_string),
            summary,
            duration_ms,
            result_status: result_status.to_string(),
            error_code: error_code.map(str::to_string),
        })
    }

    pub fn list_trace_events(
        &self,
        run_id: &str,
        limit: i64,
    ) -> Result<Vec<AgentTraceEvent>, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, run_id, occurred_at, phase, event_type, tool_name, summary,
                    duration_ms, result_status, error_code
             FROM agent_trace_events WHERE run_id=?1
             ORDER BY occurred_at, id LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![run_id, limit.clamp(1, 1_000)], |row| {
            Ok(AgentTraceEvent {
                id: row.get(0)?,
                run_id: row.get(1)?,
                occurred_at: row.get(2)?,
                phase: row.get(3)?,
                event_type: row.get(4)?,
                tool_name: row.get(5)?,
                summary: row.get(6)?,
                duration_ms: row.get(7)?,
                result_status: row.get(8)?,
                error_code: row.get(9)?,
            })
        })?;
        Ok(rows.filter_map(Result::ok).collect())
    }

    pub fn recover_interrupted_runs(&self) -> Result<i64, StoreError> {
        let affected = self.conn.execute(
            "UPDATE evolution_runs
             SET phase='interrupted', completed_at=unixepoch(),
                 error=COALESCE(error, 'App 在运行完成前退出，活动未被消费，可安全重试')
             WHERE phase NOT IN ('completed','failed','cancelled','interrupted')",
            [],
        )? as i64;
        if affected > 0 {
            self.append_audit(
                "evolution_runs_recovered",
                None,
                &serde_json::json!({"affected": affected}),
            )?;
        }
        Ok(affected)
    }

    pub fn list_entry_versions(&self, entry_id: &str) -> Result<Vec<EntryVersion>, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, entry_id, version, kind, title, summary, body, status, risk,
                    source_refs_json, origin_run_id, target_entry_id, created_at, action,
                    source_run_id, reviewer, review_reason, reviewed_at
             FROM entry_versions WHERE entry_id=?1 ORDER BY version DESC",
        )?;
        let rows = stmt.query_map([entry_id], |row| {
            let refs: String = row.get(9)?;
            Ok(EntryVersion {
                id: row.get(0)?,
                entry_id: row.get(1)?,
                version: row.get(2)?,
                kind: row.get(3)?,
                title: row.get(4)?,
                summary: row.get(5)?,
                body: row.get(6)?,
                status: row.get(7)?,
                risk: row.get(8)?,
                source_refs: serde_json::from_str(&refs).unwrap_or_default(),
                origin_run_id: row.get(10)?,
                target_entry_id: row.get(11)?,
                created_at: row.get(12)?,
                action: row.get(13)?,
                source_run_id: row.get(14)?,
                reviewer: row.get(15)?,
                review_reason: row.get(16)?,
                reviewed_at: row.get(17)?,
            })
        })?;
        Ok(rows.filter_map(Result::ok).collect())
    }

    pub fn entry_version_diff(
        &self,
        entry_id: &str,
        from_version: Option<i64>,
        to_version: i64,
    ) -> Result<EntryVersionDiff, StoreError> {
        let load = |version: i64| -> Result<(String, String), StoreError> {
            self.conn
                .query_row(
                    "SELECT summary, body FROM entry_versions WHERE entry_id=?1 AND version=?2",
                    params![entry_id, version],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?
                .ok_or_else(|| StoreError::InvalidSetting("entry version not found".into()))
        };
        let (new_summary, new_body) = load(to_version)?;
        let (old_summary, old_body) = match from_version {
            Some(version) => load(version)?,
            None => (String::new(), String::new()),
        };
        Ok(EntryVersionDiff {
            entry_id: entry_id.to_string(),
            from_version,
            to_version,
            changed: old_summary != new_summary || old_body != new_body,
            old_body,
            new_body,
            old_summary,
            new_summary,
        })
    }

    pub fn rollback_entry(&self, entry_id: &str, version: i64) -> Result<(), StoreError> {
        let tx = self.conn.unchecked_transaction()?;
        apply_entry_version(
            &tx,
            entry_id,
            version,
            "rollback",
            None,
            Some("local-user"),
            Some(&format!("回滚到 v{version}")),
        )?;
        tx.execute(
            "INSERT INTO audit_log(occurred_at, action, object_id, detail_json)
             VALUES (unixepoch(), 'evolution_entry_rolled_back', ?1, ?2)",
            params![
                entry_id,
                serde_json::to_string(&serde_json::json!({"sourceVersion": version}))?
            ],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn rollback_evolution_run(&self, run_id: &str) -> Result<RunRollbackResult, StoreError> {
        let tx = self.conn.unchecked_transaction()?;
        let phase: Option<String> = tx
            .query_row(
                "SELECT phase FROM evolution_runs WHERE id=?1",
                [run_id],
                |row| row.get(0),
            )
            .optional()?;
        let phase =
            phase.ok_or_else(|| StoreError::InvalidSetting("evolution run not found".into()))?;
        if phase != "completed" {
            return Err(StoreError::InvalidSetting(
                "only completed runs can be rolled back".into(),
            ));
        }
        let already_rolled_back: bool = tx.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM audit_log
                WHERE action='evolution_run_rolled_back' AND object_id=?1
             )",
            [run_id],
            |row| row.get(0),
        )?;
        if already_rolled_back {
            return Err(StoreError::InvalidSetting(
                "evolution run has already been rolled back".into(),
            ));
        }

        let entries = {
            let mut stmt = tx.prepare(
                "SELECT id, kind, target_entry_id, body
                 FROM entries WHERE origin_run_id=?1 ORDER BY id",
            )?;
            let rows = stmt.query_map([run_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, String>(3)?,
                ))
            })?;
            rows.filter_map(Result::ok).collect::<Vec<_>>()
        };
        if entries.is_empty() {
            return Err(StoreError::InvalidSetting(
                "run did not create any entries".into(),
            ));
        }

        let mut restored_targets = HashSet::new();
        for (_, kind, target, revision_body) in &entries {
            if kind != "revision" {
                continue;
            }
            let Some(target) = target.as_deref().filter(|value| !value.is_empty()) else {
                continue;
            };
            let applied_version: Option<i64> = tx
                .query_row(
                    "SELECT MIN(version) FROM entry_versions
                     WHERE entry_id=?1 AND action='revision_applied'
                       AND (source_run_id=?2 OR (source_run_id IS NULL AND body=?3))",
                    params![target, run_id, revision_body],
                    |row| row.get(0),
                )
                .optional()?
                .flatten();
            if let Some(applied_version) = applied_version.filter(|version| *version > 1) {
                if restored_targets.insert(target.to_string()) {
                    let current_version: i64 =
                        tx.query_row("SELECT version FROM entries WHERE id=?1", [target], |row| {
                            row.get(0)
                        })?;
                    if current_version != applied_version {
                        return Err(StoreError::InvalidSetting(format!(
                            "cannot roll back run: target {target} changed after this run"
                        )));
                    }
                    apply_entry_version(
                        &tx,
                        target,
                        applied_version - 1,
                        "run_rollback_restore",
                        Some(run_id),
                        Some("local-user"),
                        Some("按运行整体回滚"),
                    )?;
                }
            }
        }

        let mut disabled_entries = 0i64;
        for (entry_id, _, _, _) in &entries {
            let changed = tx.execute(
                "UPDATE entries SET status='disabled', updated_at=unixepoch(), version=version+1
                 WHERE id=?1 AND status!='disabled'",
                [entry_id],
            )? as i64;
            if changed > 0 {
                snapshot_current_entry_with_context(
                    &tx,
                    entry_id,
                    "run_rollback_disable",
                    Some(run_id),
                    Some("local-user"),
                    Some("按运行整体回滚"),
                )?;
                disabled_entries += changed;
            }
        }

        tx.execute(
            "INSERT INTO audit_log(occurred_at, action, object_id, detail_json)
             VALUES (unixepoch(), 'evolution_run_rolled_back', ?1, ?2)",
            params![
                run_id,
                serde_json::to_string(&serde_json::json!({
                    "disabledEntries": disabled_entries,
                    "restoredEntries": restored_targets.len(),
                    "actor": "local-user"
                }))?
            ],
        )?;
        tx.execute(
            "UPDATE evolution_runs SET rolled_back_at=unixepoch() WHERE id=?1",
            [run_id],
        )?;
        tx.commit()?;
        let restored_entries = restored_targets.len() as i64;
        Ok(RunRollbackResult {
            run_id: run_id.to_string(),
            disabled_entries,
            restored_entries,
            message: format!(
                "已回滚本次运行：停用 {disabled_entries} 条，恢复 {restored_entries} 条 Active 内容"
            ),
        })
    }

    pub fn list_audit_events(&self, limit: i64) -> Result<Vec<AuditEvent>, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, occurred_at, action, object_id, detail_json
             FROM audit_log ORDER BY occurred_at DESC, id DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map([limit], |row| {
            let detail: String = row.get(4)?;
            Ok(AuditEvent {
                id: row.get(0)?,
                occurred_at: row.get(1)?,
                action: row.get(2)?,
                object_id: row.get(3)?,
                detail: serde_json::from_str(&detail).unwrap_or(serde_json::Value::Null),
            })
        })?;
        Ok(rows.filter_map(Result::ok).collect())
    }

    pub fn store_stats(&self) -> Result<StoreStats, StoreError> {
        let count = |sql: &str| -> Result<i64, rusqlite::Error> {
            self.conn.query_row(sql, [], |row| row.get(0))
        };
        Ok(StoreStats {
            database_path: self.path.display().to_string(),
            database_bytes: fs::metadata(&self.path)
                .map(|value| value.len())
                .unwrap_or(0),
            entry_count: count("SELECT COUNT(*) FROM entries")?,
            active_count: count("SELECT COUNT(*) FROM entries WHERE status='active'")?,
            pending_count: count("SELECT COUNT(*) FROM entries WHERE status='pending'")?,
            version_count: count("SELECT COUNT(*) FROM entry_versions")?,
            activity_count: count("SELECT COUNT(*) FROM activities")?,
            reflected_activity_count: count(
                "SELECT COUNT(*) FROM activities WHERE reflected_at IS NOT NULL",
            )?,
            run_count: count("SELECT COUNT(*) FROM evolution_runs")?,
            audit_count: count("SELECT COUNT(*) FROM audit_log")?,
        })
    }

    pub fn redaction_report(&self) -> Result<RedactionReport, StoreError> {
        let processed_records =
            self.conn
                .query_row("SELECT COUNT(*) FROM activities", [], |row| row.get(0))?;
        let redacted_records = self.conn.query_row(
            "SELECT COUNT(*) FROM activities WHERE text LIKE '%[REDACTED%'",
            [],
            |row| row.get(0),
        )?;
        let markers = [
            ("API Key", "[REDACTED_API_KEY]"),
            ("访问令牌", "[REDACTED_TOKEN]"),
            ("通用凭据", "[REDACTED]"),
            ("URL 凭据", "[REDACTED_CREDENTIALS]"),
            ("邮箱", "[REDACTED_EMAIL]"),
            ("JWT", "[REDACTED_JWT]"),
            ("GitHub Token", "[REDACTED_GITHUB_TOKEN]"),
            ("AWS Key", "[REDACTED_AWS_KEY]"),
            ("私钥", "[REDACTED_PRIVATE_KEY]"),
            ("环境变量", "[REDACTED_ENV]"),
        ];
        let mut categories = Vec::new();
        let mut redaction_count = 0i64;
        for (category, marker) in markers {
            let count: i64 = self.conn.query_row(
                "SELECT COALESCE(SUM(
                    (LENGTH(text) - LENGTH(REPLACE(text, ?1, ''))) / LENGTH(?1)
                 ), 0) FROM activities",
                [marker],
                |row| row.get(0),
            )?;
            if count > 0 {
                redaction_count += count;
                categories.push(RedactionCategoryCount {
                    category: category.to_string(),
                    count,
                });
            }
        }
        Ok(RedactionReport {
            processed_records,
            redacted_records,
            redaction_count,
            categories,
        })
    }

    pub fn cache_cleanup_preview(&self) -> Result<CacheCleanupPreview, StoreError> {
        let reflected_activities = self.conn.query_row(
            "SELECT COUNT(*) FROM activities WHERE reflected_at IS NOT NULL",
            [],
            |row| row.get(0),
        )?;
        let (run_activity_links, affected_runs) = self.conn.query_row(
            "SELECT COUNT(*), COUNT(DISTINCT era.run_id)
             FROM evolution_run_activities era
             JOIN activities a ON a.id=era.activity_id
             WHERE a.reflected_at IS NOT NULL",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        Ok(CacheCleanupPreview {
            reflected_activities,
            run_activity_links,
            affected_runs,
            preserved_entries: self
                .conn
                .query_row("SELECT COUNT(*) FROM entries", [], |row| row.get(0))?,
            preserved_versions: self.conn.query_row(
                "SELECT COUNT(*) FROM entry_versions",
                [],
                |row| row.get(0),
            )?,
        })
    }

    pub fn backup_store(&self) -> Result<MaintenanceResult, StoreError> {
        self.conn.execute_batch("PRAGMA wal_checkpoint(FULL);")?;
        let directory = self.backups_directory();
        fs::create_dir_all(&directory)?;
        let now = chrono::Utc::now();
        let path = directory.join(format!(
            "recall-{}-{:09}.sqlite3",
            now.format("%Y%m%d-%H%M%S"),
            now.timestamp_subsec_nanos()
        ));
        fs::copy(&self.path, &path)?;
        restrict_file_permissions(&path)?;
        self.append_audit(
            "store_backup_created",
            None,
            &serde_json::json!({"path": path.display().to_string()}),
        )?;
        Ok(MaintenanceResult {
            path: Some(path.display().to_string()),
            affected: 1,
            message: "本地 Store 备份已创建".into(),
        })
    }

    pub fn list_backups(&self) -> Result<Vec<StoreBackup>, StoreError> {
        let directory = self.backups_directory();
        if !directory.is_dir() {
            return Ok(Vec::new());
        }
        let mut backups = Vec::new();
        for entry in fs::read_dir(directory)? {
            let entry = entry?;
            let path = entry.path();
            if !path.is_file()
                || path.extension().and_then(|value| value.to_str()) != Some("sqlite3")
            {
                continue;
            }
            let metadata = entry.metadata()?;
            let created_at = metadata
                .modified()
                .ok()
                .and_then(|value| value.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|value| value.as_secs() as i64)
                .unwrap_or(0);
            backups.push(StoreBackup {
                file_name: entry.file_name().to_string_lossy().to_string(),
                path: path.display().to_string(),
                bytes: metadata.len(),
                created_at,
            });
        }
        backups.sort_by(|left, right| right.created_at.cmp(&left.created_at));
        Ok(backups)
    }

    pub fn restore_store_backup(&self, file_name: &str) -> Result<MaintenanceResult, StoreError> {
        let requested = Path::new(file_name);
        if requested.components().count() != 1
            || requested.extension().and_then(|value| value.to_str()) != Some("sqlite3")
        {
            return Err(StoreError::InvalidSetting(
                "invalid backup file name".into(),
            ));
        }
        let directory = self.backups_directory();
        let directory = fs::canonicalize(&directory)?;
        let path = fs::canonicalize(directory.join(requested))?;
        if path.parent() != Some(directory.as_path()) {
            return Err(StoreError::InvalidSetting(
                "backup must be inside the Recall backups directory".into(),
            ));
        }

        // Keep a recoverable snapshot of the current Active Store before any
        // destructive replacement. The original sessions and run history are
        // intentionally left untouched by restore.
        let pre_restore_backup = self.backup_store()?;

        let backup = Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        let integrity: String = backup.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
        if integrity != "ok" {
            return Err(StoreError::InvalidSetting(format!(
                "backup integrity check failed: {integrity}"
            )));
        }
        let required_tables: i64 = backup.query_row(
            "SELECT COUNT(*) FROM sqlite_master
             WHERE type='table' AND name IN ('entries','entry_versions')",
            [],
            |row| row.get(0),
        )?;
        if required_tables != 2 {
            return Err(StoreError::InvalidSetting(
                "backup does not contain an Active Store".into(),
            ));
        }
        let has_version_context = ["source_run_id", "reviewer", "review_reason", "reviewed_at"]
            .into_iter()
            .all(|column| has_column(&backup, "entry_versions", column).unwrap_or(false));
        drop(backup);

        self.conn.execute(
            "ATTACH DATABASE ?1 AS restore_db",
            [path.display().to_string()],
        )?;
        let restore_result = (|| -> Result<i64, StoreError> {
            let tx = self.conn.unchecked_transaction()?;
            tx.execute("DELETE FROM entry_versions", [])?;
            tx.execute("DELETE FROM entries", [])?;
            tx.execute(
                "INSERT INTO entries
                 (id, kind, title, summary, body, status, risk, source_refs_json, updated_at,
                  origin_run_id, target_entry_id, version)
                 SELECT id, kind, title, summary, body, status, risk, source_refs_json, updated_at,
                        origin_run_id, target_entry_id, version
                 FROM restore_db.entries",
                [],
            )?;
            let version_sql = if has_version_context {
                "INSERT INTO entry_versions
                 (id, entry_id, version, kind, title, summary, body, status, risk,
                  source_refs_json, origin_run_id, target_entry_id, created_at, action,
                  source_run_id, reviewer, review_reason, reviewed_at)
                 SELECT id, entry_id, version, kind, title, summary, body, status, risk,
                        source_refs_json, origin_run_id, target_entry_id, created_at, action,
                        source_run_id, reviewer, review_reason, reviewed_at
                 FROM restore_db.entry_versions"
            } else {
                "INSERT INTO entry_versions
                 (id, entry_id, version, kind, title, summary, body, status, risk,
                  source_refs_json, origin_run_id, target_entry_id, created_at, action)
                 SELECT id, entry_id, version, kind, title, summary, body, status, risk,
                        source_refs_json, origin_run_id, target_entry_id, created_at, action
                 FROM restore_db.entry_versions"
            };
            tx.execute(version_sql, [])?;
            let affected: i64 =
                tx.query_row("SELECT COUNT(*) FROM entries", [], |row| row.get(0))?;
            tx.execute(
                "INSERT INTO audit_log(occurred_at, action, object_id, detail_json)
                 VALUES (unixepoch(), 'active_store_restored', NULL, ?1)",
                [serde_json::to_string(&serde_json::json!({
                    "backup": file_name,
                    "preRestoreBackup": pre_restore_backup.path,
                    "restoredEntries": affected,
                    "actor": "local-user"
                }))?],
            )?;
            tx.commit()?;
            Ok(affected)
        })();
        let detach_result = self.conn.execute_batch("DETACH DATABASE restore_db;");
        let affected = match restore_result {
            Ok(affected) => {
                detach_result?;
                affected
            }
            Err(error) => {
                let _ = detach_result;
                return Err(error);
            }
        };
        Ok(MaintenanceResult {
            path: Some(path.display().to_string()),
            affected,
            message: format!("已从 {file_name} 恢复 {affected} 条沉淀及其版本历史"),
        })
    }

    fn backups_directory(&self) -> std::path::PathBuf {
        self.path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join("backups")
    }

    pub fn export_redacted_store(&self) -> Result<MaintenanceResult, StoreError> {
        let directory = self
            .path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join("exports");
        fs::create_dir_all(&directory)?;
        let path = directory.join(format!(
            "recall-redacted-{}.json",
            chrono::Utc::now().format("%Y%m%d-%H%M%S")
        ));
        let stats = self.store_stats()?;
        let audit = self
            .list_audit_events(1000)?
            .into_iter()
            .map(|event| {
                serde_json::json!({
                    "id": event.id,
                    "occurredAt": event.occurred_at,
                    "action": event.action,
                    "objectId": event.object_id
                })
            })
            .collect::<Vec<_>>();
        let mut payload = serde_json::json!({
            "exportedAt": chrono::Utc::now().timestamp(),
            "entries": self.list_entries()?,
            "runs": self.list_evolution_runs(500)?,
            "audit": audit,
            "stats": {
                "databaseBytes": stats.database_bytes,
                "entryCount": stats.entry_count,
                "activeCount": stats.active_count,
                "pendingCount": stats.pending_count,
                "versionCount": stats.version_count,
                "activityCount": stats.activity_count,
                "reflectedActivityCount": stats.reflected_activity_count,
                "runCount": stats.run_count,
                "auditCount": stats.audit_count
            },
            "redactionReport": self.redaction_report()?
        });
        sanitize_export_paths(&mut payload);
        fs::write(&path, serde_json::to_vec_pretty(&payload)?)?;
        restrict_file_permissions(&path)?;
        self.append_audit(
            "redacted_store_exported",
            None,
            &serde_json::json!({"path": path.display().to_string()}),
        )?;
        Ok(MaintenanceResult {
            path: Some(path.display().to_string()),
            affected: self.list_entries()?.len() as i64,
            message: "脱敏导出已创建，不包含原始会话正文".into(),
        })
    }

    pub fn clear_reflected_activity_cache(&self) -> Result<MaintenanceResult, StoreError> {
        let preview = self.cache_cleanup_preview()?;
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "DELETE FROM evolution_run_activities
             WHERE activity_id IN (SELECT id FROM activities WHERE reflected_at IS NOT NULL)",
            [],
        )?;
        let affected =
            tx.execute("DELETE FROM activities WHERE reflected_at IS NOT NULL", [])? as i64;
        tx.execute(
            "INSERT INTO audit_log(occurred_at, action, object_id, detail_json)
             VALUES (unixepoch(), 'reflected_activity_cache_cleared', NULL, ?1)",
            [serde_json::to_string(&serde_json::json!({
                "affected": affected,
                "removedRunActivityLinks": preview.run_activity_links,
                "affectedRuns": preview.affected_runs,
                "preservedEntries": preview.preserved_entries,
                "preservedVersions": preview.preserved_versions
            }))?],
        )?;
        tx.commit()?;
        Ok(MaintenanceResult {
            path: None,
            affected,
            message: format!(
                "已清理 {affected} 条已消费活动及 {} 条运行输入关联，未修改沉淀和版本历史",
                preview.run_activity_links
            ),
        })
    }

    pub fn last_evolution_completed_at(&self) -> Result<Option<i64>, StoreError> {
        Ok(self
            .conn
            .query_row(
                "SELECT completed_at FROM evolution_runs
                 WHERE completed_at IS NOT NULL
                 ORDER BY completed_at DESC LIMIT 1",
                [],
                |row| row.get(0),
            )
            .optional()?)
    }

    pub fn pending_count(&self) -> Result<i64, StoreError> {
        Ok(self.conn.query_row(
            "SELECT COUNT(*) FROM entries WHERE status='pending'",
            [],
            |row| row.get(0),
        )?)
    }

    pub fn activity_count(&self) -> Result<i64, StoreError> {
        Ok(self
            .conn
            .query_row("SELECT COUNT(*) FROM activities", [], |row| row.get(0))?)
    }

    pub fn config(&self) -> Result<ReflectionConfigView, StoreError> {
        let get = |key: &str| -> Result<String, rusqlite::Error> {
            Ok(self
                .conn
                .query_row("SELECT value FROM config WHERE key=?1", [key], |row| {
                    row.get(0)
                })
                .optional()?
                .unwrap_or_default())
        };
        let has_key = keyring::Entry::new(
            "recall-evolution",
            &crate::keyring_account(
                match get("provider")?.as_str() {
                    "ollama" => "ollama",
                    _ => DEFAULT_MODEL_PROVIDER,
                },
                &get("base_url")?,
            ),
        )
        .ok()
        .and_then(|entry| entry.get_password().ok())
        .is_some();
        let provider = match get("provider")?.as_str() {
            "ollama" => "ollama".to_string(),
            _ => DEFAULT_MODEL_PROVIDER.to_string(),
        };
        let base_url = get("base_url")?;
        let model = get("model")?;
        let fallback_enabled = match get("fallback_enabled")?.as_str() {
            "false" | "0" => false,
            _ => true,
        };
        let fallback_base_url = match get("fallback_base_url")? {
            value if value.trim().is_empty() => DEFAULT_FALLBACK_BASE_URL.to_string(),
            value => value,
        };
        let fallback_model = match get("fallback_model")? {
            value if value.trim().is_empty() => DEFAULT_FALLBACK_MODEL.to_string(),
            value => value,
        };
        let input_price_per_million_usd = get("input_price_per_million_usd")?
            .parse::<f64>()
            .unwrap_or(DEFAULT_INPUT_PRICE_PER_MILLION_USD)
            .clamp(0.0, 10_000.0);
        let output_price_per_million_usd = get("output_price_per_million_usd")?
            .parse::<f64>()
            .unwrap_or(DEFAULT_OUTPUT_PRICE_PER_MILLION_USD)
            .clamp(0.0, 10_000.0);
        let health_matches = get("health_provider")? == provider
            && get("health_base_url")? == base_url
            && get("health_model")? == model;
        let health_status = if health_matches {
            match get("health_status")?.as_str() {
                "ok" | "error" | "checking" => get("health_status")?,
                _ => "unknown".to_string(),
            }
        } else {
            "unknown".to_string()
        };
        Ok(ReflectionConfigView {
            provider,
            base_url,
            model,
            has_api_key: has_key,
            context_mode: match get("context_mode")?.as_str() {
                "mcp" => "mcp".to_string(),
                _ => DEFAULT_CONTEXT_MODE.to_string(),
            },
            timeout_seconds: get("timeout_seconds")?
                .parse::<i64>()
                .unwrap_or(DEFAULT_MODEL_TIMEOUT_SECONDS)
                .clamp(10, 300),
            fallback_enabled,
            fallback_base_url,
            fallback_model,
            fallback_timeout_seconds: get("fallback_timeout_seconds")?
                .parse::<i64>()
                .unwrap_or(DEFAULT_MODEL_TIMEOUT_SECONDS)
                .clamp(10, 300),
            input_price_per_million_usd,
            output_price_per_million_usd,
            health_status,
            health_error: match if health_matches {
                get("health_error")?
            } else {
                String::new()
            }
            .trim()
            {
                "" => None,
                value => Some(value.chars().take(240).collect()),
            },
            last_checked_at: get("last_checked_at")?.parse::<i64>().ok(),
        })
    }

    pub fn evolution_settings(&self) -> Result<EvolutionSettingsView, StoreError> {
        let get = |key: &str| -> Result<String, rusqlite::Error> {
            Ok(self
                .conn
                .query_row("SELECT value FROM config WHERE key=?1", [key], |row| {
                    row.get(0)
                })
                .optional()?
                .unwrap_or_default())
        };
        let parse_bool = |key: &str, default: bool| -> Result<bool, rusqlite::Error> {
            Ok(match get(key)?.as_str() {
                "true" | "1" => true,
                "false" | "0" => false,
                _ => default,
            })
        };
        let parse_i64 = |key: &str, default: i64| -> Result<i64, rusqlite::Error> {
            Ok(get(key)?.parse::<i64>().unwrap_or(default))
        };
        let mode = get("run_mode")?;
        let mode = if matches!(mode.as_str(), "manual" | "listener" | "scheduled") {
            mode
        } else {
            "manual".to_string()
        };
        let lookback = parse_i64("lookback_days", 30)?;
        let lookback_days = if matches!(lookback, 1 | 7 | 30) {
            lookback
        } else {
            30
        };
        let schedule = parse_i64("schedule_hours", 12)?.clamp(1, 24);
        let max_steps = parse_i64("max_agent_steps", 6)?.clamp(2, 8);
        let since = get("listen_since")?.parse::<i64>().ok();
        Ok(EvolutionSettingsView {
            enabled: parse_bool("agent_enabled", true)?,
            codex_enabled: parse_bool("codex_enabled", true)?,
            claude_enabled: parse_bool("claude_enabled", true)?,
            lookback_days,
            run_mode: mode,
            schedule_hours: schedule,
            listen_since: since,
            auto_activate_low_risk: parse_bool("auto_activate_low_risk", true)?,
            max_agent_steps: max_steps,
            launch_at_login: parse_bool("launch_at_login", false)?,
            notifications_enabled: parse_bool("notifications_enabled", true)?,
            agent_mode: match get("agent_mode")?.as_str() {
                "verification" => "verification".to_string(),
                _ => DEFAULT_AGENT_MODE.to_string(),
            },
        })
    }

    pub fn save_evolution_settings(
        &self,
        input: &EvolutionSettingsInput,
    ) -> Result<EvolutionSettingsView, StoreError> {
        if !matches!(input.lookback_days, 1 | 7 | 30) {
            return Err(StoreError::InvalidSetting(
                "lookback_days must be 1, 7, or 30".into(),
            ));
        }
        if !matches!(input.run_mode.as_str(), "manual" | "listener" | "scheduled") {
            return Err(StoreError::InvalidSetting("invalid run mode".into()));
        }
        if !(1..=24).contains(&input.schedule_hours) {
            return Err(StoreError::InvalidSetting(
                "schedule_hours must be 1..24".into(),
            ));
        }
        if !(2..=8).contains(&input.max_agent_steps) {
            return Err(StoreError::InvalidSetting(
                "max_agent_steps must be 2..8".into(),
            ));
        }
        if !matches!(input.agent_mode.as_str(), "reflection" | "verification") {
            return Err(StoreError::InvalidSetting("invalid agent mode".into()));
        }
        for (key, value) in [
            ("agent_enabled", input.enabled.to_string()),
            ("codex_enabled", input.codex_enabled.to_string()),
            ("claude_enabled", input.claude_enabled.to_string()),
            ("lookback_days", input.lookback_days.to_string()),
            ("run_mode", input.run_mode.clone()),
            ("schedule_hours", input.schedule_hours.to_string()),
            (
                "auto_activate_low_risk",
                input.auto_activate_low_risk.to_string(),
            ),
            ("max_agent_steps", input.max_agent_steps.to_string()),
            ("launch_at_login", input.launch_at_login.to_string()),
            (
                "notifications_enabled",
                input.notifications_enabled.to_string(),
            ),
            ("agent_mode", input.agent_mode.clone()),
        ] {
            self.conn.execute(
                "INSERT INTO config(key,value) VALUES (?1,?2)
                 ON CONFLICT(key) DO UPDATE SET value=excluded.value",
                params![key, value],
            )?;
        }
        if let Some(since) = input.listen_since {
            self.conn.execute(
                "INSERT INTO config(key,value) VALUES ('listen_since',?1)
                 ON CONFLICT(key) DO UPDATE SET value=excluded.value",
                [since.to_string()],
            )?;
        } else {
            self.conn
                .execute("DELETE FROM config WHERE key='listen_since'", [])?;
        }
        self.evolution_settings()
    }

    pub fn save_config(
        &self,
        base_url: &str,
        model: &str,
        context_mode: &str,
    ) -> Result<(), StoreError> {
        self.save_config_with_provider(
            DEFAULT_MODEL_PROVIDER,
            base_url,
            model,
            context_mode,
            DEFAULT_MODEL_TIMEOUT_SECONDS,
        )
    }

    pub fn save_config_with_provider(
        &self,
        provider: &str,
        base_url: &str,
        model: &str,
        context_mode: &str,
        timeout_seconds: i64,
    ) -> Result<(), StoreError> {
        self.save_config_with_fallback(
            provider,
            base_url,
            model,
            context_mode,
            timeout_seconds,
            true,
            DEFAULT_FALLBACK_BASE_URL,
            DEFAULT_FALLBACK_MODEL,
            DEFAULT_MODEL_TIMEOUT_SECONDS,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn save_config_with_fallback(
        &self,
        provider: &str,
        base_url: &str,
        model: &str,
        context_mode: &str,
        timeout_seconds: i64,
        fallback_enabled: bool,
        fallback_base_url: &str,
        fallback_model: &str,
        fallback_timeout_seconds: i64,
    ) -> Result<(), StoreError> {
        self.save_config_with_fallback_and_pricing(
            provider,
            base_url,
            model,
            context_mode,
            timeout_seconds,
            fallback_enabled,
            fallback_base_url,
            fallback_model,
            fallback_timeout_seconds,
            DEFAULT_INPUT_PRICE_PER_MILLION_USD,
            DEFAULT_OUTPUT_PRICE_PER_MILLION_USD,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn save_config_with_fallback_and_pricing(
        &self,
        provider: &str,
        base_url: &str,
        model: &str,
        context_mode: &str,
        timeout_seconds: i64,
        fallback_enabled: bool,
        fallback_base_url: &str,
        fallback_model: &str,
        fallback_timeout_seconds: i64,
        input_price_per_million_usd: f64,
        output_price_per_million_usd: f64,
    ) -> Result<(), StoreError> {
        if !matches!(provider, "remote" | "ollama") {
            return Err(StoreError::InvalidSetting("invalid model provider".into()));
        }
        if !(10..=300).contains(&timeout_seconds) {
            return Err(StoreError::InvalidSetting(
                "timeout_seconds must be 10..300".into(),
            ));
        }
        if !(10..=300).contains(&fallback_timeout_seconds) {
            return Err(StoreError::InvalidSetting(
                "fallback_timeout_seconds must be 10..300".into(),
            ));
        }
        if fallback_enabled
            && (fallback_base_url.trim().is_empty() || fallback_model.trim().is_empty())
        {
            return Err(StoreError::InvalidSetting(
                "fallback URL and model are required when enabled".into(),
            ));
        }
        if !input_price_per_million_usd.is_finite()
            || !(0.0..=10_000.0).contains(&input_price_per_million_usd)
            || !output_price_per_million_usd.is_finite()
            || !(0.0..=10_000.0).contains(&output_price_per_million_usd)
        {
            return Err(StoreError::InvalidSetting(
                "model prices must be finite values between 0 and 10000 USD per million tokens"
                    .into(),
            ));
        }
        let values = vec![
            ("provider", provider.to_string()),
            ("base_url", base_url.to_string()),
            ("model", model.to_string()),
            ("context_mode", context_mode.to_string()),
            ("timeout_seconds", timeout_seconds.to_string()),
            ("fallback_enabled", fallback_enabled.to_string()),
            ("fallback_base_url", fallback_base_url.to_string()),
            ("fallback_model", fallback_model.to_string()),
            (
                "fallback_timeout_seconds",
                fallback_timeout_seconds.to_string(),
            ),
            (
                "input_price_per_million_usd",
                input_price_per_million_usd.to_string(),
            ),
            (
                "output_price_per_million_usd",
                output_price_per_million_usd.to_string(),
            ),
        ];
        for (key, value) in values {
            self.conn.execute(
                "INSERT INTO config(key,value) VALUES (?1,?2)
                 ON CONFLICT(key) DO UPDATE SET value=excluded.value",
                params![key, value],
            )?;
        }
        Ok(())
    }

    pub fn mark_model_health(&self, status: &str, error: Option<&str>) -> Result<(), StoreError> {
        let read = |key: &str| -> Result<String, rusqlite::Error> {
            Ok(self
                .conn
                .query_row("SELECT value FROM config WHERE key=?1", [key], |row| {
                    row.get(0)
                })
                .optional()?
                .unwrap_or_default())
        };
        self.mark_model_health_for(
            status,
            error,
            &read("provider")?,
            &read("base_url")?,
            &read("model")?,
        )
    }

    pub fn mark_model_health_for(
        &self,
        status: &str,
        error: Option<&str>,
        provider: &str,
        base_url: &str,
        model: &str,
    ) -> Result<(), StoreError> {
        let sanitized_error = error.map(sanitize_stored_error).unwrap_or_default();
        for (key, value) in [
            ("health_status", status.to_string()),
            ("health_error", sanitized_error),
            ("health_provider", provider.to_string()),
            ("health_base_url", base_url.to_string()),
            ("health_model", model.to_string()),
            (
                "last_checked_at",
                chrono::Utc::now().timestamp().to_string(),
            ),
        ] {
            self.conn.execute(
                "INSERT INTO config(key,value) VALUES (?1,?2)
                 ON CONFLICT(key) DO UPDATE SET value=excluded.value",
                params![key, value],
            )?;
        }
        Ok(())
    }

    pub fn load_config_values(&self) -> Result<(String, String), StoreError> {
        let view = self.config()?;
        Ok((view.base_url, view.model))
    }

    pub fn _default_path() -> std::path::PathBuf {
        paths::store_path()
    }
}

fn write_reflection_result(
    tx: &Transaction<'_>,
    result: &ReflectionRunResult,
) -> Result<(), StoreError> {
    for entry in &result.generated {
        write_entry_snapshot(tx, entry, "generated")?;
        tx.execute(
            "INSERT INTO audit_log(occurred_at, action, object_id, detail_json)
             VALUES (unixepoch(), 'evolution_entry_generated', ?1, ?2)",
            params![
                entry.id,
                serde_json::to_string(&serde_json::json!({
                    "status": entry.status,
                    "risk": entry.risk,
                    "sources": entry.source_refs.len()
                }))?
            ],
        )?;
    }
    for verification in &result.candidate_verifications {
        tx.execute(
            "INSERT OR REPLACE INTO candidate_verifications
             (run_id, entry_id, evidence_sufficient, supporting_evidence_json,
              contradicting_evidence_json, confidence, duplicate, conflict,
              recommendation, rationale)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                verification.run_id,
                verification.entry_id,
                verification.evidence_sufficient,
                serde_json::to_string(&verification.supporting_evidence)?,
                serde_json::to_string(&verification.contradicting_evidence)?,
                verification.confidence,
                verification.duplicate,
                verification.conflict,
                verification.recommendation,
                verification.rationale,
            ],
        )?;
    }
    tx.execute(
        "INSERT INTO reflection_runs(id, occurred_at, generated, activated, pending, discarded)
         VALUES (?1, unixepoch(), ?2, ?3, ?4, ?5)",
        params![
            result.run_id,
            result.generated.len() as i64,
            result.activated,
            result.pending,
            result.discarded
        ],
    )?;
    Ok(())
}

fn apply_entry_version(
    tx: &Transaction<'_>,
    entry_id: &str,
    version: i64,
    action: &str,
    source_run_id: Option<&str>,
    reviewer: Option<&str>,
    reason: Option<&str>,
) -> Result<(), StoreError> {
    let saved = tx
        .query_row(
            "SELECT kind, title, summary, body, risk, source_refs_json,
                    origin_run_id, target_entry_id
             FROM entry_versions WHERE entry_id=?1 AND version=?2",
            params![entry_id, version],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, Option<String>>(6)?,
                    row.get::<_, Option<String>>(7)?,
                ))
            },
        )
        .optional()?
        .ok_or_else(|| StoreError::InvalidSetting("entry version not found".into()))?;
    let changed = tx.execute(
        "UPDATE entries SET kind=?1, title=?2, summary=?3, body=?4, status='active',
                risk=?5, source_refs_json=?6, updated_at=unixepoch(),
                origin_run_id=?7, target_entry_id=?8, version=version+1
         WHERE id=?9",
        params![saved.0, saved.1, saved.2, saved.3, saved.4, saved.5, saved.6, saved.7, entry_id],
    )?;
    if changed == 0 {
        return Err(StoreError::InvalidSetting("entry not found".into()));
    }
    snapshot_current_entry_with_context(tx, entry_id, action, source_run_id, reviewer, reason)
}

fn write_entry_snapshot(
    tx: &Transaction<'_>,
    entry: &EvolutionEntry,
    action: &str,
) -> Result<(), StoreError> {
    let previous: Option<i64> = tx
        .query_row(
            "SELECT version FROM entries WHERE id=?1",
            [&entry.id],
            |row| row.get(0),
        )
        .optional()?;
    let version = previous
        .map(|value| value + 1)
        .unwrap_or(entry.version.max(1));
    tx.execute(
        "INSERT INTO entries
         (id, kind, title, summary, body, status, risk, source_refs_json, updated_at,
          origin_run_id, target_entry_id, version)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
         ON CONFLICT(id) DO UPDATE SET kind=excluded.kind, title=excluded.title,
         summary=excluded.summary, body=excluded.body, status=excluded.status,
         risk=excluded.risk, source_refs_json=excluded.source_refs_json,
         updated_at=excluded.updated_at, origin_run_id=excluded.origin_run_id,
         target_entry_id=excluded.target_entry_id, version=excluded.version",
        params![
            entry.id,
            entry.kind,
            entry.title,
            entry.summary,
            entry.body,
            entry.status,
            entry.risk,
            serde_json::to_string(&entry.source_refs)?,
            entry.updated_at,
            entry.origin_run_id,
            entry.target_entry_id,
            version
        ],
    )?;
    snapshot_current_entry_with_context(
        tx,
        &entry.id,
        action,
        entry.origin_run_id.as_deref(),
        None,
        None,
    )
}

fn snapshot_current_entry_with_context(
    tx: &Transaction<'_>,
    entry_id: &str,
    action: &str,
    source_run_id: Option<&str>,
    reviewer: Option<&str>,
    review_reason: Option<&str>,
) -> Result<(), StoreError> {
    tx.execute(
        "INSERT OR IGNORE INTO entry_versions
         (entry_id, version, kind, title, summary, body, status, risk, source_refs_json,
          origin_run_id, target_entry_id, created_at, action, source_run_id, reviewer,
          review_reason, reviewed_at)
         SELECT id, version, kind, title, summary, body, status, risk, source_refs_json,
                origin_run_id, target_entry_id, unixepoch(), ?2, ?3, ?4, ?5,
                CASE WHEN ?4 IS NULL THEN NULL ELSE unixepoch() END
         FROM entries WHERE id=?1",
        params![entry_id, action, source_run_id, reviewer, review_reason],
    )?;
    Ok(())
}

fn read_evolution_run(row: &Row<'_>) -> rusqlite::Result<EvolutionRunState> {
    let providers_json: String = row.get(12)?;
    Ok(EvolutionRunState {
        run_id: row.get(0)?,
        mode: row.get(1)?,
        phase: row.get(2)?,
        started_at: row.get(3)?,
        completed_at: row.get(4)?,
        scanned_activities: row.get(5)?,
        consumed_activities: row.get(6)?,
        generated: row.get(7)?,
        activated: row.get(8)?,
        pending: row.get(9)?,
        error: row.get(10)?,
        model: row.get(11)?,
        providers: serde_json::from_str(&providers_json).unwrap_or_default(),
        lookback_days: row.get(13)?,
        rolled_back_at: row.get(14)?,
        agent_mode: row
            .get::<_, Option<String>>(15)?
            .unwrap_or_else(|| DEFAULT_AGENT_MODE.to_string()),
        trace_count: row.get::<_, Option<i64>>(16)?.unwrap_or_default(),
        verification_status: row
            .get::<_, Option<String>>(17)?
            .unwrap_or_else(|| "not_run".to_string()),
        verification_summary: row.get(18)?,
        retry_of_run_id: row.get(19)?,
        provider_used: row.get(20)?,
        fallback_count: row.get::<_, Option<i64>>(21)?.unwrap_or_default(),
        input_activity_count: row.get::<_, Option<i64>>(22)?.unwrap_or_default(),
        input_tokens: row.get::<_, Option<i64>>(23)?.unwrap_or_default(),
        output_tokens: row.get::<_, Option<i64>>(24)?.unwrap_or_default(),
        duration_ms: row.get::<_, Option<i64>>(25)?.unwrap_or_default(),
        estimated_cost_usd: row.get(26)?,
    })
}

fn run_migrations(conn: &mut Connection) -> Result<(), StoreError> {
    let current: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if current > CURRENT_SCHEMA_VERSION {
        return Err(StoreError::InvalidSetting(format!(
            "database schema {} is newer than supported {}",
            current, CURRENT_SCHEMA_VERSION
        )));
    }
    for version in (current + 1)..=CURRENT_SCHEMA_VERSION {
        let tx = conn.transaction()?;
        match version {
            1 => {
                add_column_if_missing_tx(&tx, "activities", "reflected_at", "INTEGER")?;
                add_column_if_missing_tx(&tx, "entries", "origin_run_id", "TEXT")?;
                add_column_if_missing_tx(&tx, "entries", "target_entry_id", "TEXT")?;
                add_column_if_missing_tx(&tx, "entries", "version", "INTEGER NOT NULL DEFAULT 1")?;
                for (column, definition) in [
                    ("source_run_id", "TEXT"),
                    ("reviewer", "TEXT"),
                    ("review_reason", "TEXT"),
                    ("reviewed_at", "INTEGER"),
                ] {
                    add_column_if_missing_tx(&tx, "entry_versions", column, definition)?;
                }
                add_column_if_missing_tx(
                    &tx,
                    "scan_cursors",
                    "oldest_activity_at",
                    "INTEGER NOT NULL DEFAULT 0",
                )?;
            }
            2 => {
                for (column, definition) in [
                    ("model", "TEXT"),
                    ("providers_json", "TEXT NOT NULL DEFAULT '[]'"),
                    ("lookback_days", "INTEGER NOT NULL DEFAULT 30"),
                    ("rolled_back_at", "INTEGER"),
                    ("agent_mode", "TEXT NOT NULL DEFAULT 'reflection'"),
                    ("trace_count", "INTEGER NOT NULL DEFAULT 0"),
                    ("verification_status", "TEXT NOT NULL DEFAULT 'not_run'"),
                    ("verification_summary", "TEXT"),
                ] {
                    add_column_if_missing_tx(&tx, "evolution_runs", column, definition)?;
                }
            }
            3 => {
                for (column, definition) in [
                    ("retry_of_run_id", "TEXT"),
                    ("runner_config_json", "TEXT NOT NULL DEFAULT '{}'"),
                    ("provider_used", "TEXT"),
                    ("fallback_count", "INTEGER NOT NULL DEFAULT 0"),
                    ("input_activity_count", "INTEGER NOT NULL DEFAULT 0"),
                    ("input_tokens", "INTEGER NOT NULL DEFAULT 0"),
                    ("output_tokens", "INTEGER NOT NULL DEFAULT 0"),
                    ("model_duration_ms", "INTEGER NOT NULL DEFAULT 0"),
                    ("estimated_cost_usd", "REAL"),
                ] {
                    add_column_if_missing_tx(&tx, "evolution_runs", column, definition)?;
                }
            }
            4 => {
                tx.execute(
                    "INSERT OR IGNORE INTO entry_versions
                     (entry_id, version, kind, title, summary, body, status, risk, source_refs_json,
                      origin_run_id, target_entry_id, created_at, action, source_run_id)
                     SELECT id, version, kind, title, summary, body, status, risk, source_refs_json,
                            origin_run_id, target_entry_id, updated_at, 'migration', origin_run_id
                     FROM entries",
                    [],
                )?;
            }
            _ => unreachable!(),
        }
        tx.execute(
            "INSERT OR REPLACE INTO schema_migrations(version, applied_at)
             VALUES (?1, unixepoch())",
            [version],
        )?;
        tx.commit()?;
        conn.pragma_update(None, "user_version", version)?;
    }
    Ok(())
}

fn add_column_if_missing_tx(
    tx: &rusqlite::Transaction<'_>,
    table: &str,
    column: &str,
    definition: &str,
) -> Result<(), StoreError> {
    let exists: bool = tx.query_row(
        &format!("SELECT EXISTS(SELECT 1 FROM pragma_table_info('{table}') WHERE name=?1)"),
        [column],
        |row| row.get(0),
    )?;
    if !exists {
        tx.execute(
            &format!("ALTER TABLE {table} ADD COLUMN {column} {definition}"),
            [],
        )?;
    }
    Ok(())
}

fn has_column(conn: &Connection, table: &str, column: &str) -> Result<bool, rusqlite::Error> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(1))?;
    for name in rows.flatten() {
        if name == column {
            return Ok(true);
        }
    }
    Ok(false)
}

fn sanitize_export_paths(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::String(text) => {
            static LOCAL_PATHS: OnceLock<Regex> = OnceLock::new();
            let paths = LOCAL_PATHS.get_or_init(|| {
                Regex::new(
                    r#"(?x)(?:file://)?(?:/(?:Users|home|private|var|tmp|Volumes)/[^\s\"'<>]+|[A-Za-z]:\\(?:Users|Documents\ and\ Settings)\\[^\s\"'<>]+)"#,
                )
                .expect("export path redaction regex must compile")
            });
            *text = paths.replace_all(text, "[REDACTED_LOCAL_PATH]").to_string();
        }
        serde_json::Value::Array(items) => {
            for item in items {
                sanitize_export_paths(item);
            }
        }
        serde_json::Value::Object(map) => {
            for item in map.values_mut() {
                sanitize_export_paths(item);
            }
        }
        _ => {}
    }
}

fn restrict_file_permissions(path: &Path) -> Result<(), StoreError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

fn sanitize_stored_error(value: &str) -> String {
    crate::scanner::redact(value)
        .replace('\0', "")
        .chars()
        .take(500)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{sanitize_export_paths, Store, CURRENT_SCHEMA_VERSION};
    use crate::models::{
        Activity, CandidateVerification, EvolutionEntry, EvolutionSettingsInput,
        ReflectionRunResult, RunnerConfigSnapshot,
    };
    use serde_json::json;
    use std::path::Path;
    use tempfile::tempdir;

    #[test]
    fn redacted_exports_remove_common_absolute_local_paths() {
        let mut payload = json!({
            "mac": "failed at /Users/alice/project/file.rs:12",
            "temp": "cache /private/var/folders/aa/token.json",
            "volume": "opened file:///Volumes/Secret/data.json",
            "windows": r"C:\Users\alice\project\secret.txt",
            "remote": "https://api.example/v1"
        });
        sanitize_export_paths(&mut payload);
        let text = payload.to_string();
        assert!(!text.contains("alice"));
        assert!(!text.contains("/private/var"));
        assert!(!text.contains("/Volumes"));
        assert!(text.contains("https://api.example/v1"));
        assert!(text.matches("[REDACTED_LOCAL_PATH]").count() >= 4);
    }

    #[test]
    fn context_mode_defaults_to_guided_and_persists_mcp() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("store.sqlite3");
        {
            let store = Store::open(path.clone()).unwrap();
            assert_eq!(store.config().unwrap().context_mode, "guided");
            store
                .save_config("https://example.test", "model", "mcp")
                .unwrap();
        }
        let reopened = Store::open(path).unwrap();
        assert_eq!(reopened.config().unwrap().context_mode, "mcp");
    }

    #[test]
    fn schema_migrations_are_versioned_and_recorded() {
        let dir = tempdir().unwrap();
        let store = Store::open(dir.path().join("store.sqlite3")).unwrap();
        let version: i64 = store
            .conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, CURRENT_SCHEMA_VERSION);
        let applied: i64 = store
            .conn
            .query_row("SELECT COUNT(*) FROM schema_migrations", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(applied, CURRENT_SCHEMA_VERSION);
    }

    #[test]
    fn model_health_is_scoped_to_the_tested_configuration() {
        let dir = tempdir().unwrap();
        let store = Store::open(dir.path().join("store.sqlite3")).unwrap();
        store
            .save_config_with_provider(
                "remote",
                "https://api.example.test/v1",
                "model-a",
                "guided",
                90,
            )
            .unwrap();
        store
            .mark_model_health_for(
                "ok",
                None,
                "remote",
                "https://other.example.test/v1",
                "model-b",
            )
            .unwrap();
        assert_eq!(store.config().unwrap().health_status, "unknown");
        store
            .mark_model_health_for(
                "ok",
                None,
                "remote",
                "https://api.example.test/v1",
                "model-a",
            )
            .unwrap();
        assert_eq!(store.config().unwrap().health_status, "ok");
    }

    #[test]
    fn persisted_model_errors_are_redacted_and_bounded() {
        let dir = tempdir().unwrap();
        let store = Store::open(dir.path().join("store.sqlite3")).unwrap();
        store
            .save_config_with_provider(
                "remote",
                "https://api.example.test/v1",
                "model-a",
                "guided",
                90,
            )
            .unwrap();
        let secret = format!(
            "HTTP 401 api_key=super-secret /Users/alice/project {}",
            "x".repeat(800)
        );
        store
            .mark_model_health_for(
                "error",
                Some(&secret),
                "remote",
                "https://api.example.test/v1",
                "model-a",
            )
            .unwrap();
        let error = store.config().unwrap().health_error.unwrap();
        assert!(!error.contains("super-secret"));
        assert!(!error.contains("/Users/alice"));
        assert!(error.len() <= 500);
    }

    #[test]
    fn model_pricing_round_trips_with_legacy_defaults() {
        let dir = tempdir().unwrap();
        let store = Store::open(dir.path().join("store.sqlite3")).unwrap();
        store
            .save_config_with_fallback_and_pricing(
                "remote",
                "https://api.example.test/v1",
                "model-a",
                "guided",
                90,
                true,
                "http://127.0.0.1:11434/v1",
                "qwen3:8b",
                90,
                1.25,
                5.0,
            )
            .unwrap();
        let config = store.config().unwrap();
        assert_eq!(config.input_price_per_million_usd, 1.25);
        assert_eq!(config.output_price_per_million_usd, 5.0);
    }

    fn activity(id: &str) -> Activity {
        Activity {
            id: id.to_string(),
            provider: "codex".to_string(),
            session_id: "session-1".to_string(),
            source_path: "codex:rollout#abc".to_string(),
            kind: "user_message".to_string(),
            role: "user".to_string(),
            text: "Use stable IDs".to_string(),
            occurred_at: 42,
            metadata: json!({"source":"fixture"}),
        }
    }

    #[test]
    fn dirty_activities_are_consumed_once_and_audited() {
        let dir = tempdir().unwrap();
        let store = Store::open(dir.path().join("store.sqlite3")).unwrap();
        assert!(store.upsert_activity(&activity("a1")).unwrap());
        assert!(!store.upsert_activity(&activity("a1")).unwrap());
        assert_eq!(store.dirty_count().unwrap(), 1);
        assert_eq!(store.activities_for_reflection(10).unwrap().len(), 1);

        store
            .mark_activities_reflected(&["a1".to_string()], 100)
            .unwrap();
        store
            .append_audit(
                "reflection_completed",
                Some("run-1"),
                &json!({"consumed":1}),
            )
            .unwrap();
        assert_eq!(store.dirty_count().unwrap(), 0);
        assert!(store.activities_for_reflection(10).unwrap().is_empty());

        let count: i64 = store
            .conn
            .query_row("SELECT COUNT(*) FROM audit_log", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn evolution_run_keeps_its_exact_activity_batch() {
        let dir = tempdir().unwrap();
        let store = Store::open(dir.path().join("store.sqlite3")).unwrap();
        let first = activity("a1");
        let mut second = activity("a2");
        second.session_id = "session-2".into();
        store.upsert_activity(&first).unwrap();
        store.upsert_activity(&second).unwrap();
        store.start_evolution_run("run-1", "manual").unwrap();
        store
            .set_evolution_run_activities("run-1", &[second, first])
            .unwrap();

        let saved = store.evolution_run_activities("run-1").unwrap();
        assert_eq!(
            saved
                .iter()
                .map(|item| item.id.as_str())
                .collect::<Vec<_>>(),
            vec!["a2", "a1"]
        );
    }

    #[test]
    fn scan_cursor_tracks_unchanged_files() {
        let dir = tempdir().unwrap();
        let store = Store::open(dir.path().join("store.sqlite3")).unwrap();
        assert!(store.scan_cursor("source-1").unwrap().is_none());
        store
            .save_scan_cursor("source-1", "codex", 120, 99, 88)
            .unwrap();
        let cursor = store.scan_cursor("source-1").unwrap().unwrap();
        assert_eq!(cursor.size, 120);
        assert_eq!(cursor.modified_at, 99);
        assert_eq!(cursor.oldest_activity_at, 88);
    }

    #[test]
    fn evolution_settings_default_validate_and_persist() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("store.sqlite3");
        let store = Store::open(path.clone()).unwrap();
        let defaults = store.evolution_settings().unwrap();
        assert_eq!(defaults.lookback_days, 30);
        assert_eq!(defaults.run_mode, "manual");
        assert!(defaults.auto_activate_low_risk);

        let input = EvolutionSettingsInput {
            enabled: true,
            codex_enabled: true,
            claude_enabled: false,
            lookback_days: 7,
            run_mode: "scheduled".into(),
            schedule_hours: 6,
            listen_since: None,
            auto_activate_low_risk: false,
            max_agent_steps: 4,
            launch_at_login: true,
            notifications_enabled: false,
            agent_mode: "verification".into(),
        };
        let saved = store.save_evolution_settings(&input).unwrap();
        assert_eq!(saved.lookback_days, 7);
        assert_eq!(saved.schedule_hours, 6);
        assert!(!saved.claude_enabled);
        drop(store);
        let reopened = Store::open(path).unwrap();
        assert_eq!(reopened.evolution_settings().unwrap().run_mode, "scheduled");

        let mut invalid = input;
        invalid.lookback_days = 2;
        assert!(reopened.save_evolution_settings(&invalid).is_err());
    }

    #[test]
    fn approved_revision_updates_active_target_transactionally() {
        let dir = tempdir().unwrap();
        let store = Store::open(dir.path().join("store.sqlite3")).unwrap();
        let entry =
            |id: &str, kind: &str, body: &str, status: &str, target: Option<&str>| EvolutionEntry {
                id: id.into(),
                kind: kind.into(),
                title: id.into(),
                summary: "summary".into(),
                body: body.into(),
                status: status.into(),
                risk: if kind == "revision" { "high" } else { "low" }.into(),
                source_refs: vec!["a1".into(), "a2".into()],
                updated_at: 1,
                origin_run_id: Some("run-1".into()),
                target_entry_id: target.map(str::to_string),
                version: 1,
            };
        store
            .insert_entry(&entry("meta-1", "meta", "old body", "active", None))
            .unwrap();
        store
            .insert_entry(&entry(
                "revision-1",
                "revision",
                "new body",
                "pending",
                Some("meta-1"),
            ))
            .unwrap();
        store.set_entry_status("revision-1", "active").unwrap();
        let entries = store.list_entries().unwrap();
        assert_eq!(
            entries
                .iter()
                .find(|entry| entry.id == "meta-1")
                .unwrap()
                .body,
            "new body"
        );
        assert_eq!(
            entries
                .iter()
                .find(|entry| entry.id == "revision-1")
                .unwrap()
                .status,
            "active"
        );
    }

    #[test]
    fn evolution_run_state_is_persisted() {
        let dir = tempdir().unwrap();
        let store = Store::open(dir.path().join("store.sqlite3")).unwrap();
        store.start_evolution_run("run-1", "manual").unwrap();
        store
            .update_evolution_run("run-1", "completed", 9, 8, 2, 1, 1, None)
            .unwrap();
        let run = store.current_evolution_run().unwrap().unwrap();
        assert_eq!(run.phase, "completed");
        assert_eq!(run.consumed_activities, 8);
        assert!(run.completed_at.is_some());
    }

    #[test]
    fn retry_configuration_snapshot_is_immutable_and_linked() {
        let dir = tempdir().unwrap();
        let store = Store::open(dir.path().join("store.sqlite3")).unwrap();
        let snapshot = RunnerConfigSnapshot {
            provider: "remote".into(),
            base_url: "https://model.example/v1".into(),
            model: "model-a".into(),
            timeout_seconds: 45,
            agent_mode: "verification".into(),
            auto_activate_low_risk: false,
            max_agent_steps: 7,
            fallback_enabled: true,
            fallback_base_url: "http://127.0.0.1:11435/v1".into(),
            fallback_model: "local-a".into(),
            fallback_timeout_seconds: 30,
            input_price_per_million_usd: 1.25,
            output_price_per_million_usd: 5.0,
        };
        store
            .start_evolution_run_with_snapshot(
                "run-retry",
                "manual",
                Some("model-a"),
                &["codex".into()],
                7,
                &snapshot,
                Some("run-source"),
            )
            .unwrap();
        store
            .save_config_with_provider(
                "remote",
                "https://changed.example/v1",
                "model-b",
                "guided",
                90,
            )
            .unwrap();

        let saved = store
            .evolution_run_config_snapshot("run-retry")
            .unwrap()
            .unwrap();
        let run = store.current_evolution_run().unwrap().unwrap();
        assert_eq!(saved.base_url, "https://model.example/v1");
        assert_eq!(saved.model, "model-a");
        assert_eq!(saved.agent_mode, "verification");
        assert!(!saved.auto_activate_low_risk);
        assert_eq!(run.retry_of_run_id.as_deref(), Some("run-source"));
    }

    #[test]
    fn legacy_retry_snapshot_defaults_missing_pricing() {
        let snapshot: RunnerConfigSnapshot = serde_json::from_value(json!({
            "provider": "remote",
            "baseUrl": "https://model.example/v1",
            "model": "model-a",
            "timeoutSeconds": 45,
            "agentMode": "reflection",
            "autoActivateLowRisk": false,
            "maxAgentSteps": 6,
            "fallbackEnabled": true,
            "fallbackBaseUrl": "http://127.0.0.1:11434/v1",
            "fallbackModel": "qwen3:8b",
            "fallbackTimeoutSeconds": 30
        }))
        .unwrap();
        assert_eq!(snapshot.input_price_per_million_usd, 0.0);
        assert_eq!(snapshot.output_price_per_million_usd, 0.0);
    }

    #[test]
    fn candidate_verifications_persist_with_the_run() {
        let dir = tempdir().unwrap();
        let store = Store::open(dir.path().join("store.sqlite3")).unwrap();
        let entry = EvolutionEntry {
            id: "entry-verified".into(),
            kind: "skill".into(),
            title: "Verify before activation".into(),
            summary: "Use redacted evidence".into(),
            body: "Read the evidence and check conflicts.".into(),
            status: "pending".into(),
            risk: "review".into(),
            source_refs: vec!["activity-1".into()],
            updated_at: 1,
            origin_run_id: Some("run-verified".into()),
            target_entry_id: None,
            version: 1,
        };
        let result = ReflectionRunResult {
            run_id: "run-verified".into(),
            generated: vec![entry],
            activated: 0,
            pending: 1,
            discarded: 0,
            message: "done".into(),
            verification_status: "review_required".into(),
            verification_summary: Some("one candidate".into()),
            candidate_verifications: vec![CandidateVerification {
                run_id: "run-verified".into(),
                entry_id: "entry-verified".into(),
                evidence_sufficient: true,
                supporting_evidence: vec!["activity-1".into()],
                contradicting_evidence: Vec::new(),
                confidence: 0.88,
                duplicate: false,
                conflict: false,
                recommendation: "review".into(),
                rationale: "single source".into(),
            }],
            provider_used: "remote".into(),
            fallback_count: 0,
            input_activity_count: 1,
            input_tokens: 12,
            output_tokens: 8,
            duration_ms: 20,
            estimated_cost_usd: None,
        };
        store.persist_reflection_result(&result).unwrap();

        let saved = store
            .candidate_verifications_for_run("run-verified")
            .unwrap();
        assert_eq!(saved.len(), 1);
        assert_eq!(saved[0].entry_id, "entry-verified");
        assert_eq!(saved[0].supporting_evidence, vec!["activity-1"]);
        assert_eq!(saved[0].confidence, 0.88);
    }

    #[test]
    fn agent_trace_summary_is_redacted_bounded_and_counted() {
        let dir = tempdir().unwrap();
        let store = Store::open(dir.path().join("store.sqlite3")).unwrap();
        store.start_evolution_run("run-trace", "manual").unwrap();
        let secret = format!(
            "api_key=super-secret-value /Users/private-user/project {}",
            "x".repeat(700)
        );
        let saved = store
            .append_trace_event(
                "run-trace",
                "analyzing",
                "tool_call",
                Some("read_activity_batch"),
                &secret,
                Some(12),
                "ok",
                None,
            )
            .unwrap();

        assert!(saved.summary.contains("[REDACTED_API_KEY]"));
        assert!(!saved.summary.contains("super-secret-value"));
        assert!(!saved.summary.contains("private-user"));
        assert!(saved.summary.chars().count() <= 500);
        assert_eq!(store.list_trace_events("run-trace", 20).unwrap().len(), 1);
        assert_eq!(
            store.current_evolution_run().unwrap().unwrap().trace_count,
            1
        );
    }

    #[test]
    fn entry_versions_are_immutable_and_rollback_creates_a_new_version() {
        let dir = tempdir().unwrap();
        let store = Store::open(dir.path().join("store.sqlite3")).unwrap();
        let mut entry = EvolutionEntry {
            id: "skill-1".into(),
            kind: "skill".into(),
            title: "Stable workflow".into(),
            summary: "first".into(),
            body: "v1 body".into(),
            status: "active".into(),
            risk: "low".into(),
            source_refs: vec!["a1".into(), "a2".into()],
            updated_at: 1,
            origin_run_id: Some("run-1".into()),
            target_entry_id: None,
            version: 1,
        };
        store.insert_entry(&entry).unwrap();
        entry.summary = "second".into();
        entry.body = "v2 body".into();
        entry.updated_at = 2;
        store.insert_entry(&entry).unwrap();

        let versions = store.list_entry_versions("skill-1").unwrap();
        assert_eq!(versions.len(), 2);
        assert_eq!(versions[0].version, 2);
        assert_eq!(versions[1].body, "v1 body");
        let diff = store.entry_version_diff("skill-1", Some(1), 2).unwrap();
        assert!(diff.changed);

        store.rollback_entry("skill-1", 1).unwrap();
        let current = store
            .list_entries()
            .unwrap()
            .into_iter()
            .find(|value| value.id == "skill-1")
            .unwrap();
        assert_eq!(current.version, 3);
        assert_eq!(current.body, "v1 body");
        assert_eq!(current.status, "active");
        assert_eq!(store.list_entry_versions("skill-1").unwrap().len(), 3);
    }

    #[test]
    fn unfinished_runs_recover_without_consuming_activities() {
        let dir = tempdir().unwrap();
        let store = Store::open(dir.path().join("store.sqlite3")).unwrap();
        store.upsert_activity(&activity("a1")).unwrap();
        store.start_evolution_run("run-1", "manual").unwrap();
        store
            .update_evolution_run("run-1", "analyzing", 1, 0, 0, 0, 0, None)
            .unwrap();

        assert_eq!(store.recover_interrupted_runs().unwrap(), 1);
        let run = store.current_evolution_run().unwrap().unwrap();
        assert_eq!(run.phase, "interrupted");
        assert_eq!(store.dirty_count().unwrap(), 1);
    }

    #[test]
    fn clearing_reflected_cache_preserves_entries_and_versions() {
        let dir = tempdir().unwrap();
        let store = Store::open(dir.path().join("store.sqlite3")).unwrap();
        store.upsert_activity(&activity("a1")).unwrap();
        store
            .mark_activities_reflected(&["a1".into()], 100)
            .unwrap();
        let entry = EvolutionEntry {
            id: "meta-1".into(),
            kind: "meta".into(),
            title: "Preference".into(),
            summary: "summary".into(),
            body: "body".into(),
            status: "active".into(),
            risk: "low".into(),
            source_refs: vec!["a1".into()],
            updated_at: 1,
            origin_run_id: None,
            target_entry_id: None,
            version: 1,
        };
        store.insert_entry(&entry).unwrap();

        let result = store.clear_reflected_activity_cache().unwrap();
        assert_eq!(result.affected, 1);
        assert_eq!(store.activity_count().unwrap(), 0);
        assert_eq!(store.list_entries().unwrap().len(), 1);
        assert_eq!(store.list_entry_versions("meta-1").unwrap().len(), 1);
    }

    #[test]
    fn run_context_and_source_scan_health_are_persisted() {
        let dir = tempdir().unwrap();
        let store = Store::open(dir.path().join("store.sqlite3")).unwrap();
        store
            .start_evolution_run_with_context(
                "run-context",
                "manual",
                Some("qwen3:8b"),
                &["codex".into(), "claude-code".into()],
                7,
            )
            .unwrap();
        let run = store.current_evolution_run().unwrap().unwrap();
        assert_eq!(run.model.as_deref(), Some("qwen3:8b"));
        assert_eq!(run.providers, vec!["codex", "claude-code"]);
        assert_eq!(run.lookback_days, 7);

        store
            .save_scan_cursor("cursor-1", "codex", 10, 20, 15)
            .unwrap();
        store.record_source_scan("codex", 3).unwrap();
        let (last_scan, errors, cursors) = store.source_scan_health("codex").unwrap();
        assert!(last_scan.is_some());
        assert_eq!(errors, 3);
        assert_eq!(cursors, 1);
    }

    #[test]
    fn redaction_report_and_cleanup_preview_are_exact() {
        let dir = tempdir().unwrap();
        let store = Store::open(dir.path().join("store.sqlite3")).unwrap();
        let mut redacted = activity("redacted");
        redacted.text = "mail [REDACTED_EMAIL] key [REDACTED_API_KEY]".into();
        store.upsert_activity(&redacted).unwrap();
        store.start_evolution_run("run-preview", "manual").unwrap();
        store
            .set_evolution_run_activities("run-preview", &[redacted])
            .unwrap();
        store
            .mark_activities_reflected(&["redacted".into()], 100)
            .unwrap();

        let report = store.redaction_report().unwrap();
        assert_eq!(report.processed_records, 1);
        assert_eq!(report.redacted_records, 1);
        assert_eq!(report.redaction_count, 2);
        let preview = store.cache_cleanup_preview().unwrap();
        assert_eq!(preview.reflected_activities, 1);
        assert_eq!(preview.run_activity_links, 1);
        assert_eq!(preview.affected_runs, 1);
    }

    #[test]
    fn whole_run_rollback_restores_revision_target_and_disables_outputs() {
        let dir = tempdir().unwrap();
        let store = Store::open(dir.path().join("store.sqlite3")).unwrap();
        let entry =
            |id: &str, kind: &str, body: &str, status: &str, run: &str, target: Option<&str>| {
                EvolutionEntry {
                    id: id.into(),
                    kind: kind.into(),
                    title: id.into(),
                    summary: "summary".into(),
                    body: body.into(),
                    status: status.into(),
                    risk: if kind == "revision" { "high" } else { "low" }.into(),
                    source_refs: vec!["a1".into(), "a2".into()],
                    updated_at: 1,
                    origin_run_id: Some(run.into()),
                    target_entry_id: target.map(str::to_string),
                    version: 1,
                }
            };
        store
            .insert_entry(&entry(
                "meta-target",
                "meta",
                "old body",
                "active",
                "run-base",
                None,
            ))
            .unwrap();
        store.start_evolution_run("run-change", "manual").unwrap();
        store
            .update_evolution_run("run-change", "completed", 1, 1, 1, 0, 1, None)
            .unwrap();
        store
            .insert_entry(&entry(
                "revision-change",
                "revision",
                "new body",
                "pending",
                "run-change",
                Some("meta-target"),
            ))
            .unwrap();
        store
            .set_entry_status_with_reason("revision-change", "active", "approved")
            .unwrap();
        assert_eq!(
            store
                .list_entries()
                .unwrap()
                .into_iter()
                .find(|value| value.id == "meta-target")
                .unwrap()
                .body,
            "new body"
        );

        let result = store.rollback_evolution_run("run-change").unwrap();
        assert_eq!(result.restored_entries, 1);
        assert_eq!(result.disabled_entries, 1);
        let entries = store.list_entries().unwrap();
        assert_eq!(
            entries
                .iter()
                .find(|value| value.id == "meta-target")
                .unwrap()
                .body,
            "old body"
        );
        assert_eq!(
            entries
                .iter()
                .find(|value| value.id == "revision-change")
                .unwrap()
                .status,
            "disabled"
        );
        assert!(store.rollback_evolution_run("run-change").is_err());
    }

    #[test]
    fn active_store_backup_restore_is_scoped_and_audited() {
        let dir = tempdir().unwrap();
        let store = Store::open(dir.path().join("store.sqlite3")).unwrap();
        let mut entry = EvolutionEntry {
            id: "meta-backup".into(),
            kind: "meta".into(),
            title: "Preference".into(),
            summary: "before".into(),
            body: "before body".into(),
            status: "active".into(),
            risk: "low".into(),
            source_refs: vec!["a1".into(), "a2".into()],
            updated_at: 1,
            origin_run_id: Some("run-backup".into()),
            target_entry_id: None,
            version: 1,
        };
        store.insert_entry(&entry).unwrap();
        let backup = store.backup_store().unwrap();
        let file_name = Path::new(backup.path.as_deref().unwrap())
            .file_name()
            .unwrap()
            .to_string_lossy()
            .to_string();
        entry.summary = "after".into();
        entry.body = "after body".into();
        entry.updated_at = 2;
        store.insert_entry(&entry).unwrap();

        let restored = store.restore_store_backup(&file_name).unwrap();
        assert_eq!(restored.affected, 1);
        let current = store
            .list_entries()
            .unwrap()
            .into_iter()
            .find(|value| value.id == "meta-backup")
            .unwrap();
        assert_eq!(current.body, "before body");
        assert_eq!(current.version, 1);
        assert_eq!(store.list_entry_versions("meta-backup").unwrap().len(), 1);
        assert!(store.restore_store_backup("../outside.sqlite3").is_err());
        assert!(store
            .list_audit_events(20)
            .unwrap()
            .iter()
            .any(|event| event.action == "active_store_restored"));
    }
}

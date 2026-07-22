use crate::models::{Activity, ScanSummary, SessionSummary, SourceSummary};
use crate::paths;
use crate::store::{Store, StoreError};
use chrono::{DateTime, Utc};
use regex::Regex;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::fs::File;
#[cfg(test)]
use std::io::Cursor;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::sync::OnceLock;
use walkdir::WalkDir;

#[derive(Debug, thiserror::Error)]
pub enum ScanError {
    #[error("store error: {0}")]
    Store(#[from] StoreError),
}

#[derive(Default)]
struct ParsedSession {
    provider: String,
    session_id: String,
    title: String,
    cwd: Option<String>,
    source_path: String,
    activities: Vec<Activity>,
    updated_at: i64,
    error_count: i64,
}

#[cfg(test)]
pub fn scan_sources(store: &Store, days: i64) -> Result<ScanSummary, ScanError> {
    scan_sources_with_options(store, days, true, true, None)
}

pub fn scan_sources_with_options(
    store: &Store,
    days: i64,
    codex_enabled: bool,
    claude_enabled: bool,
    since: Option<i64>,
) -> Result<ScanSummary, ScanError> {
    let cutoff = if days < 0 {
        0
    } else {
        let window = if days == 0 { 30 } else { days };
        Utc::now().timestamp() - window * 86_400
    };
    let mut summary = ScanSummary {
        days,
        sources: Vec::new(),
        sessions: Vec::new(),
        scanned_sessions: 0,
        scanned_activities: 0,
        new_activities: 0,
        skipped_files: 0,
        errors: Vec::new(),
    };

    let activity_cutoff = since.unwrap_or(cutoff);
    let codex_root = paths::codex_home();
    let codex_dirs = [
        codex_root.join("sessions"),
        codex_root.join("archived_sessions"),
    ];
    let mut codex_source = SourceSummary {
        provider: "codex".to_string(),
        root: sanitize_cwd(&codex_root.display().to_string()),
        available: false,
        session_count: 0,
        activity_count: 0,
        error: None,
        last_scanned_at: None,
        error_count: 0,
        cursor_count: 0,
    };
    for root in codex_dirs {
        if !codex_enabled {
            break;
        }
        if root.is_dir() {
            codex_source.available = true;
            scan_directory(
                store,
                "codex",
                &root,
                cutoff,
                activity_cutoff,
                false,
                &mut summary,
                &mut codex_source,
            );
        }
    }
    if codex_enabled {
        finalize_source(store, &mut codex_source, &mut summary);
    }
    summary.sources.push(codex_source);

    let claude_root = paths::claude_home();
    let mut claude_source = SourceSummary {
        provider: "claude-code".to_string(),
        root: sanitize_cwd(&claude_root.display().to_string()),
        available: false,
        session_count: 0,
        activity_count: 0,
        error: None,
        last_scanned_at: None,
        error_count: 0,
        cursor_count: 0,
    };
    let projects = claude_root.join("projects");
    if claude_enabled && projects.is_dir() {
        claude_source.available = true;
        scan_directory(
            store,
            "claude-code",
            &projects,
            cutoff,
            activity_cutoff,
            true,
            &mut summary,
            &mut claude_source,
        );
    }
    if claude_enabled {
        finalize_source(store, &mut claude_source, &mut summary);
    }
    summary.sources.push(claude_source);
    Ok(summary)
}

#[cfg(test)]
pub(crate) fn parse_codex_fixture(input: &str) -> Result<Vec<Activity>, String> {
    parse_codex(
        "codex",
        Path::new("codex-fixture.jsonl"),
        Cursor::new(input),
    )
    .map(|parsed| parsed.activities)
}

#[cfg(test)]
pub(crate) fn parse_claude_fixture(input: &str) -> Result<Vec<Activity>, String> {
    parse_claude(
        "claude-code",
        Path::new("claude-fixture.jsonl"),
        Cursor::new(input),
    )
    .map(|parsed| parsed.activities)
}

fn scan_directory(
    store: &Store,
    provider: &str,
    root: &Path,
    cutoff: i64,
    activity_cutoff: i64,
    skip_agent_dirs: bool,
    summary: &mut ScanSummary,
    source: &mut SourceSummary,
) {
    let iter = WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|entry| {
            if !skip_agent_dirs || !entry.file_type().is_dir() {
                return true;
            }
            !entry.file_name().to_string_lossy().starts_with("agent-")
        });
    for entry in iter.filter_map(Result::ok) {
        if !entry.file_type().is_file()
            || entry.path().extension().and_then(|x| x.to_str()) != Some("jsonl")
        {
            continue;
        }
        let modified = entry
            .metadata()
            .ok()
            .and_then(|meta| meta.modified().ok())
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let size = entry.metadata().map(|meta| meta.len() as i64).unwrap_or(0);
        let source_hash = stable_id("source", &entry.path().display().to_string());
        if cutoff > 0 && modified < cutoff {
            summary.skipped_files += 1;
            continue;
        }
        if store
            .scan_cursor(&source_hash)
            .ok()
            .flatten()
            .map(|cursor| {
                cursor.size == size
                    && cursor.modified_at == modified
                    && (cursor.oldest_activity_at == 0
                        || activity_cutoff >= cursor.oldest_activity_at)
            })
            .unwrap_or(false)
        {
            continue;
        }
        match parse_file(provider, entry.path()) {
            Ok(mut parsed) => {
                if parsed.error_count > 0 {
                    source.error_count += parsed.error_count;
                    let message = format!(
                        "{}: {} 条 JSONL 记录损坏，已跳过",
                        source_label(provider, entry.path()),
                        parsed.error_count
                    );
                    if source.error.is_none() {
                        source.error = Some(message.clone());
                    }
                    summary.errors.push(message);
                }
                parsed
                    .activities
                    .retain(|activity| activity.occurred_at >= activity_cutoff);
                if parsed.activities.is_empty() {
                    summary.skipped_files += 1;
                    continue;
                }
                let session = SessionSummary {
                    id: stable_id(provider, &parsed.session_id),
                    provider: provider.to_string(),
                    title: if parsed.title.is_empty() {
                        "未命名会话".to_string()
                    } else {
                        parsed.title.clone()
                    },
                    source_path: parsed.source_path.clone(),
                    cwd: parsed.cwd.as_deref().map(sanitize_cwd),
                    activity_count: parsed.activities.len() as i64,
                    updated_at: parsed.updated_at,
                };
                if let Err(err) = store.upsert_session(&session) {
                    record_scan_error(summary, source, provider, entry.path(), &err.to_string());
                    continue;
                }
                let inserted = match store.upsert_activities(&parsed.activities) {
                    Ok(inserted) => inserted,
                    Err(err) => {
                        record_scan_error(
                            summary,
                            source,
                            provider,
                            entry.path(),
                            &err.to_string(),
                        );
                        continue;
                    }
                };
                source.session_count += 1;
                source.activity_count += parsed.activities.len() as i64;
                summary.scanned_sessions += 1;
                summary.scanned_activities += parsed.activities.len() as i64;
                summary.new_activities += inserted;
                summary.sessions.push(session);
                let oldest_activity_at = parsed
                    .activities
                    .iter()
                    .map(|activity| activity.occurred_at)
                    .min()
                    .unwrap_or(activity_cutoff);
                if let Err(err) = store.save_scan_cursor(
                    &source_hash,
                    provider,
                    size,
                    modified,
                    oldest_activity_at,
                ) {
                    record_scan_error(summary, source, provider, entry.path(), &err.to_string());
                }
            }
            Err(err) => record_scan_error(summary, source, provider, entry.path(), &err),
        }
    }
}

fn finalize_source(store: &Store, source: &mut SourceSummary, summary: &mut ScanSummary) {
    if let Err(err) = store.record_source_scan(&source.provider, source.error_count) {
        source.error_count += 1;
        if source.error.is_none() {
            source.error = Some(err.to_string());
        }
        summary.errors.push(format!("{}: {err}", source.provider));
    }
    if let Ok((last_scanned_at, error_count, cursor_count)) =
        store.source_scan_health(&source.provider)
    {
        source.last_scanned_at = last_scanned_at;
        source.error_count = error_count;
        source.cursor_count = cursor_count;
    }
    if let Ok((sessions, activities)) = store.provider_totals(&source.provider) {
        source.session_count = sessions;
        source.activity_count = activities;
    }
}

fn record_scan_error(
    summary: &mut ScanSummary,
    source: &mut SourceSummary,
    provider: &str,
    path: &Path,
    error: &str,
) {
    let message = format!("{}: {error}", source_label(provider, path));
    source.error_count += 1;
    if source.error.is_none() {
        source.error = Some(message.clone());
    }
    summary.errors.push(message);
}

fn parse_file(provider: &str, path: &Path) -> Result<ParsedSession, String> {
    let file = File::open(path).map_err(|err| err.to_string())?;
    if provider == "codex" {
        parse_codex(provider, path, BufReader::new(file))
    } else {
        parse_claude(provider, path, BufReader::new(file))
    }
}

fn parse_codex<R: BufRead>(
    provider: &str,
    path: &Path,
    reader: R,
) -> Result<ParsedSession, String> {
    let mut parsed = ParsedSession {
        provider: provider.to_string(),
        session_id: String::new(),
        source_path: source_label(provider, path),
        ..Default::default()
    };
    let mut index = 0usize;
    for line in reader.lines() {
        let line = line.map_err(|err| err.to_string())?;
        let value: Value = match serde_json::from_str(&line) {
            Ok(value) => value,
            Err(_) => {
                parsed.error_count += 1;
                index += 1;
                continue;
            }
        };
        let timestamp = timestamp(&value).unwrap_or_else(|| Utc::now().timestamp());
        parsed.updated_at = parsed.updated_at.max(timestamp);
        let kind = value.get("type").and_then(Value::as_str).unwrap_or("");
        let payload = value.get("payload").unwrap_or(&Value::Null);
        match kind {
            "session_meta" => {
                parsed.session_id = first_string(payload, &["id", "session_id"])
                    .or_else(|| first_string(&value, &["id", "session_id"]))
                    .unwrap_or_default();
                parsed.cwd = first_string(payload, &["cwd"]);
            }
            "event_msg" => {
                let event_kind = payload.get("type").and_then(Value::as_str).unwrap_or("");
                if matches!(
                    event_kind,
                    "user_message" | "agent_message" | "error" | "task_complete"
                ) {
                    if let Some(text) =
                        first_string(payload, &["message", "text", "summary"]).and_then(clean_text)
                    {
                        let role = if event_kind == "user_message" {
                            "user"
                        } else {
                            "assistant"
                        };
                        let activity_kind = if event_kind == "error" {
                            "error"
                        } else {
                            event_kind
                        };
                        push_activity(
                            &mut parsed,
                            role,
                            activity_kind,
                            text,
                            timestamp,
                            index,
                            &value,
                        );
                    }
                }
            }
            "response_item" if payload.get("type").and_then(Value::as_str) == Some("message") => {
                let role = payload.get("role").and_then(Value::as_str).unwrap_or("");
                if matches!(role, "user" | "assistant") {
                    if let Some(text) = extract_content(payload.get("content")).and_then(clean_text)
                    {
                        let activity_kind = if role == "assistant" {
                            "assistant_final"
                        } else {
                            "user_message"
                        };
                        push_activity(
                            &mut parsed,
                            role,
                            activity_kind,
                            text,
                            timestamp,
                            index,
                            &value,
                        );
                    }
                }
            }
            _ => {}
        }
        index += 1;
    }
    if parsed.session_id.is_empty() {
        parsed.session_id = path.display().to_string();
    }
    if parsed.title.is_empty() {
        parsed.title = parsed
            .activities
            .iter()
            .find(|activity| activity.role == "user")
            .map(|activity| truncate(&activity.text, 90))
            .unwrap_or_else(|| "Codex 会话".to_string());
    }
    Ok(parsed)
}

fn parse_claude<R: BufRead>(
    provider: &str,
    path: &Path,
    reader: R,
) -> Result<ParsedSession, String> {
    let mut parsed = ParsedSession {
        provider: provider.to_string(),
        source_path: source_label(provider, path),
        ..Default::default()
    };
    let mut index = 0usize;
    for line in reader.lines() {
        let line = line.map_err(|err| err.to_string())?;
        let value: Value = match serde_json::from_str(&line) {
            Ok(value) => value,
            Err(_) => {
                parsed.error_count += 1;
                index += 1;
                continue;
            }
        };
        if value.get("isMeta").and_then(Value::as_bool) == Some(true) {
            index += 1;
            continue;
        }
        let timestamp = timestamp(&value).unwrap_or_else(|| Utc::now().timestamp());
        parsed.updated_at = parsed.updated_at.max(timestamp);
        parsed.session_id = parsed.session_id_or(&value, &["sessionId", "session_id"]);
        parsed.cwd = parsed
            .cwd
            .clone()
            .or_else(|| first_string(&value, &["cwd"]));
        if value.get("type").and_then(Value::as_str) == Some("custom-title") {
            parsed.title = first_string(&value, &["customTitle", "summary"]).unwrap_or_default();
        }
        if value.get("type").and_then(Value::as_str) == Some("summary") {
            parsed.title = first_string(&value, &["summary"]).unwrap_or(parsed.title);
        }
        let message = value.get("message").unwrap_or(&Value::Null);
        let role = message
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or_else(|| value.get("type").and_then(Value::as_str).unwrap_or(""));
        if matches!(role, "user" | "assistant") {
            if let Some(text) = extract_content(message.get("content")).and_then(clean_text) {
                let activity_kind = if role == "assistant" {
                    "assistant_final"
                } else {
                    "user_message"
                };
                push_activity(
                    &mut parsed,
                    role,
                    activity_kind,
                    text,
                    timestamp,
                    index,
                    &value,
                );
            }
        }
        index += 1;
    }
    if parsed.session_id.is_empty() {
        parsed.session_id = path.display().to_string();
    }
    if parsed.title.is_empty() {
        parsed.title = parsed
            .activities
            .iter()
            .find(|activity| activity.role == "user")
            .map(|activity| truncate(&activity.text, 90))
            .unwrap_or_else(|| "Claude Code 会话".to_string());
    }
    Ok(parsed)
}

impl ParsedSession {
    fn session_id_or(&mut self, value: &Value, keys: &[&str]) -> String {
        if self.session_id.is_empty() {
            first_string(value, keys).unwrap_or_default()
        } else {
            self.session_id.clone()
        }
    }
}

fn push_activity(
    parsed: &mut ParsedSession,
    role: &str,
    kind: &str,
    text: String,
    occurred_at: i64,
    index: usize,
    raw: &Value,
) {
    let text = redact(&text);
    if text.is_empty() || is_internal_text(&text) {
        return;
    }
    let id = stable_id(
        &parsed.provider,
        &format!("{}:{}:{}:{}", parsed.session_id, index, kind, text),
    );
    parsed.activities.push(Activity {
        id,
        provider: parsed.provider.clone(),
        session_id: stable_id(&parsed.provider, &parsed.session_id),
        source_path: parsed.source_path.clone(),
        kind: kind.to_string(),
        role: role.to_string(),
        text,
        occurred_at,
        metadata: serde_json::json!({
            "source": "local_transcript",
            "line": index,
            "raw_type": raw.get("type").and_then(Value::as_str).unwrap_or("")
        }),
    });
}

fn first_string(value: &Value, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| {
        value
            .get(*key)
            .and_then(Value::as_str)
            .map(ToString::to_string)
    })
}

fn timestamp(value: &Value) -> Option<i64> {
    let raw = value.get("timestamp").or_else(|| value.get("created_at"))?;
    if let Some(text) = raw.as_str() {
        return DateTime::parse_from_rfc3339(text)
            .ok()
            .map(|date| date.timestamp());
    }
    let number = raw.as_i64()?;
    Some(if number > 1_000_000_000_000 {
        number / 1000
    } else {
        number
    })
}

fn extract_content(value: Option<&Value>) -> Option<String> {
    let value = value?;
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Array(items) => {
            let mut parts = Vec::new();
            for item in items {
                if let Some(text) = item.as_str() {
                    parts.push(text.to_string());
                } else if let Some(text) = item.get("text").and_then(Value::as_str) {
                    parts.push(text.to_string());
                } else if let Some(text) = item.get("input_text").and_then(Value::as_str) {
                    parts.push(text.to_string());
                } else if let Some(text) = item.get("output_text").and_then(Value::as_str) {
                    parts.push(text.to_string());
                }
            }
            if parts.is_empty() {
                None
            } else {
                Some(parts.join("\n"))
            }
        }
        _ => None,
    }
}

fn clean_text(text: String) -> Option<String> {
    static TAGS: OnceLock<Regex> = OnceLock::new();
    let tags = TAGS.get_or_init(|| {
        Regex::new(concat!(
            r"(?s)<system-reminder>.*?</system-reminder>",
            r"|<local-command-caveat>.*?</local-command-caveat>",
            r"|<local-command-stdout>.*?</local-command-stdout>",
            r"|<environment_context>.*?</environment_context>",
            r"|<codex_internal_context>.*?</codex_internal_context>"
        ))
        .expect("internal wrapper regex must compile")
    });
    let cleaned = tags.replace_all(&text, "");
    let cleaned = cleaned.trim();
    if cleaned.is_empty() {
        None
    } else {
        Some(cleaned.to_string())
    }
}

pub(crate) fn redact(text: &str) -> String {
    let mut value = text.to_string();
    static PATTERNS: OnceLock<Vec<(Regex, &'static str)>> = OnceLock::new();
    let patterns = PATTERNS.get_or_init(|| {
        [
            (
                r"(?i)(api[_-]?key\s*[:=]\s*)[^\s,;]+",
                "$1[REDACTED_API_KEY]",
            ),
            (r"(?i)(bearer\s+)[a-z0-9._-]{12,}", "$1[REDACTED_TOKEN]"),
            (r"(?i)(sk-[a-z0-9_-]{12,})", "[REDACTED_API_KEY]"),
            (r"(?i)(password\s*[:=]\s*)[^\s,;]+", "$1[REDACTED]"),
            (
                r"(?i)(https?://)[^/\s:@]+:[^/\s@]+@",
                "$1[REDACTED_CREDENTIALS]@",
            ),
            (
                r"(?i)\b[A-Z0-9._%+-]+@[A-Z0-9.-]+\.[A-Z]{2,}\b",
                "[REDACTED_EMAIL]",
            ),
            (
                r"\beyJ[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}\b",
                "[REDACTED_JWT]",
            ),
            (
                r"\b(?:gh[pousr]_[A-Za-z0-9]{20,}|github_pat_[A-Za-z0-9_]{20,})\b",
                "[REDACTED_GITHUB_TOKEN]",
            ),
            (r"\b(?:AKIA|ASIA)[A-Z0-9]{16}\b", "[REDACTED_AWS_KEY]"),
            (
                r"(?s)-----BEGIN [A-Z0-9 ]*PRIVATE KEY-----.*?-----END [A-Z0-9 ]*PRIVATE KEY-----",
                "[REDACTED_PRIVATE_KEY]",
            ),
            (r"(?m)([A-Z][A-Z0-9_]{2,}\s*=\s*)[^\s]+", "$1[REDACTED_ENV]"),
            (r"/Users/[^/\s]+/", "~/"),
        ]
        .into_iter()
        .map(|(pattern, replacement)| {
            (
                Regex::new(pattern).expect("redaction regex must compile"),
                replacement,
            )
        })
        .collect()
    });
    for (regex, replacement) in patterns {
        value = regex.replace_all(&value, *replacement).to_string();
    }
    truncate(&value, 8_000)
}

fn source_label(provider: &str, path: &Path) -> String {
    let file = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("session");
    let hash = stable_id("source", &path.display().to_string());
    format!(
        "{}:{}#{}",
        provider,
        file,
        &hash[hash.len().saturating_sub(8)..]
    )
}

fn sanitize_cwd(value: &str) -> String {
    let home = dirs::home_dir().map(|path| path.display().to_string());
    if let Some(home) = home {
        if value == home {
            return "~".to_string();
        }
        if let Some(relative) = value.strip_prefix(&(home.clone() + "/")) {
            return format!("~/{relative}");
        }
    }
    if let Ok(regex) = Regex::new(r"^/Users/[^/]+") {
        return regex.replace(value, "~").to_string();
    }
    value.to_string()
}

fn is_internal_text(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.starts_with("<environment_context>")
        || lower.starts_with("this session is being continued")
        || lower.contains("<system-reminder>")
}

fn truncate(text: &str, max: usize) -> String {
    text.chars().take(max).collect()
}

fn stable_id(provider: &str, value: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(provider.as_bytes());
    hasher.update([0]);
    hasher.update(value.as_bytes());
    format!("{}-{}", provider, &hex::encode(hasher.finalize())[..24])
}

#[cfg(test)]
mod tests {
    use super::{parse_claude, parse_codex, redact, scan_directory, scan_sources};
    use crate::models::{ScanSummary, SourceSummary};
    use crate::store::Store;
    use proptest::prelude::*;
    use std::fs;
    use std::io::Cursor;
    use std::path::Path;

    #[test]
    fn parses_codex_realistic_events_and_redacts_keys() {
        let api_key = format!("{}{}", "sk-", "abcdefghijklmnop");
        let input = concat!(
            r#"{"type":"session_meta","timestamp":"2026-07-22T01:00:00Z","payload":{"id":"s1","cwd":"/tmp/demo"}}"#,
            "\n",
            r#"{"type":"event_msg","timestamp":"2026-07-22T01:01:00Z","payload":{"type":"user_message","message":"Build /Users/alice/project with api_key="#,
            "PLACEHOLDER",
            r#" and SECRET_TOKEN=value"}}"#,
            "\n",
            r#"{"type":"event_msg","timestamp":"2026-07-22T01:02:00Z","payload":{"type":"agent_message","message":"Done"}}"#
        )
        .replace("PLACEHOLDER", &api_key);
        let parsed = parse_codex("codex", Path::new("/tmp/a.jsonl"), Cursor::new(input)).unwrap();
        assert_eq!(parsed.session_id, "s1");
        assert_eq!(parsed.activities.len(), 2);
        assert!(parsed.activities[0].text.contains("[REDACTED_API_KEY]"));
        assert!(parsed.activities[0].text.contains("~/project"));
        assert!(parsed.activities[0]
            .text
            .contains("SECRET_TOKEN=[REDACTED_ENV]"));
        assert!(!parsed.activities[0].text.contains("/Users/alice"));
    }

    #[test]
    fn skips_claude_meta_messages() {
        let input = concat!(
            r#"{"type":"user","sessionId":"s2","isMeta":true,"message":{"role":"user","content":"secret file"}}"#,
            "\n",
            r#"{"type":"user","sessionId":"s2","message":{"role":"user","content":"hello"}}"#
        );
        let parsed =
            parse_claude("claude-code", Path::new("/tmp/a.jsonl"), Cursor::new(input)).unwrap();
        assert_eq!(parsed.activities.len(), 1);
        assert_eq!(parsed.activities[0].text, "hello");
    }

    #[test]
    fn redacts_identity_cloud_and_private_key_secrets() {
        let github_token = format!("{}{}", "ghp_", "abcdefghijklmnopqrstuvwxyz123456");
        let aws_key = format!("{}{}", "AKIA", "ABCDEFGHIJKLMNOP");
        let private_key = format!(
            "{}{}{}",
            "-----BEGIN ", "PRIVATE KEY-----\nsecret-material\n-----END ", "PRIVATE KEY-----"
        );
        let text = format!(
            "mail alice@example.com jwt {}.{}.abcdefghijklmnop github {} aws {} url https://alice:password@example.com/private {}",
            "eyJhbGciOiJIUzI1NiJ9",
            "eyJzdWIiOiIxMjM0NTY3ODkwIn0",
            github_token,
            aws_key,
            private_key
        );
        let cleaned = redact(&text);
        assert!(cleaned.contains("[REDACTED_EMAIL]"));
        assert!(cleaned.contains("[REDACTED_JWT]"));
        assert!(cleaned.contains("[REDACTED_GITHUB_TOKEN]"));
        assert!(cleaned.contains("[REDACTED_AWS_KEY]"));
        assert!(cleaned.contains("[REDACTED_CREDENTIALS]"));
        assert!(cleaned.contains("[REDACTED_PRIVATE_KEY]"));
        assert!(!cleaned.contains("alice@example.com"));
        assert!(!cleaned.contains("secret-material"));
    }

    proptest! {
        #[test]
        fn generated_credentials_and_user_paths_never_survive_redaction(
            token in "[a-zA-Z0-9_-]{20,48}",
            user in "[a-z]{3,12}",
            local in "[a-z]{3,12}",
            domain in "[a-z]{3,10}"
        ) {
            let api_key = format!("sk-{token}");
            let bearer = format!("Bearer {token}");
            let email = format!("{local}@{domain}.com");
            let path = format!("/Users/{user}/private/project");
            let user_root = format!("/Users/{user}");
            let input = format!("api_key={api_key} auth={bearer} mail={email} path={path}");
            let cleaned = redact(&input);

            prop_assert!(!cleaned.contains(&api_key));
            prop_assert!(!cleaned.contains(&token));
            prop_assert!(!cleaned.contains(&email));
            prop_assert!(!cleaned.contains(&user_root));
            prop_assert!(cleaned.contains("[REDACTED_API_KEY]"));
            prop_assert!(cleaned.contains("[REDACTED_TOKEN]"));
            prop_assert!(cleaned.contains("[REDACTED_EMAIL]"));
        }
    }

    #[test]
    fn unchanged_files_are_skipped_and_appends_only_add_new_activity() {
        let dir = tempfile::tempdir().unwrap();
        let transcript_dir = dir.path().join("sessions");
        fs::create_dir_all(&transcript_dir).unwrap();
        let transcript = transcript_dir.join("rollout.jsonl");
        let first = concat!(
            r#"{"type":"session_meta","timestamp":"2026-07-22T01:00:00Z","payload":{"id":"s1","cwd":"/Users/alice/project"}}"#,
            "\n",
            r#"{"type":"event_msg","timestamp":"2026-07-22T01:01:00Z","payload":{"type":"user_message","message":"Build it"}}"#,
            "\n",
            r#"{"type":"event_msg","timestamp":"2026-07-22T01:02:00Z","payload":{"type":"agent_message","message":"Done"}}"#,
            "\n"
        );
        fs::write(&transcript, first).unwrap();
        let store = Store::open(dir.path().join("store.sqlite3")).unwrap();

        let run = |store: &Store| {
            let mut summary = ScanSummary {
                days: 30,
                sources: vec![],
                sessions: vec![],
                scanned_sessions: 0,
                scanned_activities: 0,
                new_activities: 0,
                skipped_files: 0,
                errors: vec![],
            };
            let mut source = SourceSummary {
                provider: "codex".into(),
                root: transcript_dir.display().to_string(),
                available: true,
                session_count: 0,
                activity_count: 0,
                error: None,
                last_scanned_at: None,
                error_count: 0,
                cursor_count: 0,
            };
            scan_directory(
                store,
                "codex",
                &transcript_dir,
                0,
                0,
                false,
                &mut summary,
                &mut source,
            );
            summary
        };

        assert_eq!(run(&store).new_activities, 2);
        assert_eq!(run(&store).new_activities, 0);
        fs::write(
            &transcript,
            format!("{first}{}\n", r#"{"type":"event_msg","timestamp":"2026-07-22T01:03:00Z","payload":{"type":"agent_message","message":"Verified"}}"#),
        ).unwrap();
        assert_eq!(run(&store).new_activities, 1);
        assert_eq!(store.activity_count().unwrap(), 3);
    }

    #[test]
    fn expanding_history_window_rescans_unchanged_files() {
        let dir = tempfile::tempdir().unwrap();
        let transcript_dir = dir.path().join("sessions");
        fs::create_dir_all(&transcript_dir).unwrap();
        let transcript = transcript_dir.join("rollout.jsonl");
        fs::write(
            &transcript,
            concat!(
                r#"{"type":"session_meta","timestamp":"2026-07-01T01:00:00Z","payload":{"id":"s1"}}"#,
                "\n",
                r#"{"type":"event_msg","timestamp":"2026-07-02T01:00:00Z","payload":{"type":"user_message","message":"Older evidence"}}"#,
                "\n",
                r#"{"type":"event_msg","timestamp":"2026-07-22T01:00:00Z","payload":{"type":"agent_message","message":"Recent evidence"}}"#,
                "\n"
            ),
        )
        .unwrap();
        let store = Store::open(dir.path().join("store.sqlite3")).unwrap();
        let run = |activity_cutoff: i64| {
            let mut summary = ScanSummary {
                days: 30,
                sources: vec![],
                sessions: vec![],
                scanned_sessions: 0,
                scanned_activities: 0,
                new_activities: 0,
                skipped_files: 0,
                errors: vec![],
            };
            let mut source = SourceSummary {
                provider: "codex".into(),
                root: transcript_dir.display().to_string(),
                available: true,
                session_count: 0,
                activity_count: 0,
                error: None,
                last_scanned_at: None,
                error_count: 0,
                cursor_count: 0,
            };
            scan_directory(
                &store,
                "codex",
                &transcript_dir,
                0,
                activity_cutoff,
                false,
                &mut summary,
                &mut source,
            );
            summary
        };
        let recent_cutoff = chrono::DateTime::parse_from_rfc3339("2026-07-20T00:00:00Z")
            .unwrap()
            .timestamp();
        let older_cutoff = chrono::DateTime::parse_from_rfc3339("2026-07-01T00:00:00Z")
            .unwrap()
            .timestamp();
        assert_eq!(run(recent_cutoff).new_activities, 1);
        assert_eq!(run(older_cutoff).new_activities, 1);
        assert_eq!(store.activity_count().unwrap(), 2);
    }

    #[test]
    #[ignore = "reads the developer's real local Codex and Claude Code histories"]
    fn scans_real_home_into_an_isolated_store() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("real-home.sqlite3")).unwrap();
        let started = std::time::Instant::now();
        let result = scan_sources(&store, 30).unwrap();
        let first_elapsed = started.elapsed();
        assert!(result.sources.iter().any(|source| source.available));
        assert!(result.scanned_sessions > 0);
        assert!(result.scanned_activities > 0);
        let second_started = std::time::Instant::now();
        let second = scan_sources(&store, 30).unwrap();
        let second_elapsed = second_started.elapsed();
        assert!(second.scanned_sessions <= result.scanned_sessions);
        eprintln!(
            "real scan: sessions={} activities={} new={} errors={} first={:?}; second sessions={} new={} elapsed={:?}",
            result.scanned_sessions,
            result.scanned_activities,
            result.new_activities,
            result.errors.len(),
            first_elapsed,
            second.scanned_sessions,
            second.new_activities,
            second_elapsed,
        );
    }
}

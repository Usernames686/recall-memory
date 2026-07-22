use crate::models::{EvolutionEntry, McpInstallResult, McpStatus};
use crate::paths;
use crate::store::Store;
use chrono::Utc;
use serde_json::{json, Value};
use std::fs;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use toml_edit::{value, DocumentMut, Item, Table};

#[derive(Debug, thiserror::Error)]
pub enum McpError {
    #[error("filesystem error: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid JSON config: {0}")]
    Json(#[from] serde_json::Error),
    #[error("invalid TOML config: {0}")]
    Toml(#[from] toml_edit::TomlError),
    #[error("store error: {0}")]
    Store(#[from] crate::store::StoreError),
    #[error("MCP protocol error: {0}")]
    Protocol(String),
}

pub fn run_stdio() -> Result<(), McpError> {
    let store_path = std::env::var_os("RECALL_STORE_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(paths::store_path);
    let store = Store::open_read_only(store_path)?;
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout().lock();
    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let request: Value = match serde_json::from_str(&line) {
            Ok(value) => value,
            Err(_) => continue,
        };
        if request.get("id").is_none() {
            continue;
        }
        let response = handle_request(&store, &request);
        serde_json::to_writer(&mut stdout, &response)?;
        stdout.write_all(b"\n")?;
        stdout.flush()?;
    }
    Ok(())
}

pub fn handle_request(store: &Store, request: &Value) -> Value {
    let id = request.get("id").cloned().unwrap_or(Value::Null);
    let method = request.get("method").and_then(Value::as_str).unwrap_or("");
    let result = match method {
        "initialize" => Ok(json!({
            "protocolVersion": "2024-11-05",
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "recall-evolution", "version": "0.1.0"}
        })),
        "tools/list" => Ok(json!({"tools": tool_definitions()})),
        "tools/call" => call_tool(store, request.get("params").unwrap_or(&Value::Null)),
        _ => Err(format!("unsupported method: {method}")),
    };
    if method == "tools/call" {
        let name = request
            .pointer("/params/name")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        let action = request
            .pointer("/params/arguments/action")
            .and_then(Value::as_str);
        let status = if result.is_ok() { "ok" } else { "error" };
        if store.is_read_only() {
            let _ = store.append_mcp_telemetry(name, action, status);
        } else {
            let _ = store.append_mcp_call(name, action, status);
        }
    }
    match result {
        Ok(value) => json!({"jsonrpc":"2.0","id":id,"result":value}),
        Err(message) => json!({"jsonrpc":"2.0","id":id,"error":{"code":-32602,"message":message}}),
    }
}

fn tool_definitions() -> Vec<Value> {
    vec![
        json!({
            "name": "evolution_context",
            "description": "Read the latest active local metacognition and learned skills. In guided mode, call action=meta at the start of every new task, use the returned context_text and skill index, then call action=skill only when a full procedure is relevant. In mcp mode, call the same tool explicitly when needed.",
            "inputSchema": {
                "type":"object",
                "properties": {
                    "action":{"type":"string","enum":["meta","list","search","skill"]},
                    "query":{"type":"string"},
                    "skill_id":{"type":"string"},
                    "limit":{"type":"integer","minimum":1,"maximum":50}
                },
                "required":["action"],
                "additionalProperties":false
            }
        }),
        json!({
            "name": "evolution_run_status",
            "description": "Read the latest Evolution Agent run state and its bounded redacted trace summaries. This tool is read-only.",
            "inputSchema": {
                "type":"object",
                "properties": {
                    "limit":{"type":"integer","minimum":1,"maximum":100}
                },
                "additionalProperties":false
            }
        }),
    ]
}

fn call_tool(store: &Store, params: &Value) -> Result<Value, String> {
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .ok_or("missing tool name")?;
    let args = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));
    let payload = match name {
        "evolution_context" => context_payload(store, &args)?,
        "evolution_run_status" => run_status_payload(store, &args)?,
        _ => return Err(format!("unknown tool: {name}")),
    };
    Ok(
        json!({"content":[{"type":"text","text":serde_json::to_string_pretty(&payload).unwrap_or_default()}]}),
    )
}

fn context_payload(store: &Store, args: &Value) -> Result<Value, String> {
    let action = args.get("action").and_then(Value::as_str).unwrap_or("meta");
    let limit = args
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(20)
        .min(50) as usize;
    let active = store
        .list_entries()
        .map_err(|err| err.to_string())?
        .into_iter()
        .filter(|entry| entry.status == "active")
        .collect::<Vec<_>>();
    let config = store.config().map_err(|err| err.to_string())?;
    match action {
        "meta" => {
            let meta = active
                .iter()
                .filter(|entry| entry.kind == "meta")
                .collect::<Vec<_>>();
            let skills = active
                .iter()
                .filter(|entry| entry.kind == "skill")
                .take(limit)
                .collect::<Vec<_>>();
            Ok(json!({
                "updated_at": active.iter().map(|entry| entry.updated_at).max(),
                "context_mode": config.context_mode,
                "meta_count": meta.len(),
                "skill_count": skills.len(),
                "context_text": build_context_text(&meta, &skills),
                "meta": meta.into_iter().map(public_entry).collect::<Vec<_>>(),
                "skill_index": skills.into_iter().map(public_summary).collect::<Vec<_>>()
            }))
        }
        "list" => Ok(
            json!({"skills": active.iter().filter(|entry| entry.kind == "skill").take(limit).map(public_summary).collect::<Vec<_>>() }),
        ),
        "search" => {
            let query = args
                .get("query")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_lowercase();
            let skills = active
                .iter()
                .filter(|entry| entry.kind == "skill")
                .filter(|entry| {
                    format!("{} {} {}", entry.title, entry.summary, entry.body)
                        .to_lowercase()
                        .contains(&query)
                })
                .take(limit)
                .map(public_summary)
                .collect::<Vec<_>>();
            Ok(json!({"query":query,"skills":skills}))
        }
        "skill" => {
            let id = args
                .get("skill_id")
                .and_then(Value::as_str)
                .ok_or("skill_id is required")?;
            let entry = active
                .iter()
                .find(|entry| entry.id == id && entry.kind == "skill")
                .ok_or("active skill not found")?;
            Ok(public_entry(entry))
        }
        _ => Err("invalid action".to_string()),
    }
}

fn run_status_payload(store: &Store, args: &Value) -> Result<Value, String> {
    let limit = args
        .get("limit")
        .and_then(Value::as_i64)
        .unwrap_or(20)
        .clamp(1, 100);
    let Some(run) = store
        .current_evolution_run()
        .map_err(|err| err.to_string())?
    else {
        return Ok(json!({"run":null,"trace":[]}));
    };
    let trace = store
        .list_trace_events(&run.run_id, limit)
        .map_err(|err| err.to_string())?
        .into_iter()
        .map(|event| {
            json!({
                "occurred_at": event.occurred_at,
                "phase": event.phase,
                "event_type": event.event_type,
                "tool_name": event.tool_name,
                "summary": event.summary,
                "result_status": event.result_status,
                "error_code": event.error_code
            })
        })
        .collect::<Vec<_>>();
    Ok(json!({"run":run,"trace":trace}))
}

fn public_summary(entry: &EvolutionEntry) -> Value {
    json!({"id":entry.id,"title":crate::scanner::redact(&entry.title),"summary":crate::scanner::redact(&entry.summary),"updated_at":entry.updated_at})
}

fn public_entry(entry: &EvolutionEntry) -> Value {
    json!({"id":entry.id,"kind":entry.kind,"title":crate::scanner::redact(&entry.title),"summary":crate::scanner::redact(&entry.summary),"body":crate::scanner::redact(&entry.body),"updated_at":entry.updated_at})
}

fn build_context_text(meta: &[&EvolutionEntry], skills: &[&EvolutionEntry]) -> String {
    if meta.is_empty() && skills.is_empty() {
        return String::new();
    }

    let mut sections = vec!["## Recall Memory Context".to_string()];
    if !meta.is_empty() {
        sections.push("### Active Meta".to_string());
        sections.extend(meta.iter().map(|entry| {
            format!(
                "#### {}\n{}",
                crate::scanner::redact(&entry.title),
                crate::scanner::redact(&entry.body)
            )
        }));
    }
    if !skills.is_empty() {
        sections.push("### Available Learned Skills".to_string());
        sections.extend(skills.iter().map(|entry| {
            format!(
                "- **{}** ({}): {}",
                crate::scanner::redact(&entry.title),
                entry.id,
                crate::scanner::redact(&entry.summary)
            )
        }));
        sections.push(
            "Use evolution_context(action=\"skill\", skill_id=\"...\") to load a full Skill body."
                .to_string(),
        );
    }
    sections.join("\n\n")
}

pub fn install(sidecar_path: &Path, store_path: &Path) -> Result<McpInstallResult, McpError> {
    let codex_path = paths::codex_config_path();
    let claude_path = paths::claude_config_path();
    let mut backups = Vec::new();
    if codex_path.exists() {
        let backup = paths::backup_path(&codex_path);
        fs::copy(&codex_path, &backup)?;
        restrict_file_permissions(&backup)?;
        backups.push(backup.display().to_string());
    }
    if claude_path.exists() {
        let backup = paths::backup_path(&claude_path);
        fs::copy(&claude_path, &backup)?;
        restrict_file_permissions(&backup)?;
        backups.push(backup.display().to_string());
    }
    install_codex(&codex_path, sidecar_path, store_path)?;
    install_claude(&claude_path, sidecar_path, store_path)?;
    Ok(McpInstallResult {
        codex_config: codex_path.display().to_string(),
        claude_config: claude_path.display().to_string(),
        backups,
        sidecar_path: sidecar_path.display().to_string(),
    })
}

pub fn uninstall() -> Result<Value, McpError> {
    let codex = restore_or_remove_codex(&paths::codex_config_path())?;
    let claude = restore_or_remove_claude(&paths::claude_config_path())?;
    Ok(json!({
        "codexRestored": codex,
        "claudeRestored": claude,
        "message": "Recall MCP 配置已卸载；其他配置保持不变"
    }))
}

fn latest_backup(path: &Path) -> Result<Option<PathBuf>, McpError> {
    let Some(parent) = path.parent() else {
        return Ok(None);
    };
    let prefix = format!(
        "{}.",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("config")
    );
    let mut backups = fs::read_dir(parent)?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|candidate| {
            candidate
                .file_name()
                .and_then(|name| name.to_str())
                .map(|name| name.starts_with(&prefix) && name.contains("recall-backup-"))
                .unwrap_or(false)
        })
        .collect::<Vec<_>>();
    backups.sort();
    Ok(backups.pop())
}

fn restore_or_remove_codex(path: &Path) -> Result<bool, McpError> {
    if let Some(backup) = latest_backup(path)? {
        fs::copy(backup, path)?;
        return Ok(true);
    }
    if !path.exists() {
        return Ok(false);
    }
    let source = fs::read_to_string(path)?;
    let mut doc = source.parse::<DocumentMut>()?;
    if let Some(table) = doc["mcp_servers"].as_table_mut() {
        table.remove("recall");
    }
    atomic_write(path, doc.to_string().as_bytes())?;
    Ok(false)
}

fn restore_or_remove_claude(path: &Path) -> Result<bool, McpError> {
    if let Some(backup) = latest_backup(path)? {
        fs::copy(backup, path)?;
        return Ok(true);
    }
    if !path.exists() {
        return Ok(false);
    }
    let mut root: Value = serde_json::from_slice(&fs::read(path)?)?;
    if let Some(map) = root.get_mut("mcpServers").and_then(Value::as_object_mut) {
        map.remove("recall");
    }
    atomic_write(path, serde_json::to_string_pretty(&root)?.as_bytes())?;
    Ok(false)
}

fn install_codex(path: &Path, sidecar: &Path, store: &Path) -> Result<(), McpError> {
    let source = fs::read_to_string(path).unwrap_or_default();
    let mut doc = if source.trim().is_empty() {
        DocumentMut::new()
    } else {
        source.parse::<DocumentMut>()?
    };
    if !doc.as_table().contains_key("mcp_servers") {
        doc["mcp_servers"] = Item::Table(Table::new());
    }
    if doc["mcp_servers"].as_table().is_none() {
        return Err(McpError::Protocol(
            "Codex mcp_servers must be a TOML table".to_string(),
        ));
    }
    if !doc["mcp_servers"]
        .as_table()
        .map(|table| table.contains_key("recall"))
        .unwrap_or(false)
    {
        doc["mcp_servers"]["recall"] = Item::Table(Table::new());
    }
    if doc["mcp_servers"]["recall"].as_table().is_none() {
        return Err(McpError::Protocol(
            "Codex mcp_servers.recall must be a TOML table".to_string(),
        ));
    }
    if !doc["mcp_servers"]["recall"]
        .as_table()
        .map(|table| table.contains_key("env"))
        .unwrap_or(false)
    {
        doc["mcp_servers"]["recall"]["env"] = Item::Table(Table::new());
    }
    if doc["mcp_servers"]["recall"]["env"].as_table().is_none() {
        return Err(McpError::Protocol(
            "Codex mcp_servers.recall.env must be a TOML table".to_string(),
        ));
    }
    doc["mcp_servers"]["recall"]["command"] = value(sidecar.display().to_string());
    doc["mcp_servers"]["recall"]["env"]["RECALL_STORE_PATH"] = value(store.display().to_string());
    atomic_write(path, doc.to_string().as_bytes())
}

fn install_claude(path: &Path, sidecar: &Path, store: &Path) -> Result<(), McpError> {
    let mut root: Value = if path.exists() {
        serde_json::from_slice(&fs::read(path)?)?
    } else {
        json!({})
    };
    if !root.is_object() {
        return Err(McpError::Protocol(
            "Claude config root must be an object".to_string(),
        ));
    }
    let object = root.as_object_mut().unwrap();
    let servers = object.entry("mcpServers").or_insert_with(|| json!({}));
    let map = servers
        .as_object_mut()
        .ok_or_else(|| McpError::Protocol("mcpServers must be an object".to_string()))?;
    map.insert(
        "recall".to_string(),
        json!({
            "command": sidecar.display().to_string(),
            "args": [],
            "env": {"RECALL_STORE_PATH": store.display().to_string()}
        }),
    );
    atomic_write(path, serde_json::to_string_pretty(&root)?.as_bytes())
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), McpError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temp = path.with_extension(format!("recall-tmp-{}", std::process::id()));
    fs::write(&temp, bytes)?;
    fs::rename(temp, path)?;
    Ok(())
}

fn restrict_file_permissions(path: &Path) -> Result<(), McpError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

pub fn sidecar_path() -> Result<PathBuf, McpError> {
    let exe = std::env::current_exe()?;
    let parent = exe
        .parent()
        .ok_or_else(|| McpError::Protocol("current executable has no parent".to_string()))?;
    let direct = parent.join("evolution-mcp");
    if direct.exists() {
        return Ok(direct);
    }
    let debug = parent.join("..").join("evolution-mcp");
    Ok(debug)
}

pub fn smoke_test(sidecar: &Path, store_path: &Path) -> Result<Value, McpError> {
    let mut child = Command::new(sidecar)
        .env("RECALL_STORE_PATH", store_path)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| McpError::Protocol("sidecar stdin unavailable".to_string()))?;
    stdin.write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/list\",\"params\":{}}\n")?;
    stdin.write_all(b"{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/call\",\"params\":{\"name\":\"evolution_context\",\"arguments\":{\"action\":\"meta\"}}}\n")?;
    drop(stdin);
    let output = child.wait_with_output()?;
    if !output.status.success() {
        return Err(McpError::Protocol(
            String::from_utf8_lossy(&output.stderr).to_string(),
        ));
    }
    let lines = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .collect::<Vec<_>>();
    let has_two_tools = lines
        .first()
        .and_then(|value| value.pointer("/result/tools"))
        .and_then(Value::as_array)
        .map(Vec::len)
        == Some(2);
    let context_text = lines
        .get(1)
        .and_then(|value| value.pointer("/result/content/0/text"))
        .and_then(Value::as_str);
    let context_payload = context_text
        .and_then(|text| serde_json::from_str::<Value>(text).ok())
        .unwrap_or(Value::Null);
    let context_readable = context_text.is_some();
    let context_text_readable = context_payload
        .get("context_text")
        .and_then(Value::as_str)
        .is_some();
    let context_mode = context_payload
        .get("context_mode")
        .and_then(Value::as_str)
        .unwrap_or("guided");
    if !has_two_tools || !context_readable {
        return Err(McpError::Protocol(
            "sidecar did not expose two tools and readable context".to_string(),
        ));
    }
    Ok(json!({
        "tools": 2,
        "contextReadable": true,
        "contextTextReadable": context_text_readable,
        "contextMode": context_mode,
        "message": format!(
            "MCP 连接正常：{} 模式，合并上下文{}读取",
            if context_mode == "guided" { "Guided" } else { "MCP" },
            if context_text_readable { "可" } else { "不可" }
        ),
        "responses": lines
    }))
}

pub fn status(store: &Store) -> McpStatus {
    let codex = fs::read_to_string(paths::codex_config_path())
        .map(|content| {
            content.contains("[mcp_servers.recall]")
                || content.contains("mcp_servers") && content.contains("recall")
        })
        .unwrap_or(false);
    let claude = fs::read(paths::claude_config_path())
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        .and_then(|value| value.pointer("/mcpServers/recall").cloned())
        .is_some();
    McpStatus {
        codex,
        claude,
        last_checked: Some(Utc::now().timestamp()),
        recent_calls: store.recent_mcp_calls(20).unwrap_or_default(),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        handle_request, install_claude, install_codex, restore_or_remove_claude,
        restore_or_remove_codex,
    };
    use crate::models::EvolutionEntry;
    use crate::store::Store;
    use serde_json::Value;
    use tempfile::tempdir;

    #[test]
    fn exposes_exactly_two_tools_and_only_active_context() {
        let dir = tempdir().unwrap();
        let store = Store::open(dir.path().join("db.sqlite")).unwrap();
        store
            .save_config("https://example.test", "model", "guided")
            .unwrap();
        store
            .insert_entry(&EvolutionEntry {
                id: "meta-1".into(),
                kind: "meta".into(),
                title: "Review discipline".into(),
                summary: "Keep checks evidence-backed".into(),
                body: "Prefer evidence-backed changes.".into(),
                status: "active".into(),
                risk: "low".into(),
                source_refs: vec![],
                updated_at: 2,
                origin_run_id: None,
                target_entry_id: None,
                version: 1,
            })
            .unwrap();
        store
            .insert_entry(&EvolutionEntry {
                id: "skill-1".into(),
                kind: "skill".into(),
                title: "Stable build".into(),
                summary: "Run checks".into(),
                body: "Run cargo test".into(),
                status: "active".into(),
                risk: "low".into(),
                source_refs: vec![],
                updated_at: 1,
                origin_run_id: None,
                target_entry_id: None,
                version: 1,
            })
            .unwrap();
        let list = handle_request(
            &store,
            &serde_json::json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}),
        );
        assert_eq!(
            list.pointer("/result/tools")
                .unwrap()
                .as_array()
                .unwrap()
                .len(),
            2
        );
        let context = handle_request(
            &store,
            &serde_json::json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"evolution_context","arguments":{"action":"meta"}}}),
        );
        let context_text = context
            .pointer("/result/content/0/text")
            .and_then(Value::as_str)
            .unwrap();
        assert!(context_text.contains("guided"));
        assert!(context_text.contains("Prefer evidence-backed changes."));
        assert!(context_text.contains("Stable build"));
        assert!(context_text.contains("Run checks"));
    }

    #[test]
    fn missing_context_mode_defaults_to_guided_and_pending_is_excluded() {
        let dir = tempdir().unwrap();
        let store = Store::open(dir.path().join("db.sqlite")).unwrap();
        assert_eq!(store.config().unwrap().context_mode, "guided");
        store
            .insert_entry(&EvolutionEntry {
                id: "pending-1".into(),
                kind: "skill".into(),
                title: "Pending skill".into(),
                summary: "Should stay out".into(),
                body: "Do not expose".into(),
                status: "pending".into(),
                risk: "review".into(),
                source_refs: vec![],
                updated_at: 1,
                origin_run_id: None,
                target_entry_id: None,
                version: 1,
            })
            .unwrap();
        let context = handle_request(
            &store,
            &serde_json::json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"evolution_context","arguments":{"action":"meta"}}}),
        );
        let text = context.to_string();
        assert!(text.contains("guided"));
        assert!(!text.contains("Pending skill"));
        assert!(!text.contains("Do not expose"));
        let payload: Value = serde_json::from_str(
            context
                .pointer("/result/content/0/text")
                .and_then(Value::as_str)
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            payload.get("context_text").and_then(Value::as_str),
            Some("")
        );
    }

    #[test]
    fn run_status_tool_is_read_only() {
        let dir = tempdir().unwrap();
        let store = Store::open(dir.path().join("db.sqlite")).unwrap();
        store.start_evolution_run("run-1", "manual").unwrap();
        store
            .append_trace_event(
                "run-1",
                "analyzing",
                "tool_call",
                Some("read_activity_batch"),
                "读取脱敏活动",
                Some(3),
                "ok",
                None,
            )
            .unwrap();
        let response = handle_request(
            &store,
            &serde_json::json!({
                "jsonrpc":"2.0","id":1,"method":"tools/call","params":{
                    "name":"evolution_run_status",
                    "arguments":{"limit":10}
                }
            }),
        );
        assert!(response.to_string().contains("read_activity_batch"));
        assert_eq!(store.pending_count().unwrap(), 0);
    }

    #[test]
    fn read_only_store_serves_context_without_mutating_the_active_store() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("db.sqlite");
        let store = Store::open(path.clone()).unwrap();
        store
            .insert_entry(&EvolutionEntry {
                id: "active-1".into(),
                kind: "meta".into(),
                title: "Read only context".into(),
                summary: "MCP must not write Active Store".into(),
                body: "Only expose approved content.".into(),
                status: "active".into(),
                risk: "low".into(),
                source_refs: vec![],
                updated_at: 1,
                origin_run_id: None,
                target_entry_id: None,
                version: 1,
            })
            .unwrap();
        let before_calls = store.recent_mcp_calls(20).unwrap().len();
        let read_only = Store::open_read_only(path).unwrap();
        let response = handle_request(
            &read_only,
            &serde_json::json!({
                "jsonrpc":"2.0","id":1,"method":"tools/call","params":{
                    "name":"evolution_context","arguments":{"action":"meta"}
                }
            }),
        );
        assert!(response.to_string().contains("Read only context"));
        assert!(read_only
            .append_mcp_call("evolution_context", Some("meta"), "ok")
            .is_err());
        let calls = store.recent_mcp_calls(20).unwrap();
        assert_eq!(calls.len(), before_calls + 1);
        assert_eq!(calls[0].tool_name, "evolution_context");
    }

    #[test]
    fn installers_preserve_unrelated_codex_and_claude_settings() {
        let dir = tempdir().unwrap();
        let codex = dir.path().join("config.toml");
        let claude = dir.path().join("claude.json");
        std::fs::write(&codex, "model = \"gpt-test\"\n[ui]\ntheme = \"dark\"\n").unwrap();
        std::fs::write(
            &claude,
            r#"{"theme":"dark","mcpServers":{"existing":{"command":"other"}}}"#,
        )
        .unwrap();
        let sidecar = dir.path().join("evolution-mcp");
        let store = dir.path().join("store.sqlite3");
        install_codex(&codex, &sidecar, &store).unwrap();
        install_claude(&claude, &sidecar, &store).unwrap();
        let first_codex = std::fs::read_to_string(&codex).unwrap();
        let first_claude = std::fs::read_to_string(&claude).unwrap();
        install_codex(&codex, &sidecar, &store).unwrap();
        install_claude(&claude, &sidecar, &store).unwrap();
        let codex_text = std::fs::read_to_string(&codex).unwrap();
        assert!(codex_text.contains("model = \"gpt-test\""));
        assert!(codex_text.contains("theme = \"dark\""));
        assert!(codex_text.contains("[mcp_servers.recall]"));
        assert_eq!(codex_text, first_codex);
        let codex_doc = codex_text.parse::<toml_edit::DocumentMut>().unwrap();
        assert_eq!(
            codex_doc["mcp_servers"]["recall"]["env"]["RECALL_STORE_PATH"].as_str(),
            Some(store.to_string_lossy().as_ref())
        );
        let claude_json: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&claude).unwrap()).unwrap();
        assert_eq!(
            claude_json.get("theme").and_then(|value| value.as_str()),
            Some("dark")
        );
        assert!(claude_json.pointer("/mcpServers/existing").is_some());
        assert!(claude_json.pointer("/mcpServers/recall").is_some());
        assert_eq!(std::fs::read_to_string(&claude).unwrap(), first_claude);
    }

    #[test]
    fn uninstall_restores_latest_backup_or_only_removes_recall() {
        let dir = tempdir().unwrap();
        let codex = dir.path().join("config.toml");
        let claude = dir.path().join("claude.json");
        std::fs::write(&codex, "model = \"before\"\n").unwrap();
        std::fs::write(&claude, r#"{"theme":"dark"}"#).unwrap();
        let codex_backup = dir.path().join("config.toml.recall-backup-20990101000000");
        let claude_backup = dir.path().join("claude.json.recall-backup-20990101000000");
        std::fs::copy(&codex, &codex_backup).unwrap();
        std::fs::copy(&claude, &claude_backup).unwrap();
        std::fs::write(
            &codex,
            "model = \"after\"\n[mcp_servers.recall]\ncommand=\"sidecar\"\n",
        )
        .unwrap();
        std::fs::write(
            &claude,
            r#"{"theme":"light","mcpServers":{"recall":{"command":"sidecar"}}}"#,
        )
        .unwrap();

        assert!(restore_or_remove_codex(&codex).unwrap());
        assert!(restore_or_remove_claude(&claude).unwrap());
        assert!(std::fs::read_to_string(&codex).unwrap().contains("before"));
        assert_eq!(
            serde_json::from_slice::<Value>(&std::fs::read(&claude).unwrap()).unwrap()["theme"],
            "dark"
        );

        std::fs::remove_file(&codex_backup).unwrap();
        std::fs::remove_file(&claude_backup).unwrap();
        std::fs::write(
            &codex,
            "model = \"keep\"\n[mcp_servers.recall]\ncommand=\"sidecar\"\n[ui]\ntheme=\"dark\"\n",
        )
        .unwrap();
        std::fs::write(&claude, r#"{"theme":"dark","mcpServers":{"recall":{"command":"sidecar"},"other":{"command":"keep"}}}"#).unwrap();
        assert!(!restore_or_remove_codex(&codex).unwrap());
        assert!(!restore_or_remove_claude(&claude).unwrap());
        assert!(std::fs::read_to_string(&codex)
            .unwrap()
            .contains("theme=\"dark\""));
        let claude_root: Value = serde_json::from_slice(&std::fs::read(&claude).unwrap()).unwrap();
        assert!(claude_root["mcpServers"]["other"].is_object());
        assert!(claude_root["mcpServers"]["recall"].is_null());
    }

    #[test]
    fn codex_installer_fails_closed_on_invalid_or_conflicting_config() {
        let dir = tempdir().unwrap();
        let codex = dir.path().join("config.toml");
        let sidecar = dir.path().join("evolution-mcp");
        let store = dir.path().join("store.sqlite3");

        let invalid = "model = [broken";
        std::fs::write(&codex, invalid).unwrap();
        assert!(install_codex(&codex, &sidecar, &store).is_err());
        assert_eq!(std::fs::read_to_string(&codex).unwrap(), invalid);

        let conflicting = "mcp_servers = \"do-not-replace\"\n";
        std::fs::write(&codex, conflicting).unwrap();
        assert!(install_codex(&codex, &sidecar, &store).is_err());
        assert_eq!(std::fs::read_to_string(&codex).unwrap(), conflicting);
    }
}

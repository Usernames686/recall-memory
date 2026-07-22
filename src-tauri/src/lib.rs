pub mod mcp;
mod models;
mod paths;
mod reflection;
mod scanner;
mod store;
mod watcher;

use models::DEFAULT_CONTEXT_MODE;
use models::{
    AgentTraceEvent, CacheCleanupPreview, DashboardSnapshot, EntryVersion, EntryVersionDiff,
    EvolutionRunDetail, EvolutionSettingsInput, EvolutionSettingsView, MaintenanceResult,
    McpInstallResult, ReflectionConfigInput, ReflectionConfigView, ReflectionRunResult,
    RunRollbackResult, ScanSummary, SourceSummary,
};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use store::Store;
use tauri::{
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent},
    AppHandle, Emitter, Manager, State, WindowEvent,
};
use tauri_plugin_autostart::ManagerExt as AutostartManagerExt;
use tauri_plugin_notification::NotificationExt;

struct AppState {
    store: Arc<Mutex<Store>>,
    watcher: Mutex<Option<notify::RecommendedWatcher>>,
    agent_running: Arc<AtomicBool>,
    cancel_requested: Arc<AtomicBool>,
    tray: Mutex<Option<TrayIcon>>,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct CommandError {
    code: String,
    message: String,
    retryable: bool,
}

impl CommandError {
    fn new(code: &str, message: impl Into<String>, retryable: bool) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            retryable,
        }
    }

    fn store(error: impl ToString) -> Self {
        Self::new("store_error", error.to_string(), false)
    }
}

#[tauri::command]
fn get_snapshot(state: State<'_, AppState>) -> Result<DashboardSnapshot, String> {
    let store = state
        .store
        .lock()
        .map_err(|_| "store lock poisoned".to_string())?;
    snapshot(&store).map_err(|err| err.to_string())
}

#[tauri::command]
fn set_consent(granted: bool, state: State<'_, AppState>) -> Result<DashboardSnapshot, String> {
    let store = state
        .store
        .lock()
        .map_err(|_| "store lock poisoned".to_string())?;
    store.set_consent(granted).map_err(|err| err.to_string())?;
    store
        .append_audit(
            "source_consent_changed",
            None,
            &serde_json::json!({"granted": granted}),
        )
        .map_err(|err| err.to_string())?;
    snapshot(&store).map_err(|err| err.to_string())
}

#[tauri::command]
fn scan_sessions(days: i64, state: State<'_, AppState>) -> Result<ScanSummary, String> {
    let store = state
        .store
        .lock()
        .map_err(|_| "store lock poisoned".to_string())?;
    if !store.consent_granted().map_err(|err| err.to_string())? {
        return Err("请先授权读取本机 Agent 会话".to_string());
    }
    let settings = store.evolution_settings().map_err(|err| err.to_string())?;
    let result = scanner::scan_sources_with_options(
        &store,
        days,
        settings.codex_enabled,
        settings.claude_enabled,
        None,
    )
    .map_err(|err| err.to_string())?;
    store
        .append_audit(
            "session_scan_completed",
            None,
            &serde_json::json!({
                "days": days,
                "sessions": result.scanned_sessions,
                "newActivities": result.new_activities,
                "errors": result.errors.len()
            }),
        )
        .map_err(|err| err.to_string())?;
    Ok(result)
}

#[tauri::command]
fn save_reflection_config(
    input: ReflectionConfigInput,
    state: State<'_, AppState>,
) -> Result<ReflectionConfigView, String> {
    if input.base_url.trim().is_empty() || input.model.trim().is_empty() {
        return Err("Base URL 和模型 ID 不能为空".to_string());
    }
    let context_mode = input
        .context_mode
        .as_deref()
        .unwrap_or(DEFAULT_CONTEXT_MODE);
    if !matches!(context_mode, "mcp" | "guided") {
        return Err("上下文模式必须是 mcp 或 guided".to_string());
    }
    if !matches!(input.provider.as_str(), "remote" | "ollama") {
        return Err("模型 Provider 必须是 remote 或 ollama".to_string());
    }
    if !(10..=300).contains(&input.timeout_seconds) {
        return Err("模型超时必须在 10 到 300 秒之间".to_string());
    }
    if let Some(api_key) = input
        .api_key
        .as_deref()
        .filter(|key| !key.trim().is_empty())
    {
        keyring::Entry::new("recall-evolution", "reflection-api")
            .map_err(|err| err.to_string())?
            .set_password(api_key.trim())
            .map_err(|err| err.to_string())?;
    }
    let store = state
        .store
        .lock()
        .map_err(|_| "store lock poisoned".to_string())?;
    store
        .save_config_with_provider(
            &input.provider,
            input.base_url.trim(),
            input.model.trim(),
            context_mode,
            input.timeout_seconds,
        )
        .map_err(|err| err.to_string())?;
    store
        .mark_model_health("unknown", None)
        .map_err(|err| err.to_string())?;
    store.config().map_err(|err| err.to_string())
}

#[tauri::command]
fn save_evolution_settings(
    app: AppHandle,
    mut input: EvolutionSettingsInput,
    state: State<'_, AppState>,
) -> Result<EvolutionSettingsView, String> {
    if input.run_mode == "listener" && input.listen_since.is_none() {
        input.listen_since = Some(chrono::Utc::now().timestamp());
    }
    let autostart = app.autolaunch();
    if input.launch_at_login {
        autostart.enable().map_err(|err| err.to_string())?;
    } else {
        autostart.disable().map_err(|err| err.to_string())?;
    }
    let store = state
        .store
        .lock()
        .map_err(|_| "store lock poisoned".to_string())?;
    store
        .save_evolution_settings(&input)
        .map_err(|err| err.to_string())
}

#[tauri::command]
async fn test_model_connection(
    input: ReflectionConfigInput,
    state: State<'_, AppState>,
) -> Result<serde_json::Value, CommandError> {
    let api_key = input
        .api_key
        .clone()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| {
            keyring::Entry::new("recall-evolution", "reflection-api")
                .ok()
                .and_then(|entry| entry.get_password().ok())
                .unwrap_or_default()
        });
    {
        let store = state
            .store
            .lock()
            .map_err(|_| CommandError::new("store_locked", "Store 暂时不可用", true))?;
        store
            .mark_model_health("checking", None)
            .map_err(CommandError::store)?;
    }
    let result =
        reflection::test_connection(input.base_url, input.model, api_key, input.timeout_seconds)
            .await;
    let store = state
        .store
        .lock()
        .map_err(|_| CommandError::new("store_locked", "Store 暂时不可用", true))?;
    match result {
        Ok(value) => {
            store
                .mark_model_health("ok", None)
                .map_err(CommandError::store)?;
            Ok(value)
        }
        Err(error) => {
            let code = reflection::error_code(&error);
            store
                .mark_model_health("error", Some(&error.to_string()))
                .map_err(CommandError::store)?;
            Err(CommandError::new(
                code,
                error.to_string(),
                code == "network_error" || code == "timeout",
            ))
        }
    }
}

#[tauri::command]
async fn reflect_now(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<ReflectionRunResult, String> {
    execute_evolution(&app, &state).await
}

#[tauri::command]
async fn run_evolution_now(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<ReflectionRunResult, String> {
    execute_evolution(&app, &state).await
}

#[tauri::command]
fn cancel_evolution(app: AppHandle, state: State<'_, AppState>) -> Result<(), CommandError> {
    if !state.agent_running.load(Ordering::Acquire) {
        return Err(CommandError::new(
            "not_running",
            "当前没有正在运行的 Evolution Agent",
            false,
        ));
    }
    state.cancel_requested.store(true, Ordering::Release);
    let store = state
        .store
        .lock()
        .map_err(|_| CommandError::new("store_locked", "Store 暂时不可用", true))?;
    if let Some(run) = store.current_evolution_run().map_err(CommandError::store)? {
        store
            .update_evolution_run(
                &run.run_id,
                "cancelling",
                run.scanned_activities,
                run.consumed_activities,
                run.generated,
                run.activated,
                run.pending,
                None,
            )
            .map_err(CommandError::store)?;
    }
    drop(store);
    emit_run_state(&app, &state.store);
    Ok(())
}

#[tauri::command]
async fn retry_evolution(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<ReflectionRunResult, CommandError> {
    execute_evolution(&app, &state)
        .await
        .map_err(|message| CommandError::new("evolution_failed", message, true))
}

#[tauri::command]
fn list_entry_versions(
    entry_id: String,
    state: State<'_, AppState>,
) -> Result<Vec<EntryVersion>, CommandError> {
    let store = state
        .store
        .lock()
        .map_err(|_| CommandError::new("store_locked", "Store 暂时不可用", true))?;
    store
        .list_entry_versions(&entry_id)
        .map_err(CommandError::store)
}

#[tauri::command]
fn get_evolution_run_detail(
    run_id: String,
    state: State<'_, AppState>,
) -> Result<EvolutionRunDetail, CommandError> {
    let store = state
        .store
        .lock()
        .map_err(|_| CommandError::new("store_locked", "Store 暂时不可用", true))?;
    store
        .evolution_run_detail(&run_id)
        .map_err(CommandError::store)
}

#[tauri::command]
fn get_evolution_run_trace(
    run_id: String,
    state: State<'_, AppState>,
) -> Result<Vec<AgentTraceEvent>, CommandError> {
    let store = state
        .store
        .lock()
        .map_err(|_| CommandError::new("store_locked", "Store 暂时不可用", true))?;
    store
        .list_trace_events(&run_id, 500)
        .map_err(CommandError::store)
}

#[tauri::command]
fn get_entry_version_diff(
    entry_id: String,
    from_version: Option<i64>,
    to_version: i64,
    state: State<'_, AppState>,
) -> Result<EntryVersionDiff, CommandError> {
    let store = state
        .store
        .lock()
        .map_err(|_| CommandError::new("store_locked", "Store 暂时不可用", true))?;
    store
        .entry_version_diff(&entry_id, from_version, to_version)
        .map_err(CommandError::store)
}

#[tauri::command]
fn rollback_entry(
    entry_id: String,
    version: i64,
    state: State<'_, AppState>,
) -> Result<(), CommandError> {
    let store = state
        .store
        .lock()
        .map_err(|_| CommandError::new("store_locked", "Store 暂时不可用", true))?;
    store
        .rollback_entry(&entry_id, version)
        .map_err(CommandError::store)
}

#[tauri::command]
fn rollback_evolution_run(
    run_id: String,
    state: State<'_, AppState>,
) -> Result<RunRollbackResult, CommandError> {
    let store = state
        .store
        .lock()
        .map_err(|_| CommandError::new("store_locked", "Store 暂时不可用", true))?;
    store
        .rollback_evolution_run(&run_id)
        .map_err(CommandError::store)
}

#[tauri::command]
fn backup_store(state: State<'_, AppState>) -> Result<MaintenanceResult, CommandError> {
    let store = state
        .store
        .lock()
        .map_err(|_| CommandError::new("store_locked", "Store 暂时不可用", true))?;
    store.backup_store().map_err(CommandError::store)
}

#[tauri::command]
fn restore_store_backup(
    file_name: String,
    state: State<'_, AppState>,
) -> Result<MaintenanceResult, CommandError> {
    let store = state
        .store
        .lock()
        .map_err(|_| CommandError::new("store_locked", "Store 暂时不可用", true))?;
    store
        .restore_store_backup(&file_name)
        .map_err(CommandError::store)
}

#[tauri::command]
fn export_redacted_store(state: State<'_, AppState>) -> Result<MaintenanceResult, CommandError> {
    let store = state
        .store
        .lock()
        .map_err(|_| CommandError::new("store_locked", "Store 暂时不可用", true))?;
    store.export_redacted_store().map_err(CommandError::store)
}

#[tauri::command]
fn clear_reflected_activity_cache(
    state: State<'_, AppState>,
) -> Result<MaintenanceResult, CommandError> {
    let store = state
        .store
        .lock()
        .map_err(|_| CommandError::new("store_locked", "Store 暂时不可用", true))?;
    store
        .clear_reflected_activity_cache()
        .map_err(CommandError::store)
}

#[tauri::command]
fn preview_reflected_activity_cache_cleanup(
    state: State<'_, AppState>,
) -> Result<CacheCleanupPreview, CommandError> {
    let store = state
        .store
        .lock()
        .map_err(|_| CommandError::new("store_locked", "Store 暂时不可用", true))?;
    store.cache_cleanup_preview().map_err(CommandError::store)
}

async fn execute_evolution(
    app: &AppHandle,
    state: &AppState,
) -> Result<ReflectionRunResult, String> {
    execute_evolution_refs(
        app,
        &state.store,
        &state.agent_running,
        &state.cancel_requested,
    )
    .await
}

async fn execute_evolution_refs(
    app: &AppHandle,
    store_ref: &Arc<Mutex<Store>>,
    running: &Arc<AtomicBool>,
    cancel_requested: &Arc<AtomicBool>,
) -> Result<ReflectionRunResult, String> {
    if running.swap(true, Ordering::AcqRel) {
        return Err("Evolution Agent 正在运行，请等待当前运行完成".into());
    }
    cancel_requested.store(false, Ordering::Release);
    let result = execute_evolution_inner(app, store_ref, cancel_requested).await;
    running.store(false, Ordering::Release);
    cancel_requested.store(false, Ordering::Release);
    result
}

async fn execute_evolution_inner(
    app: &AppHandle,
    store_ref: &Arc<Mutex<Store>>,
    cancel_requested: &Arc<AtomicBool>,
) -> Result<ReflectionRunResult, String> {
    let run_id = format!("run-{}", uuid::Uuid::new_v4());
    let (settings, config) = {
        let store = store_ref
            .lock()
            .map_err(|_| "store lock poisoned".to_string())?;
        if !store.consent_granted().map_err(|err| err.to_string())? {
            return Err("请先授权读取本机 Agent 会话".into());
        }
        let settings = store.evolution_settings().map_err(|err| err.to_string())?;
        if !settings.enabled {
            return Err("Evolution Agent 未启用".into());
        }
        let config = store.config().map_err(|err| err.to_string())?;
        let providers = [
            settings.codex_enabled.then_some("codex".to_string()),
            settings.claude_enabled.then_some("claude-code".to_string()),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
        store
            .start_evolution_run_with_agent_context(
                &run_id,
                &settings.run_mode,
                &settings.agent_mode,
                (!config.model.trim().is_empty()).then_some(config.model.as_str()),
                &providers,
                settings.lookback_days,
            )
            .map_err(|err| err.to_string())?;
        (settings, config)
    };
    emit_trace_update(
        app,
        store_ref,
        &run_id,
        reflection::TraceEvent {
            phase: "scanning".into(),
            event_type: "phase_started".into(),
            tool_name: None,
            summary: "开始扫描 Codex 与 Claude Code 会话".into(),
            duration_ms: None,
            result_status: "running".into(),
            error_code: None,
        },
    );
    emit_run_state(app, store_ref);
    check_cancelled(app, store_ref, cancel_requested, &run_id)?;

    let prepared = (|| -> Result<_, String> {
        let store = store_ref
            .lock()
            .map_err(|_| "store lock poisoned".to_string())?;
        let since = if settings.run_mode == "listener" {
            settings.listen_since
        } else {
            Some(chrono::Utc::now().timestamp() - settings.lookback_days * 86_400)
        };
        let scan = scanner::scan_sources_with_options(
            &store,
            settings.lookback_days,
            settings.codex_enabled,
            settings.claude_enabled,
            settings.listen_since,
        )
        .map_err(|err| err.to_string());
        let scan = match scan {
            Ok(scan) => scan,
            Err(error) => return Err(error),
        };
        store
            .update_evolution_run(
                &run_id,
                "reading",
                scan.scanned_activities,
                0,
                0,
                0,
                0,
                None,
            )
            .map_err(|err| err.to_string())?;
        let activities = store
            .activities_for_reflection_since(80, since)
            .map_err(|err| err.to_string())?;
        store
            .set_evolution_run_activities(&run_id, &activities)
            .map_err(|err| err.to_string())?;
        let active_entries = store.list_entries().map_err(|err| err.to_string())?;
        Ok((activities, active_entries, scan.scanned_activities))
    })();
    let (activities, active_entries, scanned_activities) = match prepared {
        Ok(value) => value,
        Err(message) => {
            emit_trace_update(
                app,
                store_ref,
                &run_id,
                reflection::TraceEvent {
                    phase: "scanning".into(),
                    event_type: "run_failed".into(),
                    tool_name: None,
                    summary: message.clone(),
                    duration_ms: None,
                    result_status: "error".into(),
                    error_code: Some("scan_error".into()),
                },
            );
            if let Ok(store) = store_ref.lock() {
                let _ =
                    store.update_evolution_run(&run_id, "failed", 0, 0, 0, 0, 0, Some(&message));
            }
            emit_run_state(app, store_ref);
            notify_run(app, store_ref, "Evolution Agent 扫描失败", &message);
            return Err(message);
        }
    };
    emit_trace_update(
        app,
        store_ref,
        &run_id,
        reflection::TraceEvent {
            phase: "reading".into(),
            event_type: "activity_batch_ready".into(),
            tool_name: None,
            summary: format!("固定读取 {} 条脱敏活动", activities.len()),
            duration_ms: None,
            result_status: "ok".into(),
            error_code: None,
        },
    );
    emit_run_state(app, store_ref);
    check_cancelled(app, store_ref, cancel_requested, &run_id)?;

    let ids = activities.iter().map(|a| a.id.clone()).collect::<Vec<_>>();
    if activities.is_empty() {
        let store = store_ref
            .lock()
            .map_err(|_| "store lock poisoned".to_string())?;
        store
            .update_evolution_run(&run_id, "completed", scanned_activities, 0, 0, 0, 0, None)
            .map_err(|err| err.to_string())?;
        drop(store);
        emit_trace_update(
            app,
            store_ref,
            &run_id,
            reflection::TraceEvent {
                phase: "completed".into(),
                event_type: "run_completed".into(),
                tool_name: None,
                summary: "没有新的脱敏活动，运行未调用模型".into(),
                duration_ms: None,
                result_status: "ok".into(),
                error_code: None,
            },
        );
        emit_run_state(app, store_ref);
        return Ok(ReflectionRunResult {
            run_id,
            generated: vec![],
            activated: 0,
            pending: 0,
            discarded: 0,
            message: "没有新的脱敏活动".into(),
            verification_status: "not_run".into(),
            verification_summary: None,
        });
    }
    let api_key = keyring::Entry::new("recall-evolution", "reflection-api")
        .map_err(|err| err.to_string())?
        .get_password()
        .unwrap_or_default();
    {
        let store = store_ref
            .lock()
            .map_err(|_| "store lock poisoned".to_string())?;
        store
            .update_evolution_run(&run_id, "analyzing", scanned_activities, 0, 0, 0, 0, None)
            .map_err(|e| e.to_string())?;
    }
    emit_run_state(app, store_ref);
    let trace_sink: reflection::TraceSink = {
        let trace_app = app.clone();
        let trace_store = store_ref.clone();
        let trace_run_id = run_id.clone();
        Arc::new(move |event| emit_trace_update(&trace_app, &trace_store, &trace_run_id, event))
    };
    let generation = reflection::generate_agent(
        config.base_url.clone(),
        config.model.clone(),
        api_key,
        activities,
        active_entries,
        settings.max_agent_steps,
        &settings.agent_mode,
        config.timeout_seconds,
        &run_id,
        trace_sink,
    );
    let mut result = match tokio::select! {
        value = generation => Some(value),
        _ = wait_for_cancel(cancel_requested.clone()) => None,
    } {
        None => return cancel_run(app, store_ref, &run_id),
        Some(Ok(value)) => value,
        Some(Err(err)) => {
            let code = reflection::error_code(&err);
            emit_trace_update(
                app,
                store_ref,
                &run_id,
                reflection::TraceEvent {
                    phase: "analyzing".into(),
                    event_type: "run_failed".into(),
                    tool_name: None,
                    summary: err.to_string(),
                    duration_ms: None,
                    result_status: "error".into(),
                    error_code: Some(code.into()),
                },
            );
            let store = store_ref
                .lock()
                .map_err(|_| "store lock poisoned".to_string())?;
            let _ = store.mark_model_health("error", Some(&err.to_string()));
            store
                .update_evolution_run(
                    &run_id,
                    "failed",
                    scanned_activities,
                    0,
                    0,
                    0,
                    0,
                    Some(&err.to_string()),
                )
                .map_err(|e| e.to_string())?;
            drop(store);
            emit_run_state(app, store_ref);
            notify_run(app, store_ref, "Evolution Agent 运行失败", &err.to_string());
            return Err(err.to_string());
        }
    };
    {
        let store = store_ref
            .lock()
            .map_err(|_| "store lock poisoned".to_string())?;
        let _ = store.mark_model_health("ok", None);
        store
            .set_run_verification(
                &run_id,
                &result.verification_status,
                result.verification_summary.as_deref(),
            )
            .map_err(|error| error.to_string())?;
    }
    if let Some(summary) = result.verification_summary.as_deref() {
        emit_trace_update(
            app,
            store_ref,
            &run_id,
            reflection::TraceEvent {
                phase: "validating".into(),
                event_type: "verification_result".into(),
                tool_name: None,
                summary: summary.to_string(),
                duration_ms: None,
                result_status: result.verification_status.clone(),
                error_code: None,
            },
        );
    }
    {
        let store = store_ref
            .lock()
            .map_err(|_| "store lock poisoned".to_string())?;
        store
            .update_evolution_run(
                &run_id,
                "validating",
                scanned_activities,
                0,
                result.generated.len() as i64,
                0,
                0,
                None,
            )
            .map_err(|e| e.to_string())?;
    }
    emit_run_state(app, store_ref);
    check_cancelled(app, store_ref, cancel_requested, &run_id)?;
    let validation = {
        let store = store_ref
            .lock()
            .map_err(|_| "store lock poisoned".to_string())?;
        reflection::validate_risk_gate_with_policy(
            &store,
            &mut result,
            settings.auto_activate_low_risk,
        )
    };
    if let Err(err) = validation {
        let message = err.to_string();
        emit_trace_update(
            app,
            store_ref,
            &run_id,
            reflection::TraceEvent {
                phase: "validating".into(),
                event_type: "risk_gate_failed".into(),
                tool_name: None,
                summary: message.clone(),
                duration_ms: None,
                result_status: "error".into(),
                error_code: Some("validation_error".into()),
            },
        );
        let store = store_ref
            .lock()
            .map_err(|_| "store lock poisoned".to_string())?;
        store
            .update_evolution_run(
                &run_id,
                "failed",
                scanned_activities,
                0,
                result.generated.len() as i64,
                0,
                0,
                Some(&message),
            )
            .map_err(|e| e.to_string())?;
        drop(store);
        emit_run_state(app, store_ref);
        notify_run(app, store_ref, "Evolution Agent 校验失败", &message);
        return Err(message);
    }
    emit_trace_update(
        app,
        store_ref,
        &run_id,
        reflection::TraceEvent {
            phase: "validating".into(),
            event_type: "risk_gate_completed".into(),
            tool_name: None,
            summary: format!(
                "风险门完成：{} 条自动启用，{} 条进入审核",
                result.activated, result.pending
            ),
            duration_ms: None,
            result_status: "ok".into(),
            error_code: None,
        },
    );
    {
        let store = store_ref
            .lock()
            .map_err(|_| "store lock poisoned".to_string())?;
        store
            .update_evolution_run(
                &run_id,
                "persisting",
                scanned_activities,
                0,
                result.generated.len() as i64,
                result.activated,
                result.pending,
                None,
            )
            .map_err(|e| e.to_string())?;
    }
    emit_run_state(app, store_ref);
    check_cancelled(app, store_ref, cancel_requested, &run_id)?;
    let persistence = {
        let store = store_ref
            .lock()
            .map_err(|_| "store lock poisoned".to_string())?;
        store.persist_evolution_result(&run_id, &result, &ids, scanned_activities)
    };
    if let Err(err) = persistence {
        let message = err.to_string();
        emit_trace_update(
            app,
            store_ref,
            &run_id,
            reflection::TraceEvent {
                phase: "persisting".into(),
                event_type: "persistence_failed".into(),
                tool_name: None,
                summary: message.clone(),
                duration_ms: None,
                result_status: "error".into(),
                error_code: Some("store_error".into()),
            },
        );
        let store = store_ref
            .lock()
            .map_err(|_| "store lock poisoned".to_string())?;
        store
            .update_evolution_run(
                &run_id,
                "failed",
                scanned_activities,
                0,
                result.generated.len() as i64,
                result.activated,
                result.pending,
                Some(&message),
            )
            .map_err(|e| e.to_string())?;
        drop(store);
        emit_run_state(app, store_ref);
        notify_run(app, store_ref, "Evolution Agent 保存失败", &message);
        return Err(message);
    }
    emit_run_state(app, store_ref);
    emit_trace_update(
        app,
        store_ref,
        &run_id,
        reflection::TraceEvent {
            phase: "completed".into(),
            event_type: "run_completed".into(),
            tool_name: None,
            summary: format!("运行完成：生成 {} 条候选", result.generated.len()),
            duration_ms: None,
            result_status: "ok".into(),
            error_code: None,
        },
    );
    if result.pending > 0 {
        notify_run(
            app,
            store_ref,
            "Recall 有新的审核项",
            &format!("{} 条候选需要人工审核", result.pending),
        );
    } else {
        notify_run(
            app,
            store_ref,
            "Evolution Agent 已完成",
            &format!("生成 {} 条候选", result.generated.len()),
        );
    }
    Ok(result)
}

fn notify_run(app: &AppHandle, store_ref: &Arc<Mutex<Store>>, title: &str, body: &str) {
    let enabled = store_ref
        .lock()
        .ok()
        .and_then(|store| store.evolution_settings().ok())
        .map(|settings| settings.notifications_enabled)
        .unwrap_or(false);
    if enabled {
        let _ = app.notification().builder().title(title).body(body).show();
    }
}

async fn wait_for_cancel(cancel_requested: Arc<AtomicBool>) {
    while !cancel_requested.load(Ordering::Acquire) {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
}

fn check_cancelled(
    app: &AppHandle,
    store_ref: &Arc<Mutex<Store>>,
    cancel_requested: &Arc<AtomicBool>,
    run_id: &str,
) -> Result<(), String> {
    if cancel_requested.load(Ordering::Acquire) {
        cancel_run::<()>(app, store_ref, run_id)
    } else {
        Ok(())
    }
}

fn cancel_run<T>(
    app: &AppHandle,
    store_ref: &Arc<Mutex<Store>>,
    run_id: &str,
) -> Result<T, String> {
    emit_trace_update(
        app,
        store_ref,
        run_id,
        reflection::TraceEvent {
            phase: "cancelled".into(),
            event_type: "run_cancelled".into(),
            tool_name: None,
            summary: "运行已取消，活动未消费".into(),
            duration_ms: None,
            result_status: "cancelled".into(),
            error_code: Some("cancelled".into()),
        },
    );
    let store = store_ref
        .lock()
        .map_err(|_| "store lock poisoned".to_string())?;
    let current = store
        .current_evolution_run()
        .map_err(|err| err.to_string())?;
    if let Some(run) = current.filter(|run| run.run_id == run_id) {
        store
            .update_evolution_run(
                run_id,
                "cancelled",
                run.scanned_activities,
                run.consumed_activities,
                run.generated,
                run.activated,
                run.pending,
                Some("用户取消，活动未被消费，可安全重试"),
            )
            .map_err(|err| err.to_string())?;
        store
            .append_audit(
                "evolution_agent_cancelled",
                Some(run_id),
                &serde_json::json!({"consumed": run.consumed_activities}),
            )
            .map_err(|err| err.to_string())?;
    }
    drop(store);
    emit_run_state(app, store_ref);
    Err("Evolution Agent 已取消，活动未被消费".into())
}

fn emit_run_state(app: &AppHandle, store_ref: &Arc<Mutex<Store>>) {
    if let Ok(store) = store_ref.lock() {
        if let Ok(Some(run)) = store.current_evolution_run() {
            let _ = app.emit("evolution-state", run);
        }
    }
}

fn emit_trace_update(
    app: &AppHandle,
    store_ref: &Arc<Mutex<Store>>,
    run_id: &str,
    event: reflection::TraceEvent,
) {
    if let Ok(store) = store_ref.lock() {
        if let Ok(saved) = store.append_trace_event(
            run_id,
            &event.phase,
            &event.event_type,
            event.tool_name.as_deref(),
            &event.summary,
            event.duration_ms,
            &event.result_status,
            event.error_code.as_deref(),
        ) {
            let _ = app.emit("evolution-trace", saved);
        }
    }
}

fn start_scheduler(
    app: AppHandle,
    store_ref: Arc<Mutex<Store>>,
    running: Arc<AtomicBool>,
    cancel_requested: Arc<AtomicBool>,
) {
    tauri::async_runtime::spawn(async move {
        let mut listener_pending_count = 0i64;
        let mut listener_last_change = 0i64;
        let mut retry_after = 0i64;
        let mut failure_count = 0u32;
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(60)).await;
            if running.load(Ordering::Acquire) {
                continue;
            }
            let now = chrono::Utc::now().timestamp();
            if now < retry_after {
                continue;
            }
            let (due, listener_mode) = {
                let Ok(store) = store_ref.lock() else {
                    continue;
                };
                let Ok(settings) = store.evolution_settings() else {
                    continue;
                };
                if !settings.enabled
                    || !store.consent_granted().unwrap_or(false)
                    || settings.run_mode == "manual"
                {
                    listener_pending_count = 0;
                    (false, false)
                } else if settings.run_mode == "scheduled" {
                    let due = store
                        .last_evolution_completed_at()
                        .ok()
                        .flatten()
                        .map(|last| now - last >= settings.schedule_hours * 3_600)
                        .unwrap_or(true);
                    (due, false)
                } else {
                    let scanned = scanner::scan_sources_with_options(
                        &store,
                        settings.lookback_days,
                        settings.codex_enabled,
                        settings.claude_enabled,
                        settings.listen_since,
                    )
                    .is_ok();
                    let pending_count = if scanned {
                        store.dirty_count_since(settings.listen_since).unwrap_or(0)
                    } else {
                        0
                    };
                    let due = if pending_count == 0 {
                        listener_pending_count = 0;
                        false
                    } else if pending_count != listener_pending_count {
                        listener_pending_count = pending_count;
                        listener_last_change = now;
                        false
                    } else {
                        now - listener_last_change >= 120
                    };
                    (due, true)
                }
            };
            if due {
                let result =
                    execute_evolution_refs(&app, &store_ref, &running, &cancel_requested).await;
                if result.is_ok() {
                    failure_count = 0;
                    retry_after = 0;
                } else {
                    failure_count = failure_count.saturating_add(1);
                    let delay = (60i64 * 2i64.pow(failure_count.min(6))).min(3_600);
                    retry_after = now + delay;
                }
                if listener_mode {
                    if result.is_ok() {
                        listener_pending_count = 0;
                    } else {
                        listener_last_change = now;
                    }
                }
            }
        }
    });
}

#[tauri::command]
fn set_entry_status(id: String, status: String, state: State<'_, AppState>) -> Result<(), String> {
    if !matches!(status.as_str(), "active" | "rejected" | "disabled") {
        return Err("无效的条目状态".to_string());
    }
    let store = state
        .store
        .lock()
        .map_err(|_| "store lock poisoned".to_string())?;
    store
        .set_entry_status(&id, &status)
        .map_err(|err| err.to_string())
}

#[tauri::command]
fn review_entry(
    id: String,
    status: String,
    reason: String,
    state: State<'_, AppState>,
) -> Result<(), CommandError> {
    if !matches!(status.as_str(), "active" | "rejected" | "disabled") {
        return Err(CommandError::new("invalid_status", "无效的审核状态", false));
    }
    if reason.trim().is_empty() {
        return Err(CommandError::new(
            "reason_required",
            "审核原因不能为空",
            false,
        ));
    }
    let store = state
        .store
        .lock()
        .map_err(|_| CommandError::new("store_locked", "Store 暂时不可用", true))?;
    store
        .set_entry_status_with_reason(&id, &status, reason.trim())
        .map_err(CommandError::store)
}

#[tauri::command]
fn install_mcp(state: State<'_, AppState>) -> Result<McpInstallResult, String> {
    let store = state
        .store
        .lock()
        .map_err(|_| "store lock poisoned".to_string())?;
    let sidecar = mcp::sidecar_path().map_err(|err| err.to_string())?;
    if !sidecar.exists() {
        return Err(format!("MCP sidecar 尚未构建: {}", sidecar.display()));
    }
    let result = mcp::install(&sidecar, store.path()).map_err(|err| err.to_string())?;
    store
        .append_audit(
            "mcp_installed",
            None,
            &serde_json::json!({"backups": result.backups.len()}),
        )
        .map_err(|err| err.to_string())?;
    Ok(result)
}

#[tauri::command]
fn test_mcp(state: State<'_, AppState>) -> Result<serde_json::Value, String> {
    let store = state
        .store
        .lock()
        .map_err(|_| "store lock poisoned".to_string())?;
    let sidecar = mcp::sidecar_path().map_err(|err| err.to_string())?;
    mcp::smoke_test(&sidecar, store.path()).map_err(|err| err.to_string())
}

fn snapshot(store: &Store) -> Result<DashboardSnapshot, store::StoreError> {
    let sessions = store.list_sessions(200)?;
    let mut sources = vec![
        SourceSummary {
            provider: "codex".to_string(),
            root: paths::codex_home().display().to_string(),
            available: paths::codex_home().join("sessions").is_dir()
                || paths::codex_home().join("archived_sessions").is_dir(),
            session_count: 0,
            activity_count: 0,
            error: None,
            last_scanned_at: None,
            error_count: 0,
            cursor_count: 0,
        },
        SourceSummary {
            provider: "claude-code".to_string(),
            root: paths::claude_home().display().to_string(),
            available: paths::claude_home().join("projects").is_dir(),
            session_count: 0,
            activity_count: 0,
            error: None,
            last_scanned_at: None,
            error_count: 0,
            cursor_count: 0,
        },
    ];
    for source in &mut sources {
        let (sessions, activities) = store.provider_totals(&source.provider)?;
        source.session_count = sessions;
        source.activity_count = activities;
        let (last_scanned_at, error_count, cursor_count) =
            store.source_scan_health(&source.provider)?;
        source.last_scanned_at = last_scanned_at;
        source.error_count = error_count;
        source.cursor_count = cursor_count;
    }
    let run = store.current_evolution_run()?;
    let run_activities = match run.as_ref() {
        Some(run) => store.evolution_run_activities(&run.run_id)?,
        None => Vec::new(),
    };
    Ok(DashboardSnapshot {
        consent_granted: store.consent_granted()?,
        sources,
        sessions,
        activities: store.list_activities(40)?,
        run_activities,
        entries: store.list_entries()?,
        pending_count: store.pending_count()?,
        activity_count: store.activity_count()?,
        dirty_count: store.dirty_count()?,
        last_reflection_at: store.last_reflection_at()?,
        config: store.config()?,
        evolution: store.evolution_settings()?,
        run,
        run_history: store.list_evolution_runs(100)?,
        store_stats: store.store_stats()?,
        redaction_report: store.redaction_report()?,
        cache_cleanup_preview: store.cache_cleanup_preview()?,
        backups: store.list_backups()?,
        audit_events: store.list_audit_events(100)?,
        mcp: mcp::status(),
    })
}

fn show_main_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.set_focus();
    }
}

fn build_tray(app: &tauri::App) -> tauri::Result<TrayIcon> {
    let show = MenuItem::with_id(app, "show_recall", "显示 Recall", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit_recall", "退出 Recall", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&show, &quit])?;
    let mut builder = TrayIconBuilder::with_id("recall")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .tooltip("Recall Memory")
        .on_menu_event(|app, event| match event.id.as_ref() {
            "show_recall" => show_main_window(app),
            "quit_recall" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_main_window(tray.app_handle());
            }
        });
    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone()).icon_as_template(true);
    }
    builder.build(app)
}

pub fn run() {
    let store = Store::open(paths::store_path()).expect("failed to open Recall data store");
    let _ = store.recover_interrupted_runs();
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _, _| {
            show_main_window(app);
        }))
        .plugin(tauri_plugin_autostart::Builder::new().build())
        .plugin(tauri_plugin_notification::init())
        .manage(AppState {
            store: Arc::new(Mutex::new(store)),
            watcher: Mutex::new(None),
            agent_running: Arc::new(AtomicBool::new(false)),
            cancel_requested: Arc::new(AtomicBool::new(false)),
            tray: Mutex::new(None),
        })
        .setup(|app| {
            let source_watcher = watcher::start(app.handle().clone())?;
            let state = app.state::<AppState>();
            *state
                .watcher
                .lock()
                .map_err(|_| std::io::Error::other("watcher lock poisoned"))? =
                Some(source_watcher);
            start_scheduler(
                app.handle().clone(),
                state.store.clone(),
                state.agent_running.clone(),
                state.cancel_requested.clone(),
            );
            let tray = build_tray(app)?;
            *state
                .tray
                .lock()
                .map_err(|_| std::io::Error::other("tray lock poisoned"))? = Some(tray);
            Ok(())
        })
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .invoke_handler(tauri::generate_handler![
            get_snapshot,
            set_consent,
            scan_sessions,
            save_reflection_config,
            save_evolution_settings,
            test_model_connection,
            reflect_now,
            run_evolution_now,
            cancel_evolution,
            retry_evolution,
            set_entry_status,
            review_entry,
            list_entry_versions,
            get_evolution_run_detail,
            get_evolution_run_trace,
            get_entry_version_diff,
            rollback_entry,
            rollback_evolution_run,
            backup_store,
            restore_store_backup,
            export_redacted_store,
            preview_reflected_activity_cache_cleanup,
            clear_reflected_activity_cache,
            install_mcp,
            test_mcp
        ])
        .run(tauri::generate_context!())
        .expect("error while running Recall Memory");
}

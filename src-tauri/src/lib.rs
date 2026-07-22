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
    RunRollbackResult, RunnerConfigSnapshot, ScanSummary, SchedulerState, SourceSummary,
};
use sha2::{Digest, Sha256};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use std::{fs, path::PathBuf};
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
    activity_signal: Arc<tokio::sync::Notify>,
    tray: Mutex<Option<TrayIcon>>,
}

/// Scope credentials to the configured provider and endpoint. A single global
/// key would be unsafe when a user tests or switches to another gateway.
pub(crate) fn keyring_account(provider: &str, base_url: &str) -> String {
    let normalized = reqwest::Url::parse(base_url.trim())
        .map(|url| url.to_string().trim_end_matches('/').to_string())
        .unwrap_or_else(|_| base_url.trim().trim_end_matches('/').to_string());
    let digest = Sha256::digest(format!("{provider}\n{normalized}").as_bytes());
    format!("reflection-api-{}", &hex::encode(digest)[..24])
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

fn evolution_command_error(message: String) -> CommandError {
    let lower = message.to_ascii_lowercase();
    let (code, retryable) = if lower.contains("401") || lower.contains("403") {
        ("unauthorized", false)
    } else if lower.contains("404") || lower.contains("model_not_found") {
        ("model_not_found", false)
    } else if lower.contains("429") || lower.contains("rate_limited") {
        ("rate_limited", true)
    } else if lower.contains("timeout") || message.contains("超时") {
        ("timeout", true)
    } else if lower.contains("invalid_response")
        || message.contains("未调用")
        || message.contains("非法")
    {
        ("invalid_response", false)
    } else if message.contains("授权") {
        ("consent_required", false)
    } else if message.contains("正在运行") {
        ("already_running", true)
    } else if message.contains("取消") {
        ("cancelled", false)
    } else if message.contains("扫描") {
        ("scan_failed", true)
    } else {
        ("evolution_failed", true)
    };
    let safe_message = scanner::redact(&message)
        .chars()
        .take(500)
        .collect::<String>();
    CommandError::new(code, safe_message, retryable)
}

fn open_store_for_app(path: PathBuf) -> Store {
    match Store::open(path.clone()) {
        Ok(store) => store,
        Err(error) if is_recoverable_database_error(&error) && path.exists() => {
            let stamp = chrono::Utc::now().format("%Y%m%d%H%M%S%f");
            let quarantine = path.with_file_name(format!(
                "{}.corrupt-{}",
                path.file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("evolution.sqlite3"),
                stamp
            ));
            if let Err(rename_error) = quarantine_database_files(&path, &quarantine) {
                panic!("Recall 数据库损坏且无法隔离：{error}; 隔离失败：{rename_error}");
            }
            eprintln!(
                "Recall 数据库已隔离到 {}，正在创建新的 Store；原文件未删除",
                quarantine.display()
            );
            let recovered = Store::open(path)
                .unwrap_or_else(|new_error| panic!("Recall 无法创建恢复后的 Store：{new_error}"));
            let quarantined_name = quarantine
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("evolution.sqlite3.corrupt");
            let _ = recovered.set_app_state(
                "recovery_notice",
                &format!(
                    "检测到本地 Store 损坏，旧文件已隔离为 {quarantined_name}。Recall 已创建空 Store，请在数据管理中恢复最近备份。"
                ),
            );
            recovered
        }
        Err(error) => panic!("Recall 无法打开本地 Store：{error}"),
    }
}

fn is_recoverable_database_error(error: &store::StoreError) -> bool {
    let message = error.to_string().to_ascii_lowercase();
    message.contains("not a database")
        || message.contains("database disk image is malformed")
        || message.contains("malformed database")
}

fn quarantine_database_files(
    path: &std::path::Path,
    quarantine: &std::path::Path,
) -> std::io::Result<()> {
    fs::rename(path, quarantine)?;
    for suffix in ["-wal", "-shm"] {
        let sidecar = PathBuf::from(format!("{}{}", path.display(), suffix));
        if sidecar.exists() {
            let target = PathBuf::from(format!("{}{}", quarantine.display(), suffix));
            fs::rename(sidecar, target)?;
        }
    }
    Ok(())
}

#[tauri::command]
fn get_snapshot(state: State<'_, AppState>) -> Result<DashboardSnapshot, CommandError> {
    let store = state
        .store
        .lock()
        .map_err(|_| CommandError::new("store_locked", "Store 暂时不可用", true))?;
    snapshot(&store).map_err(CommandError::store)
}

#[tauri::command]
fn set_consent(
    granted: bool,
    state: State<'_, AppState>,
) -> Result<DashboardSnapshot, CommandError> {
    let store = state
        .store
        .lock()
        .map_err(|_| CommandError::new("store_locked", "Store 暂时不可用", true))?;
    store.set_consent(granted).map_err(CommandError::store)?;
    store
        .append_audit(
            "source_consent_changed",
            None,
            &serde_json::json!({"granted": granted}),
        )
        .map_err(CommandError::store)?;
    snapshot(&store).map_err(CommandError::store)
}

#[tauri::command]
fn scan_sessions(days: i64, state: State<'_, AppState>) -> Result<ScanSummary, CommandError> {
    if !matches!(days, 1 | 7 | 30) {
        return Err(CommandError::new(
            "invalid_lookback",
            "扫描范围必须是 1、7 或 30 天",
            false,
        ));
    }
    let store = state
        .store
        .lock()
        .map_err(|_| CommandError::new("store_locked", "Store 暂时不可用", true))?;
    if !store.consent_granted().map_err(CommandError::store)? {
        return Err(CommandError::new(
            "consent_required",
            "请先授权读取本机 Agent 会话",
            false,
        ));
    }
    let settings = store.evolution_settings().map_err(CommandError::store)?;
    let result = scanner::scan_sources_with_roots(
        &store,
        days,
        settings.codex_enabled,
        settings.claude_enabled,
        None,
        std::path::Path::new(&settings.codex_source_path),
        std::path::Path::new(&settings.claude_source_path),
    )
    .map_err(|err| CommandError::new("scan_failed", err.to_string(), true))?;
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
        .map_err(CommandError::store)?;
    Ok(result)
}

#[tauri::command]
fn save_reflection_config(
    input: ReflectionConfigInput,
    state: State<'_, AppState>,
) -> Result<ReflectionConfigView, CommandError> {
    if input.base_url.trim().is_empty() || input.model.trim().is_empty() {
        return Err(CommandError::new(
            "invalid_model_config",
            "Base URL 和模型 ID 不能为空",
            false,
        ));
    }
    let context_mode = input
        .context_mode
        .as_deref()
        .unwrap_or(DEFAULT_CONTEXT_MODE);
    if !matches!(context_mode, "mcp" | "guided") {
        return Err(CommandError::new(
            "invalid_context_mode",
            "上下文模式必须是 mcp 或 guided",
            false,
        ));
    }
    if !matches!(input.provider.as_str(), "remote" | "ollama") {
        return Err(CommandError::new(
            "invalid_provider",
            "模型 Provider 必须是 remote 或 ollama",
            false,
        ));
    }
    if !(10..=300).contains(&input.timeout_seconds) {
        return Err(CommandError::new(
            "invalid_timeout",
            "模型超时必须在 10 到 300 秒之间",
            false,
        ));
    }
    if !(10..=300).contains(&input.fallback_timeout_seconds) {
        return Err(CommandError::new(
            "invalid_fallback_timeout",
            "备用模型超时必须在 10 到 300 秒之间",
            false,
        ));
    }
    if !input.input_price_per_million_usd.is_finite()
        || !(0.0..=10_000.0).contains(&input.input_price_per_million_usd)
        || !input.output_price_per_million_usd.is_finite()
        || !(0.0..=10_000.0).contains(&input.output_price_per_million_usd)
    {
        return Err(CommandError::new(
            "invalid_model_pricing",
            "输入和输出价格必须是 0 到 10000 USD/百万 Token 的有限数值",
            false,
        ));
    }
    if input.fallback_enabled
        && (input.fallback_base_url.trim().is_empty() || input.fallback_model.trim().is_empty())
    {
        return Err(CommandError::new(
            "invalid_fallback_config",
            "启用备用模型时 URL 和模型 ID 不能为空",
            false,
        ));
    }
    if let Some(api_key) = input.api_key.as_deref() {
        let entry = keyring::Entry::new(
            "recall-evolution",
            &keyring_account(&input.provider, &input.base_url),
        )
        .map_err(|err| CommandError::new("keychain_error", err.to_string(), false))?;
        if api_key.trim().is_empty() {
            let _ = entry.delete_credential();
        } else {
            entry
                .set_password(api_key.trim())
                .map_err(|err| CommandError::new("keychain_error", err.to_string(), false))?;
        }
    }
    let store = state
        .store
        .lock()
        .map_err(|_| CommandError::new("store_locked", "Store 暂时不可用", true))?;
    store
        .save_config_with_fallback_and_pricing(
            &input.provider,
            input.base_url.trim(),
            input.model.trim(),
            context_mode,
            input.timeout_seconds,
            input.fallback_enabled,
            input.fallback_base_url.trim(),
            input.fallback_model.trim(),
            input.fallback_timeout_seconds,
            input.input_price_per_million_usd,
            input.output_price_per_million_usd,
        )
        .map_err(CommandError::store)?;
    store
        .mark_model_health("unknown", None)
        .map_err(CommandError::store)?;
    store.config().map_err(CommandError::store)
}

#[tauri::command]
fn save_evolution_settings(
    app: AppHandle,
    mut input: EvolutionSettingsInput,
    state: State<'_, AppState>,
) -> Result<EvolutionSettingsView, CommandError> {
    if input.run_mode == "listener" && input.listen_since.is_none() {
        input.listen_since = Some(chrono::Utc::now().timestamp());
    }
    let autostart = app.autolaunch();
    if input.launch_at_login {
        autostart
            .enable()
            .map_err(|err| CommandError::new("autostart_error", err.to_string(), true))?;
    } else {
        autostart
            .disable()
            .map_err(|err| CommandError::new("autostart_error", err.to_string(), true))?;
    }
    let settings = state
        .store
        .lock()
        .map_err(|_| CommandError::new("store_locked", "Store 暂时不可用", true))?
        .save_evolution_settings(&input)
        .map_err(CommandError::store)?;
    let source_watcher = watcher::start(
        app,
        state.activity_signal.clone(),
        PathBuf::from(&settings.codex_source_path),
        PathBuf::from(&settings.claude_source_path),
    )
    .map_err(|err| CommandError::new("source_watch_error", err.to_string(), true))?;
    *state
        .watcher
        .lock()
        .map_err(|_| CommandError::new("watcher_locked", "来源监听器暂时不可用", true))? =
        Some(source_watcher);
    Ok(settings)
}

#[tauri::command]
async fn test_model_connection(
    input: ReflectionConfigInput,
    state: State<'_, AppState>,
) -> Result<serde_json::Value, CommandError> {
    let health_provider = input.provider.clone();
    let health_base_url = input.base_url.clone();
    let health_model = input.model.clone();
    let api_key = input
        .api_key
        .clone()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| {
            keyring::Entry::new(
                "recall-evolution",
                &keyring_account(&input.provider, &input.base_url),
            )
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
            .mark_model_health_for(
                "checking",
                None,
                &health_provider,
                &health_base_url,
                &health_model,
            )
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
                .mark_model_health_for(
                    "ok",
                    None,
                    &health_provider,
                    &health_base_url,
                    &health_model,
                )
                .map_err(CommandError::store)?;
            Ok(value)
        }
        Err(error) => {
            let code = reflection::error_code(&error);
            store
                .mark_model_health_for(
                    "error",
                    Some(&error.to_string()),
                    &health_provider,
                    &health_base_url,
                    &health_model,
                )
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
) -> Result<ReflectionRunResult, CommandError> {
    execute_evolution(&app, &state)
        .await
        .map_err(evolution_command_error)
}

#[tauri::command]
async fn run_evolution_now(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<ReflectionRunResult, CommandError> {
    execute_evolution(&app, &state)
        .await
        .map_err(evolution_command_error)
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
    run_id: String,
) -> Result<ReflectionRunResult, CommandError> {
    let source = {
        let store = state
            .store
            .lock()
            .map_err(|_| CommandError::new("store_locked", "Store 暂时不可用", true))?;
        let detail = store
            .evolution_run_detail(&run_id)
            .map_err(CommandError::store)?;
        if !matches!(
            detail.run.phase.as_str(),
            "failed" | "cancelled" | "interrupted"
        ) {
            return Err(CommandError::new(
                "retry_not_allowed",
                "只有失败、取消或中断的运行可以重试",
                false,
            ));
        }
        store
            .append_audit(
                "evolution_retry_requested",
                Some(&run_id),
                &serde_json::json!({"sourceRunId": run_id}),
            )
            .map_err(CommandError::store)?;
        run_id
    };
    execute_evolution_refs(
        &app,
        &state.store,
        &state.agent_running,
        &state.cancel_requested,
        Some(source),
    )
    .await
    .map_err(evolution_command_error)
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
fn dismiss_recovery_notice(state: State<'_, AppState>) -> Result<(), CommandError> {
    let store = state
        .store
        .lock()
        .map_err(|_| CommandError::new("store_locked", "Store 暂时不可用", true))?;
    store
        .set_app_state("recovery_notice", "")
        .map_err(CommandError::store)?;
    store
        .append_audit(
            "store_recovery_notice_dismissed",
            None,
            &serde_json::json!({"actor": "local-user"}),
        )
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
        None,
    )
    .await
}

async fn execute_evolution_refs(
    app: &AppHandle,
    store_ref: &Arc<Mutex<Store>>,
    running: &Arc<AtomicBool>,
    cancel_requested: &Arc<AtomicBool>,
    retry_of: Option<String>,
) -> Result<ReflectionRunResult, String> {
    if running.swap(true, Ordering::AcqRel) {
        return Err("Evolution Agent 正在运行，请等待当前运行完成".into());
    }
    cancel_requested.store(false, Ordering::Release);
    let result =
        execute_evolution_inner(app, store_ref, cancel_requested, retry_of.as_deref()).await;
    running.store(false, Ordering::Release);
    cancel_requested.store(false, Ordering::Release);
    result
}

async fn execute_evolution_inner(
    app: &AppHandle,
    store_ref: &Arc<Mutex<Store>>,
    cancel_requested: &Arc<AtomicBool>,
    retry_of: Option<&str>,
) -> Result<ReflectionRunResult, String> {
    let run_id = format!("run-{}", uuid::Uuid::new_v4());
    let (settings, config) = {
        let store = store_ref
            .lock()
            .map_err(|_| "store lock poisoned".to_string())?;
        if !store.consent_granted().map_err(|err| err.to_string())? {
            return Err("请先授权读取本机 Agent 会话".into());
        }
        let mut settings = store.evolution_settings().map_err(|err| err.to_string())?;
        if !settings.enabled {
            return Err("Evolution Agent 未启用".into());
        }
        let mut config = store.config().map_err(|err| err.to_string())?;
        let (runner_snapshot, run_mode, providers, lookback_days) =
            if let Some(source_run_id) = retry_of {
                let source = store
                    .evolution_run_detail(source_run_id)
                    .map_err(|err| err.to_string())?;
                if !matches!(
                    source.run.phase.as_str(),
                    "failed" | "cancelled" | "interrupted"
                ) {
                    return Err("原运行状态已变化，不能继续重试".into());
                }
                let snapshot = store
                    .evolution_run_config_snapshot(source_run_id)
                    .map_err(|err| err.to_string())?
                    .ok_or_else(|| "原运行没有不可变配置快照，请新建一次运行".to_string())?;
                config.provider = snapshot.provider.clone();
                config.base_url = snapshot.base_url.clone();
                config.model = snapshot.model.clone();
                config.timeout_seconds = snapshot.timeout_seconds;
                config.fallback_enabled = snapshot.fallback_enabled;
                config.fallback_base_url = snapshot.fallback_base_url.clone();
                config.fallback_model = snapshot.fallback_model.clone();
                config.fallback_timeout_seconds = snapshot.fallback_timeout_seconds;
                config.input_price_per_million_usd = snapshot.input_price_per_million_usd;
                config.output_price_per_million_usd = snapshot.output_price_per_million_usd;
                settings.agent_mode = snapshot.agent_mode.clone();
                settings.auto_activate_low_risk = snapshot.auto_activate_low_risk;
                settings.max_agent_steps = snapshot.max_agent_steps;
                (
                    snapshot,
                    source.run.mode,
                    source.run.providers,
                    source.run.lookback_days,
                )
            } else {
                let providers = [
                    settings.codex_enabled.then_some("codex".to_string()),
                    settings.claude_enabled.then_some("claude-code".to_string()),
                ]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>();
                (
                    RunnerConfigSnapshot::from_views(&config, &settings),
                    settings.run_mode.clone(),
                    providers,
                    settings.lookback_days,
                )
            };
        store
            .start_evolution_run_with_snapshot(
                &run_id,
                &run_mode,
                (!runner_snapshot.model.trim().is_empty())
                    .then_some(runner_snapshot.model.as_str()),
                &providers,
                lookback_days,
                &runner_snapshot,
                retry_of,
            )
            .map_err(|err| err.to_string())?;
        if let Some(source_run_id) = retry_of {
            store
                .append_audit(
                    "evolution_retry_started",
                    Some(&run_id),
                    &serde_json::json!({"sourceRunId": source_run_id}),
                )
                .map_err(|err| err.to_string())?;
        }
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
        if let Some(source_run_id) = retry_of {
            let source = store
                .evolution_run_detail(source_run_id)
                .map_err(|err| err.to_string())?;
            if !matches!(
                source.run.phase.as_str(),
                "failed" | "cancelled" | "interrupted"
            ) {
                return Err("原运行状态已变化，不能继续重试".into());
            }
            let activities = source.activities;
            let active_entries = store.list_entries().map_err(|err| err.to_string())?;
            store
                .update_evolution_run(
                    &run_id,
                    "reading",
                    source.run.scanned_activities,
                    0,
                    0,
                    0,
                    0,
                    None,
                )
                .map_err(|err| err.to_string())?;
            store
                .set_evolution_run_activities(&run_id, &activities)
                .map_err(|err| err.to_string())?;
            return Ok((activities, active_entries, source.run.scanned_activities));
        }
        let since = if settings.run_mode == "listener" {
            settings.listen_since
        } else {
            Some(chrono::Utc::now().timestamp() - settings.lookback_days * 86_400)
        };
        let scan = scanner::scan_sources_with_roots(
            &store,
            settings.lookback_days,
            settings.codex_enabled,
            settings.claude_enabled,
            settings.listen_since,
            std::path::Path::new(&settings.codex_source_path),
            std::path::Path::new(&settings.claude_source_path),
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
            candidate_verifications: Vec::new(),
            provider_used: String::new(),
            fallback_count: 0,
            input_activity_count: 0,
            input_tokens: 0,
            output_tokens: 0,
            duration_ms: 0,
            estimated_cost_usd: None,
        });
    }
    let api_key = keyring::Entry::new(
        "recall-evolution",
        &keyring_account(&config.provider, &config.base_url),
    )
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
        &config.provider,
        config.base_url.clone(),
        config.model.clone(),
        api_key,
        activities,
        active_entries,
        settings.max_agent_steps,
        &settings.agent_mode,
        config.timeout_seconds,
        config.fallback_enabled,
        config.fallback_base_url.clone(),
        config.fallback_model.clone(),
        config.fallback_timeout_seconds,
        config.input_price_per_million_usd,
        config.output_price_per_million_usd,
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
        store
            .set_run_model_usage(&run_id, &result)
            .map_err(|err| err.to_string())?;
    }
    {
        let store = store_ref
            .lock()
            .map_err(|_| "store lock poisoned".to_string())?;
        let (health_provider, health_base_url, health_model) = if config.provider == "remote"
            && result.provider_used == "ollama"
            && config.fallback_enabled
        {
            (
                "ollama",
                config.fallback_base_url.as_str(),
                config.fallback_model.as_str(),
            )
        } else {
            (
                config.provider.as_str(),
                config.base_url.as_str(),
                config.model.as_str(),
            )
        };
        let _ =
            store.mark_model_health_for("ok", None, health_provider, health_base_url, health_model);
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
    activity_signal: Arc<tokio::sync::Notify>,
) {
    tauri::async_runtime::spawn(async move {
        let mut scheduler = store_ref
            .lock()
            .ok()
            .and_then(|store| store.scheduler_state().ok())
            .unwrap_or_default();
        loop {
            tokio::select! {
                _ = tokio::time::sleep(std::time::Duration::from_secs(60)) => {}
                _ = activity_signal.notified() => {}
            }
            if running.load(Ordering::Acquire) {
                continue;
            }
            let now = chrono::Utc::now().timestamp();
            if now < scheduler.retry_after {
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
                    if scheduler != SchedulerState::default() {
                        scheduler = SchedulerState::default();
                        let _ = store.save_scheduler_state(&scheduler);
                    }
                    (false, false)
                } else if settings.run_mode == "scheduled" {
                    if scheduler.listener_pending_count != 0 || scheduler.listener_last_change != 0
                    {
                        scheduler.listener_pending_count = 0;
                        scheduler.listener_last_change = 0;
                        let _ = store.save_scheduler_state(&scheduler);
                    }
                    let due = store
                        .last_evolution_completed_at()
                        .ok()
                        .flatten()
                        .map(|last| now - last >= settings.schedule_hours * 3_600)
                        .unwrap_or(true);
                    (due, false)
                } else {
                    let scanned = scanner::scan_sources_with_roots(
                        &store,
                        settings.lookback_days,
                        settings.codex_enabled,
                        settings.claude_enabled,
                        settings.listen_since,
                        std::path::Path::new(&settings.codex_source_path),
                        std::path::Path::new(&settings.claude_source_path),
                    )
                    .is_ok();
                    let pending_count = if scanned {
                        store.dirty_count_since(settings.listen_since).unwrap_or(0)
                    } else {
                        0
                    };
                    let due = if pending_count == 0 {
                        if scheduler.listener_pending_count != 0
                            || scheduler.listener_last_change != 0
                        {
                            scheduler.listener_pending_count = 0;
                            scheduler.listener_last_change = 0;
                            let _ = store.save_scheduler_state(&scheduler);
                        }
                        false
                    } else if pending_count != scheduler.listener_pending_count {
                        scheduler.listener_pending_count = pending_count;
                        scheduler.listener_last_change = now;
                        let _ = store.save_scheduler_state(&scheduler);
                        false
                    } else {
                        now - scheduler.listener_last_change >= 120
                    };
                    (due, true)
                }
            };
            if due {
                let result =
                    execute_evolution_refs(&app, &store_ref, &running, &cancel_requested, None)
                        .await;
                if result.is_ok() {
                    scheduler.failure_count = 0;
                    scheduler.retry_after = 0;
                } else {
                    scheduler.failure_count = scheduler.failure_count.saturating_add(1);
                    let delay =
                        (60i64 * 2i64.pow(scheduler.failure_count.clamp(0, 6) as u32)).min(3_600);
                    scheduler.retry_after = now + delay;
                }
                if listener_mode {
                    if result.is_ok() {
                        scheduler.listener_pending_count = 0;
                        scheduler.listener_last_change = 0;
                    } else {
                        scheduler.listener_last_change = now;
                    }
                }
                if let Ok(store) = store_ref.lock() {
                    let _ = store.save_scheduler_state(&scheduler);
                }
            }
        }
    });
}

#[tauri::command]
fn set_entry_status(
    id: String,
    status: String,
    state: State<'_, AppState>,
) -> Result<(), CommandError> {
    if !matches!(status.as_str(), "active" | "rejected" | "disabled") {
        return Err(CommandError::new("invalid_status", "无效的条目状态", false));
    }
    let store = state
        .store
        .lock()
        .map_err(|_| CommandError::new("store_locked", "Store 暂时不可用", true))?;
    store
        .set_entry_status(&id, &status)
        .map_err(CommandError::store)
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
fn install_mcp(state: State<'_, AppState>) -> Result<McpInstallResult, CommandError> {
    let store = state
        .store
        .lock()
        .map_err(|_| CommandError::new("store_locked", "Store 暂时不可用", true))?;
    let sidecar = mcp::sidecar_path()
        .map_err(|err| CommandError::new("mcp_sidecar_missing", err.to_string(), false))?;
    if !sidecar.exists() {
        return Err(CommandError::new(
            "mcp_sidecar_missing",
            format!("MCP sidecar 尚未构建: {}", sidecar.display()),
            false,
        ));
    }
    let result = mcp::install(&sidecar, store.path())
        .map_err(|err| CommandError::new("mcp_install_failed", err.to_string(), true))?;
    store
        .append_audit(
            "mcp_installed",
            None,
            &serde_json::json!({"backups": result.backups.len()}),
        )
        .map_err(CommandError::store)?;
    Ok(result)
}

fn record_mcp_health(store: &Store, status: &str, error: Option<&str>) {
    let _ = store.set_app_state(
        "mcp_last_checked",
        &chrono::Utc::now().timestamp().to_string(),
    );
    let _ = store.set_app_state("mcp_health_status", status);
    let safe_error = error
        .map(scanner::redact)
        .unwrap_or_default()
        .chars()
        .take(240)
        .collect::<String>();
    let _ = store.set_app_state("mcp_health_error", &safe_error);
}

#[tauri::command]
fn test_mcp(state: State<'_, AppState>) -> Result<serde_json::Value, CommandError> {
    let store = state
        .store
        .lock()
        .map_err(|_| CommandError::new("store_locked", "Store 暂时不可用", true))?;
    let sidecar = mcp::sidecar_path()
        .map_err(|err| CommandError::new("mcp_sidecar_missing", err.to_string(), false))?;
    match mcp::smoke_test(&sidecar, store.path()) {
        Ok(result) => {
            record_mcp_health(&store, "ok", None);
            Ok(result)
        }
        Err(error) => {
            record_mcp_health(&store, "error", Some(&error.to_string()));
            Err(CommandError::new(
                "mcp_test_failed",
                error.to_string(),
                true,
            ))
        }
    }
}

#[tauri::command]
fn test_mcp_target(
    target: String,
    state: State<'_, AppState>,
) -> Result<serde_json::Value, CommandError> {
    if !matches!(target.as_str(), "codex" | "claude-code") {
        return Err(CommandError::new(
            "invalid_mcp_target",
            "MCP target must be codex or claude-code",
            false,
        ));
    }
    let store = state
        .store
        .lock()
        .map_err(|_| CommandError::new("store_locked", "Store 暂时不可用", true))?;
    let status = mcp::status(&store);
    let configured = if target == "codex" {
        status.codex
    } else {
        status.claude
    };
    if !configured {
        record_mcp_health(
            &store,
            "error",
            Some(&format!("{target} 尚未安装 Recall MCP")),
        );
        return Err(CommandError::new(
            "mcp_not_installed",
            format!("{target} 尚未安装 Recall MCP"),
            false,
        ));
    }
    let sidecar = mcp::sidecar_path()
        .map_err(|err| CommandError::new("mcp_sidecar_missing", err.to_string(), false))?;
    match mcp::smoke_test(&sidecar, store.path()) {
        Ok(smoke) => {
            record_mcp_health(&store, "ok", None);
            Ok(serde_json::json!({"target": target, "configured": true, "smoke": smoke}))
        }
        Err(error) => {
            record_mcp_health(&store, "error", Some(&error.to_string()));
            Err(CommandError::new(
                "mcp_test_failed",
                error.to_string(),
                true,
            ))
        }
    }
}

#[tauri::command]
fn uninstall_mcp(state: State<'_, AppState>) -> Result<serde_json::Value, CommandError> {
    let result = mcp::uninstall()
        .map_err(|err| CommandError::new("mcp_uninstall_failed", err.to_string(), true))?;
    let store = state
        .store
        .lock()
        .map_err(|_| CommandError::new("store_locked", "Store 暂时不可用", true))?;
    store
        .append_audit("mcp_uninstalled", None, &result)
        .map_err(CommandError::store)?;
    Ok(result)
}

fn snapshot(store: &Store) -> Result<DashboardSnapshot, store::StoreError> {
    let sessions = store.list_sessions(200)?;
    let evolution = store.evolution_settings()?;
    let codex_root = PathBuf::from(&evolution.codex_source_path);
    let claude_root = PathBuf::from(&evolution.claude_source_path);
    let mut sources = vec![
        SourceSummary {
            provider: "codex".to_string(),
            root: scanner::display_source_root(&codex_root),
            available: codex_root.join("sessions").is_dir()
                || codex_root.join("archived_sessions").is_dir(),
            session_count: 0,
            activity_count: 0,
            error: None,
            last_scanned_at: None,
            error_count: 0,
            cursor_count: 0,
        },
        SourceSummary {
            provider: "claude-code".to_string(),
            root: scanner::display_source_root(&claude_root),
            available: claude_root.join("projects").is_dir(),
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
        evolution,
        run,
        run_history: store.list_evolution_runs(100)?,
        store_stats: store.store_stats()?,
        redaction_report: store.redaction_report()?,
        cache_cleanup_preview: store.cache_cleanup_preview()?,
        backups: store.list_backups()?,
        audit_events: store.list_audit_events(100)?,
        mcp: mcp::status(store),
        recovery_notice: store.recovery_notice()?,
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
    let store = open_store_for_app(paths::store_path());
    let _ = store.recover_interrupted_runs();
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _, _| {
            show_main_window(app);
        }))
        .plugin(tauri_plugin_autostart::Builder::new().build())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .manage(AppState {
            store: Arc::new(Mutex::new(store)),
            watcher: Mutex::new(None),
            agent_running: Arc::new(AtomicBool::new(false)),
            cancel_requested: Arc::new(AtomicBool::new(false)),
            activity_signal: Arc::new(tokio::sync::Notify::new()),
            tray: Mutex::new(None),
        })
        .setup(|app| {
            let state = app.state::<AppState>();
            let settings = state
                .store
                .lock()
                .map_err(|_| std::io::Error::other("store lock poisoned"))?
                .evolution_settings()
                .map_err(|error| std::io::Error::other(error.to_string()))?;
            let source_watcher = watcher::start(
                app.handle().clone(),
                state.activity_signal.clone(),
                PathBuf::from(settings.codex_source_path),
                PathBuf::from(settings.claude_source_path),
            )?;
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
                state.activity_signal.clone(),
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
            dismiss_recovery_notice,
            install_mcp,
            test_mcp,
            test_mcp_target,
            uninstall_mcp
        ])
        .run(tauri::generate_context!())
        .expect("error while running Recall Memory");
}

#[cfg(test)]
mod tests {
    use super::{evolution_command_error, keyring_account, open_store_for_app};

    #[test]
    fn keyring_credentials_are_scoped_to_provider_and_endpoint() {
        let openai = keyring_account("remote", "https://api.openai.com/v1");
        assert_eq!(
            openai,
            keyring_account("remote", "https://api.openai.com/v1/")
        );
        assert_ne!(
            openai,
            keyring_account("remote", "https://gateway.example/v1")
        );
        assert_ne!(
            openai,
            keyring_account("ollama", "https://api.openai.com/v1")
        );
    }

    #[test]
    fn evolution_command_errors_keep_stable_codes_and_redact_messages() {
        let error = evolution_command_error(
            "HTTP 401 api_key=secret-value /Users/alice/project".to_string(),
        );
        assert_eq!(error.code, "unauthorized");
        assert!(!error.message.contains("secret-value"));
        assert!(!error.message.contains("/Users/alice"));
        assert!(!error.retryable);
    }

    #[test]
    fn corrupted_store_is_quarantined_and_reported_to_the_ui() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("evolution.sqlite3");
        std::fs::write(&path, b"not a sqlite database").unwrap();
        let store = open_store_for_app(path.clone());
        assert!(store.recovery_notice().unwrap().unwrap().contains("已隔离"));
        assert!(dir
            .path()
            .read_dir()
            .unwrap()
            .filter_map(Result::ok)
            .any(|entry| entry.file_name().to_string_lossy().contains(".corrupt-")));
        assert!(path.exists());
    }
}

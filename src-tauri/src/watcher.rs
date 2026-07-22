use crate::paths;
use notify::{Config, Event, RecommendedWatcher, RecursiveMode, Watcher};
use serde::Serialize;
use std::path::Path;
use std::sync::Arc;
use tauri::{AppHandle, Emitter};
use tokio::sync::Notify;

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct SourceDirtyEvent {
    provider: String,
    source: String,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct SourceWatchErrorEvent {
    provider: String,
    source: String,
    error: String,
}

pub fn start(app: AppHandle, activity_signal: Arc<Notify>) -> notify::Result<RecommendedWatcher> {
    let callback_app = app.clone();
    let status_app = app.clone();
    let mut watcher = RecommendedWatcher::new(
        move |result: notify::Result<Event>| {
            let Ok(event) = result else { return };
            for path in event.paths {
                if path.extension().and_then(|value| value.to_str()) != Some("jsonl") {
                    continue;
                }
                let provider = provider_for(&path);
                activity_signal.notify_one();
                let _ = callback_app.emit(
                    "source-dirty",
                    SourceDirtyEvent {
                        provider: provider.to_string(),
                        source: hashed_source(&path),
                    },
                );
            }
        },
        Config::default(),
    )?;

    for path in [
        paths::codex_home().join("sessions"),
        paths::codex_home().join("archived_sessions"),
        paths::claude_home().join("projects"),
    ] {
        if path.is_dir() {
            if let Err(error) = watcher.watch(&path, RecursiveMode::Recursive) {
                let provider = provider_for(&path);
                let _ = status_app.emit(
                    "source-watch-error",
                    SourceWatchErrorEvent {
                        provider: provider.to_string(),
                        source: hashed_source(&path),
                        error: crate::scanner::redact(&error.to_string()),
                    },
                );
            }
        }
    }
    Ok(watcher)
}

fn provider_for(path: &Path) -> &'static str {
    if path.starts_with(paths::claude_home()) {
        "claude-code"
    } else {
        "codex"
    }
}

fn hashed_source(path: &Path) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(path.to_string_lossy().as_bytes());
    hex::encode(digest)[..12].to_string()
}

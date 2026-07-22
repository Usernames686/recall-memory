use std::path::PathBuf;

pub fn app_data_dir() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("Recall")
}

pub fn store_path() -> PathBuf {
    app_data_dir().join("evolution.sqlite3")
}

pub fn codex_home() -> PathBuf {
    std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|p| p.join(".codex")))
        .unwrap_or_else(|| PathBuf::from(".codex"))
}

pub fn claude_home() -> PathBuf {
    std::env::var_os("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|p| p.join(".claude")))
        .unwrap_or_else(|| PathBuf::from(".claude"))
}

pub fn codex_config_path() -> PathBuf {
    codex_home().join("config.toml")
}

pub fn claude_config_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".claude.json")
}

pub fn backup_path(path: &std::path::Path) -> PathBuf {
    let stamp = chrono::Utc::now().format("%Y%m%d%H%M%S");
    PathBuf::from(format!("{}.recall-backup-{}", path.display(), stamp))
}

# Recall Memory

Recall Memory is a local-first macOS app that turns redacted Codex and Claude Code activity into reviewable Meta and Skill candidates.

## MVP scope

- Read-only Codex and Claude Code JSONL session scanners.
- Redaction before local storage or model input.
- Restricted Evolution Agent with Ollama/Qwen3 or OpenAI-compatible providers.
- Review queue, immutable revisions, diffs, per-run rollback, audit history.
- Two read-only MCP tools that expose only Active Meta and Skill entries.
- Local SQLite backup, restore, redacted export, and cache maintenance.

Original session files are never modified. The app only supports Codex and Claude Code in this MVP.

## Development

Requirements: macOS, Node.js, pnpm, and Rust.

```bash
pnpm install
pnpm tauri:dev
```

For a release bundle:

```bash
pnpm tauri:build
```

The default local model configuration is `http://127.0.0.1:11434/v1` with `qwen3:8b`. An OpenAI-compatible endpoint can be configured in the Settings page. API keys are stored in the macOS Keychain, not SQLite.

## Verification

```bash
cargo test --manifest-path src-tauri/Cargo.toml --all-targets
pnpm build
```

The generated macOS app and DMG are build artifacts and are intentionally excluded from Git by `.gitignore`.

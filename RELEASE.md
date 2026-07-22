# macOS Release Checklist

Local `pnpm tauri:build` and `pnpm tauri:build:universal` builds are ad-hoc
signed for development. A distributable build must run the release workflow
with these GitHub Actions secrets configured:

- `APPLE_CERTIFICATE`: base64 Developer ID Application certificate
- `APPLE_CERTIFICATE_PASSWORD`
- `APPLE_SIGNING_IDENTITY`
- `APPLE_ID`, `APPLE_PASSWORD`, and `APPLE_TEAM_ID` for notarization
- `TAURI_SIGNING_PRIVATE_KEY` and `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` for
  updater artifacts, once an updater public key is configured in the app

The workflow verifies both the main executable and MCP sidecar contain
`arm64` and `x86_64`, notarizes the Universal DMG, staples the ticket, and
uploads it to the tagged GitHub release. Tauri updater signing is intentionally
separate and must not be enabled until a project-owned updater key is created
and stored outside the repository.

Before tagging a release, run the real model acceptance described in
[`REAL_MODEL_E2E.md`](REAL_MODEL_E2E.md) and record its provider and run id in
the release notes without recording credentials or raw session content.

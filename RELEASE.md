# macOS Release Checklist

Local `pnpm tauri:build` and `pnpm tauri:build:universal` builds are ad-hoc
signed for development. A distributable build must run the release workflow
with these GitHub Actions secrets configured:

- `APPLE_CERTIFICATE`: base64 Developer ID Application certificate
- `APPLE_CERTIFICATE_PASSWORD`
- `APPLE_SIGNING_IDENTITY`
- `APPLE_ID`, `APPLE_PASSWORD`, and `APPLE_TEAM_ID` for notarization

The workflow verifies both the main executable and MCP sidecar contain
`arm64` and `x86_64`, notarizes the Universal DMG, staples the ticket, and
uploads it to the tagged GitHub release. Tauri updater signing is intentionally
separate and must not be enabled until a project-owned updater key is created
and stored outside the repository.

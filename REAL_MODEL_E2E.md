# Real model acceptance

The default test suite uses a local mock OpenAI-compatible server. A real
acceptance run is intentionally opt-in and never writes credentials to the
repository or SQLite.

Set the endpoint, model, and key only in the shell environment:

```bash
export RECALL_REAL_MODEL_BASE_URL="https://your-gateway.example/v1"
export RECALL_REAL_MODEL_ID="your-tool-calling-model"
export RECALL_REAL_MODEL_API_KEY="..."
pnpm test:real-model
```

The model must support OpenAI-compatible tool calls and return the restricted
sequence for verification mode. `pnpm test:real-model` creates a temporary
SQLite Store, parses the checked-in redacted Codex and Claude Code JSONL
fixtures, runs candidate-level verification, persists the candidate and
consumed activity batch, approves one candidate, and calls both read-only MCP
tools. It also asserts that the approved content is present in the context
exposed to the next round. The run must complete without fallback.

For the desktop product acceptance, use the same endpoint in Settings, scan a
redacted Codex or Claude Code session, run verification mode, approve the
candidate in the Review page, and call both read-only MCP tools. Record the run
id and provider in the release checklist; never record the API key or raw
session text.

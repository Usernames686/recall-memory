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
sequence for verification mode. The test asserts that the provider completes
the context/activity read, candidate proposal, candidate-level verification,
and structured finish protocol without fallback.

For the full product acceptance, use the same endpoint in Settings, scan a
redacted Codex or Claude Code fixture, run verification mode, approve the
candidate in the Review page, and call both read-only MCP tools. Record the
run id and provider in the release checklist; never record the API key or raw
session text.

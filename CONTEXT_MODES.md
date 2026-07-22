# Context Modes

Recall Memory exposes two context-reading modes for the Codex and Claude Code MCP bridge.

## Guided

`guided` is the default. The `evolution_context` tool description asks an Agent to call
`evolution_context(action="meta")` at the start of a new task. The response includes
the latest active Meta, a compact learned-Skill index, and a `context_text` block that
can be used directly as task context. Full Skill bodies are loaded only when needed.

Guided mode is a protocol-level instruction. An MCP server cannot force a client to
call a tool or mutate that client's system prompt.

## MCP

`mcp` keeps the same tool surface but makes no start-of-task recommendation. Agents
read context explicitly when their workflow requires it.

## Safety and compatibility

- Only `active` entries are returned. Pending and rejected candidates remain local to
  the review queue.
- The MCP bridge exposes exactly two read-only tools: `evolution_context` and
  `evolution_run_status`. Candidate creation stays inside the restricted desktop
  Agent and review workflow.
- Context mode is stored in the existing SQLite key/value configuration. Existing
  installations without a mode default to `guided`.
- Host-level automatic injection is intentionally out of scope. It requires a
  Codex/Claude Runner integration rather than an MCP sidecar change.

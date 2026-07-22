import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react"
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest"
import type { EvolutionRunDetail, EvolutionRunState, Snapshot } from "./lib/types"

const runtime = vi.hoisted(() => ({
  invoke: vi.fn(),
  listen: vi.fn(),
}))

vi.mock("./lib/runtime", () => ({
  isTauri: () => true,
  invoke: runtime.invoke,
}))

vi.mock("@tauri-apps/api/event", () => ({
  listen: runtime.listen,
}))

import App from "./App"

function runState(phase: EvolutionRunState["phase"]): EvolutionRunState {
  return {
    runId: "run-fixture",
    mode: "manual",
    phase,
    startedAt: 100,
    completedAt: ["completed", "failed", "cancelled", "interrupted"].includes(phase) ? 110 : undefined,
    scannedActivities: 1,
    consumedActivities: phase === "completed" ? 1 : 0,
    generated: 1,
    activated: 0,
    pending: 1,
    error: phase === "failed" ? "mock failure" : undefined,
    model: "mock-model",
    providers: ["codex"],
    lookbackDays: 7,
    agentMode: "verification",
    traceCount: 1,
    verificationStatus: "review_required",
    verificationSummary: "one candidate needs review",
    providerUsed: "remote",
    fallbackCount: 0,
    inputActivityCount: 1,
    inputTokens: 20,
    outputTokens: 10,
    durationMs: 50,
  }
}

function snapshot(phase: EvolutionRunState["phase"] = "completed"): Snapshot {
  const run = runState(phase)
  return {
    consentGranted: true,
    sources: [{ provider: "codex", root: "~/.codex", available: true, sessionCount: 1, activityCount: 1, errorCount: 0, cursorCount: 1 }],
    sessions: [],
    activities: [],
    runActivities: [],
    entries: [{ id: "entry-1", kind: "skill", title: "Verify releases", summary: "Run checks before release", body: "Run focused tests, then the full build.", status: "pending", risk: "review", sourceRefs: ["activity-1"], updatedAt: 100, originRunId: run.runId, version: 1 }],
    pendingCount: 1,
    activityCount: 1,
    dirtyCount: 0,
    config: { provider: "remote", baseUrl: "https://api.example/v1", model: "mock-model", hasApiKey: true, contextMode: "guided", timeoutSeconds: 90, fallbackEnabled: true, fallbackBaseUrl: "http://127.0.0.1:11434/v1", fallbackModel: "qwen3:8b", fallbackTimeoutSeconds: 90, inputPricePerMillionUsd: 0, outputPricePerMillionUsd: 0, healthStatus: "ok" },
    evolution: { enabled: true, codexEnabled: true, claudeEnabled: true, lookbackDays: 7, runMode: "manual", scheduleHours: 12, autoActivateLowRisk: false, maxAgentSteps: 6, launchAtLogin: false, notificationsEnabled: true, agentMode: "verification", codexSourcePath: "/Users/test/.codex", claudeSourcePath: "/Users/test/.claude" },
    run,
    runHistory: [run],
    storeStats: { databasePath: "~/Recall/evolution.sqlite3", databaseBytes: 1024, entryCount: 1, activeCount: 0, pendingCount: 1, versionCount: 1, activityCount: 1, reflectedActivityCount: 0, runCount: 1, auditCount: 1 },
    redactionReport: { processedRecords: 1, redactedRecords: 0, redactionCount: 0, categories: [] },
    cacheCleanupPreview: { reflectedActivities: 0, runActivityLinks: 0, affectedRuns: 0, preservedEntries: 1, preservedVersions: 1 },
    backups: [],
    auditEvents: [],
    mcp: { codex: true, claude: false, recentCalls: [] },
  }
}

function detail(current: Snapshot): EvolutionRunDetail {
  return {
    run: current.run!,
    activities: [{ id: "activity-1", provider: "codex", sessionId: "session-1", sourcePath: "codex:fixture", kind: "assistant_final", role: "assistant", text: "Always run focused tests before the full build.", occurredAt: 100, metadata: {} }],
    entries: current.entries,
    traces: [],
    candidateVerifications: [{ runId: "run-fixture", entryId: "entry-1", evidenceSufficient: false, supportingEvidence: ["activity-1"], contradictingEvidence: ["activity-2"], confidence: 0.62, duplicate: true, conflict: false, recommendation: "review", rationale: "Evidence is mixed." }],
  }
}

function mockBackend(current: Snapshot) {
  runtime.invoke.mockImplementation(async (command: string, args?: Record<string, unknown>) => {
    if (command === "get_snapshot") return current
    if (command === "get_evolution_run_trace") return []
    if (command === "get_evolution_run_detail") return detail(current)
    if (command === "save_evolution_settings") return args?.input
    if (command === "save_reflection_config") return { ...current.config, ...(args?.input as object) }
    if (command === "test_model_connection") throw { code: "unauthorized", message: "API Key 已失效", retryable: false }
    if (command === "retry_evolution") return { runId: "run-retry", generated: [], activated: 0, pending: 0, discarded: 0, message: "重试完成", providerUsed: "remote", fallbackCount: 0, inputActivityCount: 1, inputTokens: 1, outputTokens: 1, durationMs: 1 }
    return undefined
  })
}

beforeEach(() => {
  runtime.invoke.mockReset()
  runtime.listen.mockReset()
  runtime.listen.mockResolvedValue(() => {})
})

afterEach(cleanup)

describe("Recall Memory Tauri workflows", () => {
  it("saves the selected lookback and surfaces structured model errors", async () => {
    const current = snapshot()
    mockBackend(current)
    render(<App />)

    fireEvent.click(await screen.findByRole("button", { name: "设置" }))
    fireEvent.click(within(screen.getByLabelText("设置分类")).getByRole("button", { name: "数据源" }))
    fireEvent.click(screen.getByRole("button", { name: "30 天" }))
    fireEvent.change(screen.getByLabelText("Codex 根目录"), { target: { value: "/Volumes/Agents/codex" } })
    fireEvent.click(screen.getByRole("button", { name: "保存设置" }))
    await waitFor(() => expect(runtime.invoke).toHaveBeenCalledWith("save_evolution_settings", { input: expect.objectContaining({ lookbackDays: 30, codexSourcePath: "/Volumes/Agents/codex" }) }))

    fireEvent.click(screen.getByRole("button", { name: "进化 Agent" }))
    fireEvent.click(screen.getByRole("button", { name: "测试连接" }))
    expect(await screen.findByText("API Key 已失效")).toBeInTheDocument()
  })

  it("saves per-model pricing used by run cost estimates", async () => {
    const current = snapshot()
    mockBackend(current)
    render(<App />)

    fireEvent.click(await screen.findByRole("button", { name: "设置" }))
    fireEvent.change(screen.getByLabelText("输入价格（USD / 百万 Token）"), { target: { value: "1.25" } })
    fireEvent.change(screen.getByLabelText("输出价格（USD / 百万 Token）"), { target: { value: "5" } })
    fireEvent.click(screen.getByRole("button", { name: "保存模型" }))
    await waitFor(() => expect(runtime.invoke).toHaveBeenCalledWith("save_reflection_config", {
      input: expect.objectContaining({ inputPricePerMillionUsd: 1.25, outputPricePerMillionUsd: 5 }),
    }))
  })

  it("cancels a running agent", async () => {
    const current = snapshot("analyzing")
    mockBackend(current)
    render(<App />)

    fireEvent.click(await screen.findByRole("button", { name: "Evolution Agent" }))
    fireEvent.click(screen.getByRole("button", { name: "取消运行" }))
    await waitFor(() => expect(runtime.invoke).toHaveBeenCalledWith("cancel_evolution", undefined))
  })

  it("retries a failed immutable run snapshot", async () => {
    const current = snapshot("failed")
    mockBackend(current)
    render(<App />)

    fireEvent.click(await screen.findByRole("button", { name: "Evolution Agent" }))
    fireEvent.click(screen.getByRole("button", { name: "重试" }))
    await waitFor(() => expect(runtime.invoke).toHaveBeenCalledWith("retry_evolution", { runId: "run-fixture" }))
  })

  it("shows candidate-level verification and redacted evidence", async () => {
    const current = snapshot()
    mockBackend(current)
    render(<App />)

    fireEvent.click(await screen.findByRole("button", { name: "审核中心" }))
    fireEvent.click(screen.getByRole("button", { name: "证据" }))
    expect(await screen.findByText("建议复核")).toBeInTheDocument()
    expect(screen.getByText(/证据不足.*检测到重复.*未发现冲突/)).toBeInTheDocument()
    expect(screen.getByText("反对证据：activity-2")).toBeInTheDocument()
    expect(screen.getByText("Always run focused tests before the full build.")).toBeInTheDocument()
  })

  it("shows and dismisses a persisted Store recovery notice", async () => {
    const current = { ...snapshot(), recoveryNotice: "检测到本地 Store 损坏，旧文件已隔离。" }
    mockBackend(current)
    render(<App />)

    expect(await screen.findByText("检测到本地 Store 损坏，旧文件已隔离。")).toBeInTheDocument()
    fireEvent.click(screen.getByRole("button", { name: "关闭恢复提示" }))
    await waitFor(() => expect(runtime.invoke).toHaveBeenCalledWith("dismiss_recovery_notice", undefined))
  })
})

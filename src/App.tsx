import { useCallback, useEffect, useMemo, useState } from "react"
import {
  Activity,
  BookOpen,
  Bot,
  Check,
  ChevronRight,
  CircleAlert,
  CircleCheck,
  Clock3,
  Cpu,
  Database,
  Download,
  FileText,
  Fingerprint,
  FolderOpen,
  KeyRound,
  Layers3,
  History,
  HardDrive,
  ListChecks,
  LockKeyhole,
  Menu,
  Network,
  Play,
  RefreshCw,
  RotateCcw,
  Settings2,
  ShieldCheck,
  Sparkles,
  TerminalSquare,
  TimerReset,
  Trash2,
  X,
} from "lucide-react"
import { listen } from "@tauri-apps/api/event"
import type {
  AgentMode,
  AgentTraceEvent,
  CandidateVerification,
  ContextMode,
  EvolutionEntry,
  EvolutionRunDetail,
  EvolutionPhase,
  EvolutionRunState,
  EvolutionSettings,
  EntryVersion,
  EntryVersionDiff,
  MaintenanceResult,
  ModelProvider,
  Provider,
  ReflectionConfig,
  ReflectionConfigInput,
  ReflectionResult,
  RunRollbackResult,
  ScanResult,
  Snapshot,
  SourceSummary,
} from "./lib/types"
import { invoke, isTauri } from "./lib/runtime"

type View = "overview" | "agent" | "runs" | "review" | "sources" | "repository" | "management" | "settings"
type SettingsTab = "agent" | "sources" | "automation" | "safety" | "connection"

const defaultEvolution: EvolutionSettings = {
  enabled: true,
  codexEnabled: true,
  claudeEnabled: true,
  lookbackDays: 30,
  runMode: "manual",
  scheduleHours: 12,
  autoActivateLowRisk: true,
  maxAgentSteps: 6,
  launchAtLogin: false,
  notificationsEnabled: true,
  agentMode: "reflection",
  codexSourcePath: "~/.codex",
  claudeSourcePath: "~/.claude",
}

const emptySnapshot: Snapshot = {
  consentGranted: false,
  sources: [],
  sessions: [],
  activities: [],
  runActivities: [],
  entries: [],
  runHistory: [],
  storeStats: { databasePath: "", databaseBytes: 0, entryCount: 0, activeCount: 0, pendingCount: 0, versionCount: 0, activityCount: 0, reflectedActivityCount: 0, runCount: 0, auditCount: 0 },
  redactionReport: { processedRecords: 0, redactedRecords: 0, redactionCount: 0, categories: [] },
  cacheCleanupPreview: { reflectedActivities: 0, runActivityLinks: 0, affectedRuns: 0, preservedEntries: 0, preservedVersions: 0 },
  backups: [],
  auditEvents: [],
  mcp: { codex: false, claude: false, recentCalls: [] },
  pendingCount: 0,
  activityCount: 0,
  dirtyCount: 0,
  config: { provider: "remote", baseUrl: "", model: "", hasApiKey: false, contextMode: "guided", timeoutSeconds: 90, fallbackEnabled: true, fallbackBaseUrl: "http://127.0.0.1:11434/v1", fallbackModel: "qwen3:8b", fallbackTimeoutSeconds: 90, inputPricePerMillionUsd: 0, outputPricePerMillionUsd: 0, healthStatus: "unknown" },
  evolution: defaultEvolution,
}

const nav = [
  { id: "overview" as const, label: "总览", icon: Layers3 },
  { id: "agent" as const, label: "Evolution Agent", icon: Bot },
  { id: "runs" as const, label: "运行历史", icon: History },
  { id: "review" as const, label: "审核中心", icon: ListChecks },
  { id: "sources" as const, label: "数据源", icon: FolderOpen },
  { id: "repository" as const, label: "沉淀仓库", icon: BookOpen },
  { id: "management" as const, label: "数据管理", icon: HardDrive },
]

const phaseOrder: EvolutionPhase[] = ["scanning", "reading", "analyzing", "validating", "persisting", "completed"]
const phaseLabel: Record<EvolutionPhase, string> = {
  idle: "空闲",
  listening: "监听中",
  queued: "排队中",
  scanning: "扫描会话",
  reading: "读取上下文",
  analyzing: "Agent 分析",
  validating: "安全校验",
  persisting: "保存结果",
  cancelling: "正在取消",
  cancelled: "已取消",
  interrupted: "已中断",
  completed: "已完成",
  failed: "运行失败",
}

function formatCount(value: number) {
  return new Intl.NumberFormat("zh-CN").format(value)
}

function formatTime(value?: number) {
  if (!value) return "尚未运行"
  return new Intl.DateTimeFormat("zh-CN", { month: "short", day: "numeric", hour: "2-digit", minute: "2-digit" }).format(new Date(value * 1000))
}

function formatDuration(startedAt: number, completedAt?: number) {
  if (!completedAt) return "运行中"
  const seconds = Math.max(0, completedAt - startedAt)
  if (seconds < 60) return `${seconds} 秒`
  return `${Math.floor(seconds / 60)} 分 ${seconds % 60} 秒`
}

function providerAccent(provider: Provider) {
  return provider === "codex" ? "codex" : "claude"
}

function isLocalModelUrl(value: string) {
  try {
    const url = new URL(value)
    return url.protocol === "http:" && ["127.0.0.1", "localhost", "::1"].includes(url.hostname)
  } catch {
    return false
  }
}

function runIsBusy(run?: EvolutionRunState) {
  return Boolean(run && !["completed", "failed", "cancelled", "interrupted", "idle", "listening"].includes(run.phase))
}

function modelConfigured(config: ReflectionConfig) {
  return Boolean(config.baseUrl && config.model && (config.hasApiKey || isLocalModelUrl(config.baseUrl)))
}

function errorText(error: unknown) {
  if (error instanceof Error) return error.message
  if (typeof error === "string") return error
  if (error && typeof error === "object" && "message" in error && typeof error.message === "string") return error.message
  return "操作失败，请稍后重试"
}

export default function App() {
  const [view, setView] = useState<View>("overview")
  const [snapshot, setSnapshot] = useState<Snapshot>(emptySnapshot)
  const [loading, setLoading] = useState(true)
  const [busy, setBusy] = useState<string | null>(null)
  const [notice, setNotice] = useState<{ tone: "success" | "error" | "info"; text: string } | null>(null)
  const [menuOpen, setMenuOpen] = useState(false)
  const [liveTrace, setLiveTrace] = useState<AgentTraceEvent[]>([])

  const loadSnapshot = useCallback(async () => {
    setLoading(true)
    try {
      const next = await invoke<Snapshot>("get_snapshot")
      setSnapshot(next)
      if (next.run?.runId) {
        try {
          setLiveTrace(await invoke<AgentTraceEvent[]>("get_evolution_run_trace", { runId: next.run.runId }))
        } catch {
          setLiveTrace([])
        }
      } else {
        setLiveTrace([])
      }
    } catch (error) {
      setNotice({ tone: "info", text: errorText(error) })
    } finally {
      setLoading(false)
    }
  }, [])

  useEffect(() => { void loadSnapshot() }, [loadSnapshot])

  useEffect(() => {
    if (!isTauri()) return
    let disposed = false
    let sourceRefreshTimer: ReturnType<typeof window.setTimeout> | undefined
    const cleanups: Array<() => void> = []
    void listen<{ provider: string }>("source-dirty", (event) => {
      setNotice({ tone: "info", text: `${event.payload.provider === "codex" ? "Codex" : "Claude Code"} 检测到新会话活动` })
      if (sourceRefreshTimer) window.clearTimeout(sourceRefreshTimer)
      sourceRefreshTimer = window.setTimeout(() => { void loadSnapshot() }, 250)
    }).then((unlisten) => disposed ? unlisten() : cleanups.push(unlisten))
    void listen<{ provider: string; error: string }>("source-watch-error", (event) => {
      const label = event.payload.provider === "codex" ? "Codex" : "Claude Code"
      setNotice({ tone: "error", text: `${label} 监听不可用：${event.payload.error}。仍可手动扫描。` })
    }).then((unlisten) => disposed ? unlisten() : cleanups.push(unlisten))
    void listen<EvolutionRunState>("evolution-state", (event) => {
      setSnapshot((current) => ({ ...current, run: event.payload }))
      if (["completed", "failed"].includes(event.payload.phase)) void loadSnapshot()
    }).then((unlisten) => disposed ? unlisten() : cleanups.push(unlisten))
    void listen<AgentTraceEvent>("evolution-trace", (event) => {
      setLiveTrace((current) => current.some((item) => item.id === event.payload.id) ? current : [...current, event.payload].slice(-200))
    }).then((unlisten) => disposed ? unlisten() : cleanups.push(unlisten))
    return () => {
      disposed = true
      if (sourceRefreshTimer) window.clearTimeout(sourceRefreshTimer)
      cleanups.forEach((cleanup) => cleanup())
    }
  }, [loadSnapshot])

  const runAction = async <T,>(label: string, command: string, args?: Record<string, unknown>) => {
    setBusy(label)
    try {
      const result = await invoke<T>(command, args)
      await loadSnapshot()
      setNotice({ tone: "success", text: "操作完成" })
      return result
    } catch (error) {
      const text = errorText(error)
      setNotice({ tone: text.includes("已取消") ? "info" : "error", text })
      return null
    } finally {
      setBusy(null)
    }
  }

  const authorize = async () => {
    setBusy("authorize")
    try {
      await invoke<Snapshot>("set_consent", { granted: true })
      const result = await invoke<ScanResult>("scan_sessions", { days: snapshot.evolution.lookbackDays })
      await loadSnapshot()
      setNotice({ tone: "success", text: `授权完成，新增 ${result.newActivities} 条脱敏活动` })
    } catch (error) {
      setNotice({ tone: "error", text: errorText(error) })
    } finally {
      setBusy(null)
    }
  }

  const saveEvolution = async (settings: EvolutionSettings) => {
    const next = await runAction<EvolutionSettings>("settings", "save_evolution_settings", { input: settings })
    if (next) setSnapshot((current) => ({ ...current, evolution: next }))
    return next
  }

  const saveModel = async (input: ReflectionConfigInput) => {
    const next = await runAction<ReflectionConfig>("model", "save_reflection_config", { input })
    if (next) setSnapshot((current) => ({ ...current, config: next }))
    return next
  }

  const updateEntry = (id: string, status: "active" | "rejected" | "disabled") => {
    const reason = status === "rejected" ? window.prompt("请输入拒绝原因")?.trim() : status === "active" ? "人工审核批准" : "用户手动禁用"
    if (!reason) return Promise.resolve(null)
    return runAction<void>("entry", "review_entry", { id, status, reason })
  }

  const runEvolution = async () => {
    const result = await runAction<ReflectionResult>("evolution", "run_evolution_now")
    if (result) setNotice({ tone: "success", text: result.message })
  }

  const cancelEvolution = () => runAction<void>("cancel", "cancel_evolution")
  const retryEvolution = async (runId: string) => {
    const result = await runAction<ReflectionResult>("evolution", "retry_evolution", { runId })
    if (result) setNotice({ tone: "success", text: result.message })
  }

  const maintenance = async (label: string, command: string, args?: Record<string, unknown>) => {
    const result = await runAction<MaintenanceResult>(label, command, args)
    if (result) setNotice({ tone: "success", text: result.path ? `${result.message}：${result.path}` : result.message })
  }

  const rollbackRun = async (runId: string) => {
    const result = await runAction<RunRollbackResult>("rollback-run", "rollback_evolution_run", { runId })
    if (result) setNotice({ tone: "success", text: result.message })
    return result
  }

  const activeEntries = snapshot.entries.filter((entry) => entry.status === "active")
  const pendingEntries = snapshot.entries.filter((entry) => entry.status === "pending")
  const title = view === "settings" ? "设置" : nav.find((item) => item.id === view)?.label ?? "进化中枢"

  return <div className="app-shell">
    <aside className={`rail ${menuOpen ? "rail-open" : ""}`}>
      <div className="brand-lockup"><div className="brand-mark"><Fingerprint size={17} strokeWidth={2.4} /></div><div><strong>Recall</strong><span>MEMORY</span></div></div>
      <div className="rail-rule" />
      <nav className="nav-group" aria-label="主导航">
        <span className="nav-caption">WORKSPACE</span>
        {nav.map((item) => { const Icon = item.icon; return <button key={item.id} className={`nav-item ${view === item.id ? "active" : ""}`} onClick={() => { setView(item.id); setMenuOpen(false) }}><Icon size={17} /><span>{item.label}</span>{item.id === "agent" && snapshot.dirtyCount > 0 && <i>{snapshot.dirtyCount}</i>}</button> })}
      </nav>
      <div className="rail-bottom">
        <button className={`nav-item ${view === "settings" ? "active" : ""}`} onClick={() => { setView("settings"); setMenuOpen(false) }}><Settings2 size={17} /><span>设置</span></button>
        <div className="privacy-card"><ShieldCheck size={16} /><div><strong>本地优先</strong><span>原始会话只读</span></div></div>
      </div>
    </aside>
    <main className="workspace">
      <header className="topbar">
        <button className="mobile-menu" onClick={() => setMenuOpen((open) => !open)} aria-label="打开导航"><Menu size={19} /></button>
        <div className="breadcrumbs"><span>WORKSPACE</span><ChevronRight size={14} /><strong>{title}</strong></div>
        <div className="top-actions"><div className={`runtime-pill ${isTauri() ? "online" : "offline"}`}><span />{isTauri() ? phaseLabel[snapshot.run?.phase ?? (snapshot.evolution.runMode === "listener" ? "listening" : "idle")] : "浏览器预览"}</div><button className="icon-button" onClick={() => void loadSnapshot()} title="刷新数据" aria-label="刷新数据"><RefreshCw size={17} className={loading ? "spin" : ""} /></button><button className="avatar-button" title="本地安全存储" aria-label="本地安全存储"><LockKeyhole size={15} /></button></div>
      </header>
      {notice && <div className={`notice ${notice.tone}`} role="status"><span>{notice.tone === "success" ? <CircleCheck size={16} /> : notice.tone === "error" ? <CircleAlert size={16} /> : <Activity size={16} />}{notice.text}</span><button onClick={() => setNotice(null)} aria-label="关闭提示"><X size={14} /></button></div>}
      <div className="page-wrap">
        {view === "overview" && <Overview snapshot={snapshot} active={activeEntries.length} pending={pendingEntries.length} onNavigate={setView} onRun={() => void runEvolution()} onAuthorize={() => void authorize()} onDismissRecovery={() => void runAction("dismiss-recovery", "dismiss_recovery_notice")} busy={busy} />}
        {view === "agent" && <AgentConsole snapshot={snapshot} trace={liveTrace} onRun={() => void runEvolution()} onCancel={() => void cancelEvolution()} onRetry={(runId) => void retryEvolution(runId)} onEntry={updateEntry} onSaveMode={(agentMode) => saveEvolution({ ...snapshot.evolution, agentMode })} busy={busy} />}
        {view === "runs" && <RunHistory snapshot={snapshot} onRetry={(runId) => void retryEvolution(runId)} onRollback={rollbackRun} busy={busy} />}
        {view === "review" && <ReviewCenter entries={snapshot.entries} runs={snapshot.runHistory} onEntry={updateEntry} />}
        {view === "sources" && <Sources snapshot={snapshot} onAuthorize={() => void authorize()} onScan={() => void runAction<ScanResult>("scan", "scan_sessions", { days: snapshot.evolution.lookbackDays })} onSave={saveEvolution} busy={busy} />}
        {view === "repository" && <Repository entries={snapshot.entries} onEntry={updateEntry} onRefresh={() => void loadSnapshot()} />}
        {view === "management" && <Management snapshot={snapshot} busy={busy} onBackup={() => void maintenance("backup", "backup_store")} onRestore={(fileName) => void maintenance("restore", "restore_store_backup", { fileName })} onExport={() => void maintenance("export", "export_redacted_store")} onClear={() => void maintenance("clear", "clear_reflected_activity_cache")} onTestMcp={() => void runAction("test-mcp", "test_mcp")} />}
        {view === "settings" && <SettingsPage snapshot={snapshot} busy={busy} onSaveEvolution={saveEvolution} onSaveModel={saveModel} onTestModel={(input) => runAction("test-model", "test_model_connection", { input })} onInstallMcp={() => runAction("install", "install_mcp")} onTestMcp={() => runAction("test-mcp", "test_mcp")} />}
      </div>
    </main>
  </div>
}

function Overview({ snapshot, active, pending, onNavigate, onRun, onAuthorize, onDismissRecovery, busy }: { snapshot: Snapshot; active: number; pending: number; onNavigate: (view: View) => void; onRun: () => void; onAuthorize: () => void; onDismissRecovery: () => void; busy: string | null }) {
  const mode = snapshot.evolution.runMode === "manual" ? "手动" : snapshot.evolution.runMode === "listener" ? "持续监听" : `每 ${snapshot.evolution.scheduleHours} 小时`
  const configured = modelConfigured(snapshot.config)
  return <div className="page overview-page">
    <section className="command-band">
      <div><p className="eyebrow">EVOLUTION CONTROL</p><h1>内置 Agent 正在管理你的工作记忆</h1><p className="lede">扫描本地 Codex 与 Claude Code 会话，生成可追溯的 Meta 和 Skill，再交给下一轮任务读取。</p></div>
      <div className="hero-actions"><button className="button primary" onClick={onRun} disabled={busy !== null || !configured || !snapshot.consentGranted}><Play size={16} />{busy === "evolution" ? "运行中" : "立即进化"}</button><button className="button subtle" onClick={() => onNavigate("agent")}><Bot size={16} />查看 Agent</button></div>
    </section>
    {snapshot.recoveryNotice && <section className="run-error recovery-alert"><CircleAlert size={17} /><span>{snapshot.recoveryNotice}</span><button className="button outline" onClick={() => onNavigate("management")}>查看备份</button><button className="icon-button" onClick={onDismissRecovery} title="关闭恢复提示" aria-label="关闭恢复提示"><X size={14} /></button></section>}
    {(!snapshot.consentGranted || !configured) && <section className="panel onboarding-panel"><div className="panel-heading"><div><h2>首次使用检查</h2><p>完成数据授权和模型连接后即可运行第一次反思</p></div><span className="count-badge accent">{Number(snapshot.consentGranted) + Number(configured)}/2</span></div><div className="onboarding-steps"><div className={snapshot.consentGranted ? "complete" : ""}><i>{snapshot.consentGranted ? <Check size={14} /> : "1"}</i><div><strong>会话目录</strong><span>{snapshot.consentGranted ? "已授权只读访问" : "检测 Codex 与 Claude Code 本地目录"}</span></div>{!snapshot.consentGranted && <button className="button outline" onClick={onAuthorize} disabled={busy !== null}>授权并检测</button>}</div><div className={configured ? "complete" : ""}><i>{configured ? <Check size={14} /> : "2"}</i><div><strong>模型连接</strong><span>{configured ? `${snapshot.config.model} 已配置` : "连接 Ollama/Qwen3 或 OpenAI-compatible 模型"}</span></div>{!configured && <button className="button outline" onClick={() => onNavigate("settings")}>配置模型</button>}</div></div></section>}
    <section className="metrics-grid">
      <Metric label="待分析活动" value={snapshot.dirtyCount} detail={`回看 ${snapshot.evolution.lookbackDays} 天`} icon={<Activity size={17} />} />
      <Metric label="Active 沉淀" value={active} detail="可供下一轮读取" icon={<Fingerprint size={17} />} />
      <Metric label="待审核" value={pending} detail="修订与高风险候选" icon={<ShieldCheck size={17} />} tone="amber" />
      <Metric label="运行策略" value={mode} detail={snapshot.run ? `上次 ${formatTime(snapshot.run.completedAt ?? snapshot.run.startedAt)}` : "尚未运行"} icon={<Clock3 size={17} />} text />
    </section>
    <section className="overview-columns">
      <div className="panel agent-status-panel"><div className="panel-heading"><div><h2>Evolution Agent</h2><p>受限工具、脱敏输入、本地风险门</p></div><span className={`state-badge ${snapshot.run?.phase === "failed" ? "amber" : configured ? "green" : "amber"}`}>{snapshot.run?.phase ? phaseLabel[snapshot.run.phase] : configured ? "就绪" : "待配置"}</span></div><RunTimeline phase={snapshot.run?.phase} /><div className="agent-summary"><span><b>{snapshot.run?.consumedActivities ?? 0}</b> 本次活动</span><span><b>{snapshot.run?.generated ?? 0}</b> 生成候选</span><span><b>{snapshot.run?.activated ?? 0}</b> 自动启用</span></div></div>
      <div className="panel source-health"><div className="panel-heading"><div><h2>会话来源</h2><p>只读扫描，原始记录不改写</p></div><button className="icon-button" onClick={() => onNavigate("sources")} title="查看数据源"><ChevronRight size={16} /></button></div>{snapshot.sources.map((source) => <ProviderRow source={source} key={source.provider} />)}</div>
    </section>
  </div>
}

function AgentConsole({ snapshot, trace, onRun, onCancel, onRetry, onEntry, onSaveMode, busy }: { snapshot: Snapshot; trace: AgentTraceEvent[]; onRun: () => void; onCancel: () => void; onRetry: (runId: string) => void; onEntry: (id: string, status: "active" | "rejected" | "disabled") => void; onSaveMode: (mode: AgentMode) => Promise<unknown>; busy: string | null }) {
  const pending = snapshot.entries.filter((entry) => entry.status === "pending")
  const runEntries = snapshot.entries.filter((entry) => entry.originRunId === snapshot.run?.runId)
  const configured = modelConfigured(snapshot.config)
  const canRetry = ["failed", "cancelled", "interrupted"].includes(snapshot.run?.phase ?? "")
  const lastTrace = trace[trace.length - 1]
  const verificationLabel = snapshot.run?.verificationStatus === "passed" ? "通过" : snapshot.run?.verificationStatus === "review_required" ? "需复核" : snapshot.run?.verificationStatus === "failed" ? "失败" : "未运行"
  return <div className="page">
    <PageHeader title="Evolution Agent" description="内置 Agent 读取脱敏活动、生成候选，并在验证模式下执行只读证据检查。"><div className="header-actions">{runIsBusy(snapshot.run) ? <button className="button danger" onClick={onCancel} disabled={busy === "cancel"}><X size={15} />{snapshot.run?.phase === "cancelling" ? "正在取消" : "取消运行"}</button> : canRetry && snapshot.run?.runId ? <button className="button outline" onClick={() => onRetry(snapshot.run!.runId)} disabled={busy !== null}><RotateCcw size={15} />重试</button> : null}<button className="button primary" onClick={onRun} disabled={busy !== null || !configured || !snapshot.consentGranted || runIsBusy(snapshot.run)}><Sparkles size={16} className={busy === "evolution" ? "spin" : ""} />{busy === "evolution" ? "Agent 运行中" : "扫描并进化"}</button></div></PageHeader>
    <section className="agent-mode-bar"><div className="segmented">{(["reflection", "verification"] as const).map((mode) => <button key={mode} className={snapshot.evolution.agentMode === mode ? "selected" : ""} disabled={busy !== null || runIsBusy(snapshot.run)} onClick={() => void onSaveMode(mode)}>{mode === "reflection" ? "反思模式" : "验证模式"}</button>)}</div><span className={`model-health ${snapshot.config.healthStatus}`}><i />{snapshot.config.healthStatus === "ok" ? `${snapshot.config.model} 已连接` : snapshot.config.healthStatus === "error" ? snapshot.config.healthError || "模型连接失败" : `${snapshot.config.provider === "remote" ? "远程" : "Ollama"} · ${snapshot.config.model || "待配置"}`}</span></section>
    <section className="agent-live-metrics"><div><span>当前阶段</span><strong>{phaseLabel[snapshot.run?.phase ?? "idle"]}</strong></div><div><span>当前工具</span><strong>{lastTrace?.toolName || "等待工具"}</strong></div><div><span>已读活动</span><strong>{snapshot.run?.scannedActivities ?? 0}</strong></div><div><span>候选</span><strong>{snapshot.run?.generated ?? 0}</strong></div><div><span>验证</span><strong>{verificationLabel}</strong></div></section>
    <div className="agent-console-grid">
      <section className="panel agent-run-panel"><div className="panel-heading"><div><h2>当前运行</h2><p>{snapshot.run?.runId ?? "还没有运行记录"}</p></div><span className={`state-badge ${snapshot.run?.phase === "failed" ? "amber" : "green"}`}>{phaseLabel[snapshot.run?.phase ?? (snapshot.evolution.runMode === "listener" ? "listening" : "idle")]}</span></div><RunTimeline phase={snapshot.run?.phase} />{snapshot.run?.error && <div className="run-error"><CircleAlert size={15} />{snapshot.run.error}</div>}<div className="run-stats"><div><span>扫描</span><strong>{snapshot.run?.scannedActivities ?? 0}</strong></div><div><span>处理</span><strong>{snapshot.run?.consumedActivities ?? 0}</strong></div><div><span>生成</span><strong>{snapshot.run?.generated ?? 0}</strong></div><div><span>启用</span><strong>{snapshot.run?.activated ?? 0}</strong></div></div></section>
      <section className="panel boundary-panel"><div className="panel-heading"><div><h2>Runner 边界</h2><p>{snapshot.evolution.agentMode === "verification" ? "反思工具 + 只读验证工具" : "受限反思工具"}</p></div><LockKeyhole size={18} /></div><div className="boundary-list"><span><Check size={14} />读取 Active Meta 与 Skill</span><span><Check size={14} />读取最多 80 条脱敏活动</span><span><Check size={14} />验证重复项和 Revision 冲突</span><span><Check size={14} />无 Shell 和任意文件权限</span></div></section>
    </div>
    <section className="panel trace-panel"><div className="panel-heading"><div><h2>Agent 实时轨迹</h2><p>仅保存脱敏阶段、工具和结果摘要</p></div><span className="count-badge">{trace.length}</span></div><TraceList trace={trace} emptyTitle="等待 Agent 轨迹" /></section>
    <section className="panel input-panel"><div className="panel-heading"><div><h2>本次实际输入</h2><p>最近一次 Agent 运行读取的脱敏批次，按运行记录固定保存</p></div><span className="count-badge">{snapshot.runActivities.length} 条</span></div>{snapshot.runActivities.length ? <div className="activity-list compact">{snapshot.runActivities.slice(0, 8).map((activity) => <ActivityRow key={activity.id} activity={activity} />)}</div> : <Empty icon={Database} title="还没有运行输入" description="点击扫描并进化后，这里会显示 Agent 真正读取的活动" />}</section>
    <section className="panel output-panel"><div className="panel-heading"><div><h2>本轮产出</h2><p>展示最近一次运行生成的 Meta、Skill 与 Revision</p></div><span className="count-badge">{runEntries.length} 条</span></div>{runEntries.length ? <div className="run-output-list">{runEntries.map((entry) => <RunOutputRow entry={entry} key={entry.id} />)}</div> : <Empty icon={Fingerprint} title="本轮没有可靠候选" description="没有足够证据时，Agent 会完成运行但不生成沉淀" />}</section>
    <section className="panel candidate-panel"><div className="panel-heading"><div><h2>审核队列</h2><p>Revision 和风险候选不会自动进入下一轮上下文</p></div><span className="count-badge accent">{pending.length} 待审核</span></div>{pending.length ? pending.map((entry) => <CandidateRow entry={entry} onEntry={onEntry} key={entry.id} />) : <Empty icon={ShieldCheck} title="审核队列为空" description="低风险新增会自动启用，其余候选会留在这里" />}</section>
  </div>
}

function Sources({ snapshot, onAuthorize, onScan, onSave, busy }: { snapshot: Snapshot; onAuthorize: () => void; onScan: () => void; onSave: (settings: EvolutionSettings) => Promise<unknown>; busy: string | null }) {
  return <div className="page"><PageHeader title="数据源" description="只读发现本机 Codex 和 Claude Code 会话，进入 Store 前先完成脱敏。"><button className="button primary" onClick={onScan} disabled={busy !== null || !snapshot.consentGranted}><RefreshCw size={16} className={busy === "scan" ? "spin" : ""} />检测新活动</button></PageHeader>
    {!snapshot.consentGranted && <div className="consent-banner"><div className="consent-icon"><ShieldCheck size={21} /></div><div><strong>授权读取本地会话</strong><p>仅访问 Codex 和 Claude Code 的会话目录，不修改原文件。</p></div><button className="button primary" onClick={onAuthorize} disabled={busy !== null}><Check size={16} />授权并发现</button></div>}
    <div className="range-toolbar"><div><strong>历史回看范围</strong><span>用于手动和定时扫描</span></div><div className="segmented">{([1, 7, 30] as const).map((days) => <button key={days} className={snapshot.evolution.lookbackDays === days ? "selected" : ""} onClick={() => void onSave({ ...snapshot.evolution, lookbackDays: days })}>{days} 天</button>)}</div></div>
    <div className="provider-grid">{snapshot.sources.length ? snapshot.sources.map((source) => <ProviderCard source={source} enabled={source.provider === "codex" ? snapshot.evolution.codexEnabled : snapshot.evolution.claudeEnabled} key={source.provider} />) : (["codex", "claude-code"] as const).map((provider) => <ProviderCard source={{ provider, root: "等待首次扫描", available: false, sessionCount: 0, activityCount: 0, errorCount: 0, cursorCount: 0 }} enabled key={provider} />)}</div>
    <section className="panel session-panel"><div className="panel-heading"><div><h2>最近会话</h2><p>当前设置为最近 {snapshot.evolution.lookbackDays} 天</p></div><span className="count-badge">{snapshot.sessions.length}</span></div>{snapshot.sessions.length ? <div className="session-table"><div className="table-head"><span>会话</span><span>来源</span><span>更新时间</span><span>活动</span></div>{snapshot.sessions.slice(0, 16).map((session) => <div className="table-row" key={session.id}><div className="session-name"><span className={`provider-dot ${providerAccent(session.provider)}`} />{session.title || "未命名会话"}<small>{session.cwd || session.sourcePath}</small></div><span className="muted">{session.provider === "codex" ? "Codex" : "Claude Code"}</span><span className="muted">{formatTime(session.updatedAt)}</span><span className="mono muted">{session.activityCount}</span></div>)}</div> : <Empty icon={FileText} title="尚未发现会话" description="授权后点击检测新活动" />}</section>
  </div>
}

function RunHistory({ snapshot, onRetry, onRollback, busy }: { snapshot: Snapshot; onRetry: (runId: string) => void; onRollback: (runId: string) => Promise<RunRollbackResult | null>; busy: string | null }) {
  const [selected, setSelected] = useState(snapshot.runHistory[0]?.runId ?? "")
  const [detail, setDetail] = useState<EvolutionRunDetail | null>(null)
  const [detailVersion, setDetailVersion] = useState(0)
  useEffect(() => {
    if (!selected && snapshot.runHistory[0]) setSelected(snapshot.runHistory[0].runId)
  }, [selected, snapshot.runHistory])
  useEffect(() => {
    if (!selected || !isTauri()) return
    let active = true
    void invoke<EvolutionRunDetail>("get_evolution_run_detail", { runId: selected }).then((value) => { if (active) setDetail(value) }).catch(() => { if (active) setDetail(null) })
    return () => { active = false }
  }, [selected, snapshot.run?.phase, detailVersion])
  const rollback = async () => {
    if (!detail || !window.confirm(`确认回滚本次运行产生的 ${detail.entries.length} 条候选？已批准 Revision 对 Active Store 的修改也会恢复，原始会话不会改变。`)) return
    if (await onRollback(detail.run.runId)) setDetailVersion((value) => value + 1)
  }
  const canRetry = detail && detail.run.runId === snapshot.run?.runId && ["failed", "cancelled", "interrupted"].includes(detail.run.phase)
  return <div className="page"><PageHeader title="运行历史" description="查看每次反思使用的脱敏活动、候选输出、状态与错误。" />
    <div className="history-layout"><section className="panel run-list-panel"><div className="panel-heading"><div><h2>最近运行</h2><p>失败、取消和中断的任务不会消费活动</p></div><span className="count-badge">{snapshot.runHistory.length}</span></div><div className="run-list">{snapshot.runHistory.length ? snapshot.runHistory.map((run) => <button key={run.runId} className={selected === run.runId ? "selected" : ""} onClick={() => setSelected(run.runId)}><span className={`status-dot ${run.phase}`} /><div><strong>{formatTime(run.startedAt)}</strong><small>{run.mode === "manual" ? "手动" : run.mode === "listener" ? "监听" : "定时"} · {formatDuration(run.startedAt, run.completedAt)} · {run.runId.slice(-8)}</small></div><span className={`state-badge ${run.rolledBackAt ? "muted-badge" : run.phase === "completed" ? "green" : run.phase === "failed" ? "amber" : "muted-badge"}`}>{run.rolledBackAt ? "已回滚" : phaseLabel[run.phase]}</span></button>) : <Empty icon={History} title="还没有运行记录" description="完成一次进化后会在这里出现" />}</div></section>
      <section className="panel run-detail-panel">{detail ? <><div className="panel-heading"><div><h2>运行详情</h2><p>{detail.run.runId}</p></div><div className="header-actions">{canRetry && <button className="button outline" onClick={() => onRetry(detail.run.runId)} disabled={busy !== null}><RotateCcw size={14} />安全重试</button>}{detail.run.phase === "completed" && detail.entries.length > 0 && !detail.run.rolledBackAt && <button className="button danger" onClick={() => void rollback()} disabled={busy !== null}><RotateCcw size={14} />回滚本次运行</button>}</div></div>{detail.run.rolledBackAt && <div className="run-note"><RotateCcw size={15} />本次运行已于 {formatTime(detail.run.rolledBackAt)} 回滚，版本和审计记录仍保留。</div>}{detail.run.error && <div className="run-error"><CircleAlert size={15} />{detail.run.error}</div>}<div className="run-context-strip"><span><b>模型</b>{detail.run.model || "未记录"}</span><span><b>Agent 模式</b>{detail.run.agentMode === "verification" ? "验证" : "反思"}</span><span><b>来源</b>{detail.run.providers.length ? detail.run.providers.map((value) => value === "codex" ? "Codex" : "Claude Code").join(" + ") : "未记录"}</span><span><b>范围</b>{detail.run.lookbackDays || 30} 天</span><span><b>耗时</b>{formatDuration(detail.run.startedAt, detail.run.completedAt)}</span></div><div className="run-detail-stats"><span><b>{detail.run.scannedActivities}</b>扫描</span><span><b>{detail.run.consumedActivities}</b>消费</span><span><b>{detail.run.generated}</b>候选</span><span><b>{detail.run.activated}</b>启用</span></div>{detail.run.agentMode === "verification" && <div className={`verification-result ${detail.run.verificationStatus}`}><ShieldCheck size={15} /><div><strong>{detail.run.verificationStatus === "passed" ? "验证通过" : detail.run.verificationStatus === "review_required" ? "需要人工复核" : detail.run.verificationStatus === "failed" ? "验证失败" : "等待验证"}</strong><span>{detail.run.verificationSummary || "只读验证尚未生成摘要"}</span></div></div>}<div className="detail-section"><strong>Agent 轨迹</strong><span>{detail.traces.length} 条脱敏摘要</span></div><TraceList trace={detail.traces} emptyTitle="本轮没有轨迹" /><div className="detail-section"><strong>脱敏输入</strong><span>{detail.activities.length} 条固定活动批次</span></div>{detail.activities.slice(0, 6).map((activity) => <ActivityRow key={activity.id} activity={activity} />)}<div className="detail-section"><strong>本轮产出</strong><span>{detail.entries.length} 条</span></div>{detail.entries.map((entry) => <RunOutputRow entry={entry} activities={detail.activities} key={entry.id} />)}{!detail.activities.length && !detail.entries.length && <Empty icon={Database} title="本轮没有输入或产出" description="空数据运行会正常结束，不会写入候选" />}</> : <Empty icon={History} title="选择一条运行" description="右侧会展示固定输入、输出和错误信息" />}</section>
    </div>
    {detail && <RunUsageStats run={detail.run} />}
  </div>
}

function RunUsageStats({ run }: { run: EvolutionRunState }) { return <section className="panel usage-panel"><div className="panel-heading"><div><h2>模型使用统计</h2><p>只显示数量、耗时和 Provider，不保存 Prompt 或原始输出</p></div><Cpu size={17} /></div><div className="store-stat-grid"><span><b>{run.inputActivityCount}</b>输入活动</span><span><b>{run.inputTokens}</b>估算输入 Token</span><span><b>{run.outputTokens}</b>估算输出 Token</span><span><b>{run.durationMs} ms</b>模型耗时</span><span><b>{run.providerUsed || "未记录"}</b>实际 Provider</span><span><b>{run.fallbackCount}</b>Fallback</span><span><b>{run.estimatedCostUsd == null ? "未配置" : `$${run.estimatedCostUsd.toFixed(6)}`}</b>估算费用</span></div></section> }

function ReviewCenter({ entries, runs, onEntry }: { entries: EvolutionEntry[]; runs: EvolutionRunState[]; onEntry: (id: string, status: "active" | "rejected" | "disabled") => void }) {
  const [kind, setKind] = useState<"all" | "meta" | "skill" | "revision">("all")
  const [risk, setRisk] = useState<"all" | "low" | "high" | "review">("all")
  const [runId, setRunId] = useState("all")
  const [selected, setSelected] = useState<EvolutionEntry | null>(null)
  const [detail, setDetail] = useState<EvolutionRunDetail | null>(null)
  useEffect(() => {
    if (!selected?.originRunId || !isTauri()) {
      setDetail(null)
      return
    }
    let active = true
    void invoke<EvolutionRunDetail>("get_evolution_run_detail", { runId: selected.originRunId })
      .then((value) => { if (active) setDetail(value) })
      .catch(() => { if (active) setDetail(null) })
    return () => { active = false }
  }, [selected])
  const pending = entries.filter((entry) => entry.status === "pending" && (kind === "all" || entry.kind === kind) && (risk === "all" || entry.risk === risk) && (runId === "all" || entry.originRunId === runId))
  return <div className="page"><PageHeader title="审核中心" description="Revision、高风险与证据不足的候选必须由用户决定是否进入 Active Store。" />
    <div className="review-toolbar"><label>类型<select value={kind} onChange={(event) => setKind(event.target.value as typeof kind)}><option value="all">全部</option><option value="meta">Meta</option><option value="skill">Skill</option><option value="revision">Revision</option></select></label><label>风险<select value={risk} onChange={(event) => setRisk(event.target.value as typeof risk)}><option value="all">全部</option><option value="low">低风险</option><option value="high">高风险</option><option value="review">需复核</option></select></label><label>运行<select value={runId} onChange={(event) => setRunId(event.target.value)}><option value="all">全部运行</option>{runs.map((run) => <option value={run.runId} key={run.runId}>{formatTime(run.startedAt)} · {run.runId.slice(-8)}</option>)}</select></label><span className="count-badge accent">{pending.length} 待审核</span></div>
    <section className="panel candidate-panel">{pending.length ? pending.map((entry) => <CandidateRow entry={entry} onEntry={onEntry} onInspect={() => setSelected(entry)} key={entry.id} />) : <Empty icon={ShieldCheck} title="没有符合条件的候选" description="审核结果会保留在版本历史和审计记录中" />}</section>
    {selected && <div className="evidence-drawer"><div className="history-drawer-head"><div><span>{selected.kind.toUpperCase()}</span><h2>{selected.title}</h2></div><button className="icon-button" onClick={() => setSelected(null)} title="关闭证据详情"><X size={16} /></button></div><p>{selected.summary}</p>{detail ? <><CandidateVerificationSummary verification={detail.candidateVerifications.find((item) => item.entryId === selected.id)} /><div className="detail-section"><strong>关联脱敏证据</strong><span>{selected.sourceRefs.length} 条引用</span></div>{detail.activities.filter((activity) => selected.sourceRefs.includes(activity.id)).map((activity) => <div className="evidence-item" key={activity.id}><span>{activity.provider === "codex" ? "Codex" : "Claude Code"} · {formatTime(activity.occurredAt)}</span><p>{activity.text}</p></div>)}</> : <Empty icon={FileText} title="正在加载证据" description="从原运行记录读取脱敏内容" />}</div>}
  </div>
}

function CandidateVerificationSummary({ verification }: { verification?: CandidateVerification }) {
  if (!verification) return <div className="candidate-verification"><span>未提供候选级验证结果</span><span>该候选仍由本地风险门和人工审核决定。</span></div>
  const recommendation = verification.recommendation === "approve" ? "建议批准" : verification.recommendation === "reject" ? "建议拒绝" : "建议复核"
  return <div className="candidate-verification"><div className="candidate-verification-head"><strong>{recommendation}</strong><span>{Math.round(verification.confidence * 100)}% 置信度</span></div><span>证据{verification.evidenceSufficient ? "充分" : "不足"} · {verification.duplicate ? "检测到重复" : "未发现重复"} · {verification.conflict ? "存在冲突" : "未发现冲突"}</span><span>{verification.rationale || "未提供验证理由"}</span>{verification.supportingEvidence.length > 0 && <small>支持证据：{verification.supportingEvidence.join("、")}</small>}{verification.contradictingEvidence.length > 0 && <small>反对证据：{verification.contradictingEvidence.join("、")}</small>}</div>
}

function Repository({ entries, onEntry, onRefresh }: { entries: EvolutionEntry[]; onEntry: (id: string, status: "active" | "rejected" | "disabled") => void; onRefresh: () => void }) {
  const [filter, setFilter] = useState<"all" | "active" | "pending">("all")
  const [selected, setSelected] = useState<EvolutionEntry | null>(null)
  const [versions, setVersions] = useState<EntryVersion[]>([])
  const [diff, setDiff] = useState<EntryVersionDiff | null>(null)
  const [historyError, setHistoryError] = useState("")
  const visible = useMemo(() => entries.filter((entry) => filter === "all" || entry.status === filter), [entries, filter])
  const openHistory = async (entry: EvolutionEntry) => {
    setSelected(entry); setDiff(null); setHistoryError("")
    try { setVersions(await invoke<EntryVersion[]>("list_entry_versions", { entryId: entry.id })) } catch (error) { setHistoryError(errorText(error)) }
  }
  const compare = async (version: EntryVersion) => {
    if (!selected) return
    try { setDiff(await invoke<EntryVersionDiff>("get_entry_version_diff", { entryId: selected.id, fromVersion: version.version, toVersion: selected.version })) } catch (error) { setHistoryError(errorText(error)) }
  }
  const rollback = async (version: number) => {
    if (!selected || !window.confirm(`确认将“${selected.title}”回滚到 v${version}？原始会话不会被修改。`)) return
    try { await invoke("rollback_entry", { entryId: selected.id, version }); await openHistory({ ...selected, version: selected.version + 1 }); onRefresh() } catch (error) { setHistoryError(errorText(error)) }
  }
  return <div className="page"><PageHeader title="沉淀仓库" description="保存已激活和待审核的 Meta、Skill 与 Revision，每条内容都保留来源。" /><div className="repository-toolbar"><div className="segmented">{(["all", "active", "pending"] as const).map((item) => <button key={item} className={filter === item ? "selected" : ""} onClick={() => setFilter(item)}>{item === "all" ? "全部" : item === "active" ? "Active" : "待审核"}<span>{item === "all" ? entries.length : entries.filter((entry) => entry.status === item).length}</span></button>)}</div><span className="muted"><Database size={14} />SQLite 本地存储</span></div><div className="repo-grid">{visible.length ? visible.map((entry) => <EntryCard entry={entry} onEntry={onEntry} onHistory={() => void openHistory(entry)} key={entry.id} />) : <div className="panel full-empty"><Empty icon={BookOpen} title="仓库还没有条目" description="运行 Evolution Agent 后，结果会在这里归档" /></div>}</div>{selected && <div className="history-drawer"><div className="history-drawer-head"><div><span>{selected.kind.toUpperCase()}</span><h2>{selected.title}</h2></div><button className="icon-button" onClick={() => setSelected(null)} title="关闭版本历史"><X size={16} /></button></div>{historyError && <div className="run-error">{historyError}</div>}<div className="version-list">{versions.map((version) => <button key={version.id} className={version.version === selected.version ? "current" : ""} onClick={() => void compare(version)}><div><strong>v{version.version}</strong><span>{version.action} · {formatTime(version.createdAt)}</span>{version.reviewer && <small>{version.reviewer} · {version.reviewReason || "已审核"}</small>}</div><span className="state-badge muted-badge">{version.status}</span></button>)}</div>{diff && <div className="version-diff"><div><strong>v{diff.fromVersion ?? 0}</strong><p>{diff.oldBody || "空"}</p></div><ChevronRight size={18} /><div><strong>v{diff.toVersion}</strong><p>{diff.newBody || "空"}</p></div>{diff.fromVersion !== selected.version && <button className="button outline" onClick={() => void rollback(diff.fromVersion!)}><RotateCcw size={14} />回滚到此版本</button>}</div>}</div>}</div>
}

function Management({ snapshot, busy, onBackup, onRestore, onExport, onClear, onTestMcp }: { snapshot: Snapshot; busy: string | null; onBackup: () => void; onRestore: (fileName: string) => void; onExport: () => void; onClear: () => void; onTestMcp: () => void }) {
  const stats = snapshot.storeStats
  const cleanup = snapshot.cacheCleanupPreview
  const redaction = snapshot.redactionReport
  const latestBackup = snapshot.backups[0]
  const clear = () => {
    if (window.confirm(`确认清理 ${cleanup.reflectedActivities} 条已消费活动？这会移除 ${cleanup.runActivityLinks} 条运行输入关联，影响 ${cleanup.affectedRuns} 条运行的输入明细；${cleanup.preservedEntries} 条沉淀和 ${cleanup.preservedVersions} 个版本会完整保留。`)) onClear()
  }
  const restore = () => {
    if (latestBackup && window.confirm(`确认从 ${latestBackup.fileName} 恢复 Active Store？当前沉淀和版本指针会被备份内容替换；活动、运行记录和原始会话不会改变。`)) onRestore(latestBackup.fileName)
  }
  const uninstallMcp = async () => {
    if (!window.confirm("确认卸载 Recall MCP？优先恢复安装前备份；没有备份时只移除 Recall 配置项。")) return
    try { await invoke("uninstall_mcp"); window.alert("Recall MCP 已卸载") } catch (error) { window.alert(errorText(error)) }
  }
  const testMcpTarget = async (target: "codex" | "claude-code") => {
    try { await invoke("test_mcp_target", { target }); window.alert(`${target === "codex" ? "Codex" : "Claude Code"} MCP 连通`) } catch (error) { window.alert(errorText(error)) }
  }
  return <div className="page"><PageHeader title="数据管理" description="检查来源健康度、Store 占用、MCP 状态与最近审计事件。" />
    <div className="management-grid"><section className="panel"><div className="panel-heading"><div><h2>本地 Store</h2><p>{stats.databasePath || "等待桌面运行时"}</p></div><HardDrive size={18} /></div><div className="store-stat-grid"><span><b>{stats.activeCount}</b>Active</span><span><b>{stats.versionCount}</b>版本</span><span><b>{stats.runCount}</b>运行</span><span><b>{(stats.databaseBytes / 1024 / 1024).toFixed(1)} MB</b>占用</span></div><div className="cleanup-impact"><Database size={14} /><span>{latestBackup ? `最近备份 ${formatTime(latestBackup.createdAt)}；` : "尚无备份；"}清理会移除 {cleanup.runActivityLinks} 条运行输入关联，Active 和版本历史不受影响</span></div><div className="maintenance-actions"><button className="button outline" onClick={onBackup} disabled={busy !== null}><HardDrive size={14} />备份</button><button className="button outline" onClick={restore} disabled={busy !== null || !latestBackup}><RotateCcw size={14} />恢复最近备份</button><button className="button outline" onClick={onExport} disabled={busy !== null}><Download size={14} />脱敏导出</button><button className="button danger" onClick={clear} disabled={busy !== null || cleanup.reflectedActivities === 0}><Trash2 size={14} />清理缓存</button></div></section><section className="panel"><div className="panel-heading"><div><h2>MCP 分发</h2><p>只读取 Active Meta 与 Skill</p></div><Network size={18} /></div><div className="connection-list"><ConnectionItem label="Codex" path="~/.codex/config.toml" connected={snapshot.mcp.codex} icon={<TerminalSquare size={17} />} /><ConnectionItem label="Claude Code" path="~/.claude.json" connected={snapshot.mcp.claude} icon={<Network size={17} />} /></div><div className={`model-health-card ${snapshot.mcp.healthStatus ?? "unknown"}`}><span><i />{snapshot.mcp.healthStatus === "ok" ? "最近测试正常" : snapshot.mcp.healthStatus === "error" ? "最近测试失败" : "尚未执行真实测试"}</span><small>{snapshot.mcp.healthError || (snapshot.mcp.lastChecked ? `最近检查 ${formatTime(snapshot.mcp.lastChecked)}` : "配置存在不等于 sidecar 可连接")}</small></div><button className="button outline full-button" onClick={onTestMcp} disabled={busy !== null}>测试 MCP 连通性</button></section></div>
    <section className="panel redaction-panel"><div className="panel-heading"><div><h2>脱敏报告</h2><p>统计进入本地活动 Store 后仍可见的脱敏标记，不读取原始会话正文</p></div><ShieldCheck size={18} /></div><div className="redaction-summary"><span><b>{formatCount(redaction.processedRecords)}</b>处理记录</span><span><b>{formatCount(redaction.redactedRecords)}</b>含敏感片段</span><span><b>{formatCount(redaction.redactionCount)}</b>替换次数</span></div><div className="redaction-categories">{redaction.categories.length ? redaction.categories.map((item) => <span key={item.category}>{item.category}<b>{formatCount(item.count)}</b></span>) : <span>当前缓存未检测到敏感字段标记</span>}</div></section>
    <section className="panel source-management"><div className="panel-heading"><div><h2>来源健康</h2><p>路径、最近扫描、错误数和文件游标</p></div></div>{snapshot.sources.map((source) => <div className="source-health-row" key={source.provider}><span className={`provider-dot ${providerAccent(source.provider)}`} /><div><strong>{source.provider === "codex" ? "Codex" : "Claude Code"}</strong><small>{source.root}</small></div><span>{formatCount(source.sessionCount)} 会话 · {source.cursorCount} 游标<br />{formatTime(source.lastScannedAt)}</span><span className={`state-badge ${source.available && source.errorCount === 0 ? "green" : "amber"}`}>{!source.available ? "目录不存在" : source.errorCount > 0 ? `${source.errorCount} 个错误` : "正常"}</span></div>)}</section>
    <section className="panel audit-panel"><div className="panel-heading"><div><h2>最近审计</h2><p>启用、拒绝、回滚、导出和缓存维护均会记录</p></div><span className="count-badge">{stats.auditCount}</span></div>{snapshot.auditEvents.slice(0, 20).map((event) => <div className="audit-row" key={event.id}><span>{event.action}</span><code>{event.objectId || "store"}</code><time>{formatTime(event.occurredAt)}</time></div>)}</section>
    <section className="panel mcp-calls-panel"><div className="panel-heading"><div><h2>最近 MCP 调用</h2><p>只保存工具名、动作和结果，不保存会话正文</p></div><span className="count-badge">{snapshot.mcp.recentCalls.length}</span></div>{snapshot.mcp.recentCalls.length ? snapshot.mcp.recentCalls.map((call) => <div className="audit-row" key={call.id}><span>{call.toolName}{call.action ? ` · ${call.action}` : ""}</span><code>{call.resultStatus}</code><time>{formatTime(call.occurredAt)}</time></div>) : <Empty icon={Network} title="尚无 MCP 调用" description="下一轮 Agent 或外部客户端调用只读 MCP 后会显示摘要" />}<div className="maintenance-actions"><button className="button outline" onClick={() => void testMcpTarget("codex")} disabled={busy !== null}>测试 Codex</button><button className="button outline" onClick={() => void testMcpTarget("claude-code")} disabled={busy !== null}>测试 Claude Code</button><button className="button danger" onClick={() => void uninstallMcp()} disabled={busy !== null}>卸载并恢复 MCP 配置</button></div></section>
  </div>
}

function SettingsPage({ snapshot, busy, onSaveEvolution, onSaveModel, onTestModel, onInstallMcp, onTestMcp }: { snapshot: Snapshot; busy: string | null; onSaveEvolution: (settings: EvolutionSettings) => Promise<unknown>; onSaveModel: (input: ReflectionConfigInput) => Promise<unknown>; onTestModel: (input: ReflectionConfigInput) => Promise<unknown>; onInstallMcp: () => Promise<unknown>; onTestMcp: () => Promise<unknown> }) {
  const [tab, setTab] = useState<SettingsTab>("agent")
  const [settings, setSettings] = useState(snapshot.evolution)
  const [baseUrl, setBaseUrl] = useState(snapshot.config.baseUrl || "https://api.openai.com")
  const [model, setModel] = useState(snapshot.config.model)
  const [apiKey, setApiKey] = useState("")
  const [contextMode, setContextMode] = useState<ContextMode>(snapshot.config.contextMode)
  const [provider, setProvider] = useState<ModelProvider>(snapshot.config.provider)
  const [timeoutSeconds, setTimeoutSeconds] = useState(snapshot.config.timeoutSeconds)
  const [fallbackEnabled, setFallbackEnabled] = useState(snapshot.config.fallbackEnabled)
  const [fallbackBaseUrl, setFallbackBaseUrl] = useState(snapshot.config.fallbackBaseUrl)
  const [fallbackModel, setFallbackModel] = useState(snapshot.config.fallbackModel)
  const [fallbackTimeoutSeconds, setFallbackTimeoutSeconds] = useState(snapshot.config.fallbackTimeoutSeconds)
  const [inputPricePerMillionUsd, setInputPricePerMillionUsd] = useState(snapshot.config.inputPricePerMillionUsd)
  const [outputPricePerMillionUsd, setOutputPricePerMillionUsd] = useState(snapshot.config.outputPricePerMillionUsd)
  useEffect(() => { setSettings(snapshot.evolution) }, [snapshot.evolution])
  useEffect(() => { setBaseUrl(snapshot.config.baseUrl || "https://api.openai.com/v1"); setModel(snapshot.config.model); setContextMode(snapshot.config.contextMode); setProvider(snapshot.config.provider); setTimeoutSeconds(snapshot.config.timeoutSeconds); setFallbackEnabled(snapshot.config.fallbackEnabled); setFallbackBaseUrl(snapshot.config.fallbackBaseUrl); setFallbackModel(snapshot.config.fallbackModel); setFallbackTimeoutSeconds(snapshot.config.fallbackTimeoutSeconds); setInputPricePerMillionUsd(snapshot.config.inputPricePerMillionUsd); setOutputPricePerMillionUsd(snapshot.config.outputPricePerMillionUsd) }, [snapshot.config])
  const tabs = [
    { id: "agent" as const, label: "进化 Agent", icon: Bot },
    { id: "sources" as const, label: "数据源", icon: FolderOpen },
    { id: "automation" as const, label: "监听与调度", icon: TimerReset },
    { id: "safety" as const, label: "安全与审核", icon: ShieldCheck },
    { id: "connection" as const, label: "MCP 与存储", icon: Network },
  ]
  const saveSettings = () => onSaveEvolution(settings)
  const modelInput: ReflectionConfigInput = { provider, baseUrl, model, apiKey: apiKey || undefined, contextMode, timeoutSeconds, fallbackEnabled, fallbackBaseUrl, fallbackModel, fallbackTimeoutSeconds, inputPricePerMillionUsd, outputPricePerMillionUsd }
  const saveModel = async () => { const result = await onSaveModel({ ...modelInput, apiKey }); if (result) setApiKey("") }
  return <div className="page settings-page"><PageHeader title="设置" description="集中管理 Evolution Agent、数据范围、自动运行、安全策略和 MCP 分发。" />
    <div className="settings-shell"><nav className="settings-tabs" aria-label="设置分类">{tabs.map((item) => { const Icon = item.icon; return <button key={item.id} className={tab === item.id ? "active" : ""} onClick={() => setTab(item.id)}><Icon size={16} />{item.label}</button> })}</nav><div className="settings-content">
      {tab === "agent" && <SettingsSection title="备用 Ollama" description="仅在远程模型网络失败或超时时启用，不会在鉴权和模型配置错误时接管。"><SettingRow title="启用备用模型" description="本地备用配置会随每次运行冻结"><Toggle checked={fallbackEnabled} onChange={setFallbackEnabled} /></SettingRow>{fallbackEnabled && <div className="form-grid"><label>备用 Base URL<input value={fallbackBaseUrl} onChange={(event) => setFallbackBaseUrl(event.target.value)} /></label><label>备用 Model ID<input value={fallbackModel} onChange={(event) => setFallbackModel(event.target.value)} /></label><label>备用超时（秒）<input type="number" min={10} max={300} value={fallbackTimeoutSeconds} onChange={(event) => setFallbackTimeoutSeconds(Math.min(300, Math.max(10, Number(event.target.value) || 10)))} /></label></div>}</SettingsSection>}
      {tab === "agent" && <SettingsSection title="进化 Agent" description="使用 OpenAI-compatible 模型运行受限 Rig Agent。"><SettingRow title="启用 Agent" description="关闭后手动、监听和定时运行都会停止"><Toggle checked={settings.enabled} onChange={(enabled) => setSettings({ ...settings, enabled })} /></SettingRow><div className="setting-block"><div><strong>默认 Agent 模式</strong><p>验证模式会在候选生成后执行只读证据与冲突检查</p></div><div className="segmented">{(["reflection", "verification"] as const).map((mode) => <button key={mode} className={settings.agentMode === mode ? "selected" : ""} onClick={() => setSettings({ ...settings, agentMode: mode })}>{mode === "reflection" ? "反思" : "验证"}</button>)}</div></div><div className="setting-block"><div><strong>模型 Provider</strong><p>远程优先，Ollama 作为本地备用</p></div><div className="segmented">{(["remote", "ollama"] as const).map((value) => <button key={value} className={provider === value ? "selected" : ""} onClick={() => setProvider(value)}>{value === "remote" ? "远程" : "Ollama"}</button>)}</div></div><div className="form-grid"><label>Base URL<input value={baseUrl} onChange={(event) => setBaseUrl(event.target.value)} /></label><label>Model ID<input value={model} onChange={(event) => setModel(event.target.value)} placeholder="例如 qwen3:8b" /></label><label>超时（秒）<input type="number" min={10} max={300} value={timeoutSeconds} onChange={(event) => setTimeoutSeconds(Math.min(300, Math.max(10, Number(event.target.value) || 10)))} /></label><label>API Key<input value={apiKey} onChange={(event) => setApiKey(event.target.value)} type="password" placeholder={snapshot.config.hasApiKey ? "已保存在 macOS Keychain" : provider === "ollama" ? "本地模型可留空" : "输入 API Key"} /></label><label>输入价格（USD / 百万 Token）<input type="number" min={0} step="0.01" value={inputPricePerMillionUsd} onChange={(event) => setInputPricePerMillionUsd(Math.max(0, Number(event.target.value) || 0))} /></label><label>输出价格（USD / 百万 Token）<input type="number" min={0} step="0.01" value={outputPricePerMillionUsd} onChange={(event) => setOutputPricePerMillionUsd(Math.max(0, Number(event.target.value) || 0))} /></label></div><div className="model-preset"><button className="text-button" onClick={() => { setProvider("remote"); if (isLocalModelUrl(baseUrl)) setBaseUrl("https://api.openai.com/v1") }}><Network size={14} />远程 OpenAI-compatible</button><button className="text-button" onClick={() => { setProvider("ollama"); setBaseUrl("http://127.0.0.1:11434/v1"); setModel("qwen3:8b"); setApiKey("") }}><Cpu size={14} />本地 Ollama / Qwen3</button></div><div className={`model-health-card ${snapshot.config.healthStatus}`}><span><i />{snapshot.config.healthStatus === "ok" ? "连接正常" : snapshot.config.healthStatus === "error" ? "连接异常" : "尚未测试"}</span><small>{snapshot.config.healthError || (snapshot.config.lastCheckedAt ? `最近检查 ${formatTime(snapshot.config.lastCheckedAt)}` : "连接测试会使用上方尚未保存的表单内容")}</small></div><div className="settings-actions"><span><KeyRound size={14} />密钥不会写入 SQLite</span><button className="button outline" onClick={() => void onTestModel(modelInput)} disabled={busy !== null || !baseUrl.trim() || !model.trim() || (provider === "remote" && !apiKey && !snapshot.config.hasApiKey)}>测试连接</button><button className="button primary" onClick={() => void saveModel()} disabled={busy !== null || !baseUrl.trim() || !model.trim()}>保存模型</button></div><SaveSettings busy={busy} onClick={() => void saveSettings()} /></SettingsSection>}
      {tab === "sources" && <SettingsSection title="数据源" description="选择 Agent 可以读取的本地会话来源和历史范围。"><SettingRow title="Codex" description="根目录下需要包含 sessions 或 archived_sessions"><Toggle checked={settings.codexEnabled} onChange={(codexEnabled) => setSettings({ ...settings, codexEnabled })} /></SettingRow><label className="source-path-field">Codex 根目录<input value={settings.codexSourcePath} onChange={(event) => setSettings({ ...settings, codexSourcePath: event.target.value })} placeholder="~/.codex" /></label><SettingRow title="Claude Code" description="根目录下需要包含 projects"><Toggle checked={settings.claudeEnabled} onChange={(claudeEnabled) => setSettings({ ...settings, claudeEnabled })} /></SettingRow><label className="source-path-field">Claude Code 根目录<input value={settings.claudeSourcePath} onChange={(event) => setSettings({ ...settings, claudeSourcePath: event.target.value })} placeholder="~/.claude" /></label><div className="setting-block"><div><strong>历史回看范围</strong><p>用于手动和定时扫描</p></div><div className="segmented">{([1, 7, 30] as const).map((days) => <button key={days} className={settings.lookbackDays === days ? "selected" : ""} onClick={() => setSettings({ ...settings, lookbackDays: days })}>{days} 天</button>)}</div></div><SaveSettings busy={busy} onClick={() => void saveSettings()} /></SettingsSection>}
      {tab === "automation" && <SettingsSection title="监听与调度" description="手动按钮始终可用，后台模式只在 Recall 运行时生效。"><div className="mode-options">{(["manual", "listener", "scheduled"] as const).map((mode) => <button key={mode} className={settings.runMode === mode ? "selected" : ""} onClick={() => setSettings({ ...settings, runMode: mode, listenSince: mode === "listener" ? Math.floor(Date.now() / 1000) : settings.listenSince })}><span>{mode === "manual" ? <Play size={17} /> : mode === "listener" ? <Activity size={17} /> : <Clock3 size={17} />}</span><strong>{mode === "manual" ? "手动" : mode === "listener" ? "持续监听" : "定时运行"}</strong><small>{mode === "manual" ? "只在点击时运行" : mode === "listener" ? "只处理开启后的新活动" : "按间隔扫描并进化"}</small></button>)}</div>{settings.runMode === "scheduled" && <div className="setting-block"><div><strong>运行间隔</strong><p>允许 1 到 24 小时</p></div><div className="schedule-control"><button className={settings.scheduleHours === 6 ? "selected" : ""} onClick={() => setSettings({ ...settings, scheduleHours: 6 })}>6 小时</button><button className={settings.scheduleHours === 12 ? "selected" : ""} onClick={() => setSettings({ ...settings, scheduleHours: 12 })}>12 小时</button><input type="number" min={1} max={24} value={settings.scheduleHours} onChange={(event) => setSettings({ ...settings, scheduleHours: Math.min(24, Math.max(1, Number(event.target.value))) })} aria-label="自定义运行间隔" /></div></div>}{settings.runMode === "listener" && <div className="listen-note"><Activity size={15} /><span>监听起点：{formatTime(settings.listenSince)}。此前历史不会进入监听队列。</span></div>}<SettingRow title="登录时启动" description="macOS 登录后自动启动 Recall 并恢复后台调度"><Toggle checked={settings.launchAtLogin} onChange={(launchAtLogin) => setSettings({ ...settings, launchAtLogin })} /></SettingRow><SettingRow title="系统通知" description="反思完成、需要审核或运行失败时通知"><Toggle checked={settings.notificationsEnabled} onChange={(notificationsEnabled) => setSettings({ ...settings, notificationsEnabled })} /></SettingRow><SaveSettings busy={busy} onClick={() => void saveSettings()} /></SettingsSection>}
      {tab === "safety" && <SettingsSection title="安全与审核" description="模型只生成候选，本地规则决定是否进入 Active Store。"><SettingRow title="低风险自动启用" description="新增 Meta 或 Skill 有两个以上来源时可自动启用"><Toggle checked={settings.autoActivateLowRisk} onChange={(autoActivateLowRisk) => setSettings({ ...settings, autoActivateLowRisk })} /></SettingRow><div className="setting-block"><div><strong>最大 Agent steps</strong><p>限制单次模型与工具循环</p></div><div className="stepper"><button onClick={() => setSettings({ ...settings, maxAgentSteps: Math.max(2, settings.maxAgentSteps - 1) })}>−</button><span>{settings.maxAgentSteps}</span><button onClick={() => setSettings({ ...settings, maxAgentSteps: Math.min(8, settings.maxAgentSteps + 1) })}>+</button></div></div><div className="security-facts"><span><Check size={14} />会话正文进入模型前脱敏</span><span><Check size={14} />Revision 永远需要审核</span><span><Check size={14} />无 Shell、任意文件和 MCP 写权限</span></div><SaveSettings busy={busy} onClick={() => void saveSettings()} /></SettingsSection>}
      {tab === "connection" && <SettingsSection title="MCP 与存储" description="MCP 只向下一轮 Agent 分发已批准的 Meta 与 Skill。"><div className="connection-list"><ConnectionItem label="Codex" path="~/.codex/config.toml" connected={snapshot.mcp.codex} icon={<TerminalSquare size={17} />} /><ConnectionItem label="Claude Code" path="~/.claude.json" connected={snapshot.mcp.claude} icon={<Network size={17} />} /></div><div className="setting-block"><div><strong>上下文模式</strong><p>Guided 会建议任务开始时读取，MCP 模式按需调用</p></div><div className="segmented">{(["guided", "mcp"] as const).map((mode) => <button key={mode} className={contextMode === mode ? "selected" : ""} onClick={() => setContextMode(mode)}>{mode === "guided" ? "Guided" : "MCP"}</button>)}</div></div><div className="storage-strip"><Database size={18} /><div><strong>本地 SQLite</strong><span>{formatCount(snapshot.activityCount)} 条活动，{snapshot.entries.length} 条沉淀</span></div></div><div className="settings-actions"><button className="button outline" onClick={() => void onTestMcp()} disabled={busy !== null}>测试 MCP</button><button className="button outline" onClick={() => void saveModel()} disabled={busy !== null || !baseUrl || !model}>保存模式</button><button className="button primary" onClick={() => void onInstallMcp()} disabled={busy !== null}><Network size={15} />安装 MCP</button></div></SettingsSection>}
    </div></div>
  </div>
}

function PageHeader({ title, description, children }: { title: string; description: string; children?: React.ReactNode }) { return <section className="page-intro"><div><h1>{title}</h1><p className="lede">{description}</p></div><div className="page-intro-action">{children}</div></section> }
function Metric({ label, value, detail, icon, tone, text }: { label: string; value: number | string; detail: string; icon: React.ReactNode; tone?: string; text?: boolean }) { return <div className={`metric-card ${tone ?? ""}`}><div className="metric-icon">{icon}</div><div><span>{label}</span><strong className={text ? "metric-text" : ""}>{typeof value === "number" ? formatCount(value) : value}</strong><small>{detail}</small></div></div> }
function RunTimeline({ phase }: { phase?: EvolutionPhase }) { const current = phase === "failed" ? -1 : phaseOrder.indexOf(phase ?? "scanning"); return <div className={`run-timeline ${phase === "failed" ? "failed" : ""}`}>{phaseOrder.map((item, index) => <div key={item} className={index < current ? "done" : index === current ? "current" : ""}><i>{index < current ? <Check size={11} /> : index + 1}</i><span>{phaseLabel[item]}</span></div>)}</div> }
function ProviderRow({ source }: { source: SourceSummary }) { return <div className="provider-row"><span className={`provider-dot ${providerAccent(source.provider)}`} /><div><strong>{source.provider === "codex" ? "Codex" : "Claude Code"}</strong><small>{source.available ? `${formatCount(source.sessionCount)} 个会话` : "未找到目录"}</small></div><span className={`state-badge ${source.available ? "green" : "muted-badge"}`}>{source.available ? "可读取" : "未连接"}</span></div> }
function ProviderCard({ source, enabled }: { source: SourceSummary; enabled: boolean }) { return <article className={`provider-card ${!enabled ? "disabled" : ""}`}><div className={`provider-logo ${providerAccent(source.provider)}`}>{source.provider === "codex" ? "C" : "A"}</div><div className="provider-card-main"><div className="provider-title"><strong>{source.provider === "codex" ? "Codex" : "Claude Code"}</strong><span className={`availability ${source.available && enabled ? "available" : ""}`}><i />{!enabled ? "已关闭" : source.available ? "可读取" : "未找到"}</span></div><small>{source.root}</small><div className="provider-metrics"><span><b>{formatCount(source.sessionCount)}</b> 会话</span><span><b>{formatCount(source.activityCount)}</b> 活动</span><span><b>{enabled ? "只读" : "停用"}</b> 权限</span></div></div></article> }
function ActivityRow({ activity }: { activity: Snapshot["activities"][number] }) { return <div className="activity-row"><div className={`activity-icon ${activity.kind === "error" ? "error" : ""}`}>{activity.kind === "error" ? <CircleAlert size={14} /> : <FileText size={14} />}</div><div className="activity-copy"><strong>{activity.kind === "user_message" ? "用户请求" : activity.kind === "assistant_final" ? "最终回答" : activity.kind}</strong><span>{activity.text}</span></div><div className="activity-meta"><b>{activity.provider === "codex" ? "Codex" : "Claude"}</b><small>{formatTime(activity.occurredAt)}</small></div></div> }
function CandidateRow({ entry, onEntry, onInspect }: { entry: EvolutionEntry; onEntry: (id: string, status: "active" | "rejected" | "disabled") => void; onInspect?: () => void }) { return <div className="candidate-row"><div className="entry-kind">{entry.kind === "skill" ? <BookOpen size={16} /> : <Fingerprint size={16} />}</div><div className="candidate-copy"><div><strong>{entry.title}</strong><span className="state-badge amber">需审核</span></div><p>{entry.summary}</p><small>{entry.sourceRefs.length} 个来源 · {formatTime(entry.updatedAt)}</small></div><div className="candidate-actions">{onInspect && <button className="button outline" onClick={onInspect}><FileText size={14} />证据</button>}<button className="button outline" onClick={() => onEntry(entry.id, "rejected")}>拒绝</button><button className="button primary" onClick={() => onEntry(entry.id, "active")}><Check size={14} />批准</button></div></div> }
function RunOutputRow({ entry, activities }: { entry: EvolutionEntry; activities?: EvolutionRunDetail["activities"] }) { const evidence = activities?.filter((activity) => entry.sourceRefs.includes(activity.id)) ?? []; return <div className="run-output-row"><div className="entry-kind">{entry.kind === "skill" ? <BookOpen size={16} /> : entry.kind === "revision" ? <RefreshCw size={16} /> : <Fingerprint size={16} />}</div><div className="candidate-copy"><div><strong>{entry.title}</strong><span className={`state-badge ${entry.status === "active" ? "green" : "amber"}`}>{entry.status === "active" ? "已启用" : "待审核"}</span></div><p>{entry.summary}</p><small>{entry.kind.toUpperCase()} · {entry.sourceRefs.length} 个来源</small>{activities && <details className="entry-evidence"><summary>查看关联脱敏证据（{evidence.length}/{entry.sourceRefs.length}）</summary>{evidence.length ? evidence.map((activity) => <div className="evidence-item" key={activity.id}><span>{activity.provider === "codex" ? "Codex" : "Claude Code"} · {formatTime(activity.occurredAt)}</span><p>{activity.text}</p></div>) : <span>本轮活动已清理或引用不存在</span>}</details>}</div></div> }
function TraceList({ trace, emptyTitle }: { trace: AgentTraceEvent[]; emptyTitle: string }) { return trace.length ? <div className="trace-list">{trace.slice(-16).map((event) => <div className={`trace-row ${event.resultStatus}`} key={event.id}><span className="trace-dot" /><div><strong>{event.toolName || event.eventType.replaceAll("_", " ")}</strong><p>{event.summary}</p></div><time>{event.durationMs ? `${event.durationMs} ms` : formatTime(event.occurredAt)}</time></div>)}</div> : <Empty icon={Activity} title={emptyTitle} description="运行后会显示阶段、工具调用和验证结果" /> }
function EntryCard({ entry, onEntry, onHistory }: { entry: EvolutionEntry; onEntry: (id: string, status: "active" | "rejected" | "disabled") => void; onHistory: () => void }) { return <article className="entry-card"><div className="entry-card-top"><span className={`entry-type ${entry.kind}`}>{entry.kind.toUpperCase()}</span><span className={`state-badge ${entry.status === "active" ? "green" : entry.status === "pending" ? "amber" : "muted-badge"}`}>{entry.status === "active" ? "Active" : entry.status === "pending" ? "待审核" : entry.status === "rejected" ? "已拒绝" : "已禁用"}</span></div><h3>{entry.title}</h3><p>{entry.summary}</p><div className="entry-card-foot"><span><FileText size={13} />{entry.sourceRefs.length} 个来源 · v{entry.version}</span><button className="icon-button entry-disable" onClick={onHistory} title="查看版本历史"><History size={13} /></button>{entry.status === "active" && <button className="icon-button entry-disable" onClick={() => onEntry(entry.id, "disabled")} title="禁用条目"><X size={13} /></button>}{entry.status === "disabled" && <button className="icon-button entry-disable" onClick={() => onEntry(entry.id, "active")} title="恢复条目"><RotateCcw size={13} /></button>}</div></article> }
function Empty({ icon: Icon, title, description }: { icon: typeof Activity; title: string; description: string }) { return <div className="empty-state"><div className="empty-icon"><Icon size={19} /></div><strong>{title}</strong><p>{description}</p></div> }
function Toggle({ checked, onChange }: { checked: boolean; onChange: (checked: boolean) => void }) { return <button type="button" role="switch" aria-checked={checked} className={`toggle ${checked ? "checked" : ""}`} onClick={() => onChange(!checked)}><span /></button> }
function SettingsSection({ title, description, children }: { title: string; description: string; children: React.ReactNode }) { return <section className="settings-section"><div className="settings-section-head"><h2>{title}</h2><p>{description}</p></div>{children}</section> }
function SettingRow({ title, description, children }: { title: string; description: string; children: React.ReactNode }) { return <div className="setting-row"><div><strong>{title}</strong><p>{description}</p></div>{children}</div> }
function SaveSettings({ busy, onClick }: { busy: string | null; onClick: () => void }) { return <div className="settings-actions end"><button className="button primary" onClick={onClick} disabled={busy !== null}><Check size={15} />{busy === "settings" ? "保存中" : "保存设置"}</button></div> }
function ConnectionItem({ label, path, connected, icon }: { label: string; path: string; connected: boolean; icon: React.ReactNode }) { return <div className="connection-row"><div className="connection-icon">{icon}</div><div><strong>{label}</strong><span>{path}</span></div><span className={`state-badge ${connected ? "green" : "muted-badge"}`}>{connected ? "已连接" : "未安装"}</span></div> }

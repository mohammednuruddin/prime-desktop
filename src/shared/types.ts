export type ViewId = 'chat' | 'fleet' | 'approval' | 'dashboard' | 'autonomy' | 'skills' | 'diagnostics' | 'settings'

export interface ProjectTab {
  id: string
  path: string
  name: string
}

export interface BinaryState {
  status: 'checking' | 'found' | 'installing' | 'installed' | 'error'
  path: string | null
  version: string | null
  error: string | null
  progress?: number
}

export interface AgentInfo {
  id: string
  name: string
  path: string
  status: 'starting' | 'idle' | 'working' | 'error' | 'stopped'
  model: string | null
  thinkingLevel: string | null
  messageCount: number
  cost: number
  tokensIn: number
  tokensOut: number
  contextPercent: number | null
  contextTokens: number | null
  contextWindow: number | null
  isStreaming: boolean
  sessionName: string | null
  sessionId: string | null
  version?: string
  extensionUi?: {
    title?: string
    statuses: Record<string, string>
    widgets: Record<string, { lines: string[]; placement: 'aboveEditor' | 'belowEditor' }>
    workingMessage?: string
    editorText?: string
  }
}

export interface FleetAgent {
  id: string
  name: string
  sessionId: string
  status: string
  projectPath: string
  observed: boolean
  lastEvent: string
  children: FleetAgent[]
}

export interface SubagentNode {
  id: string
  sessionId: string
  activeSessionId: string | null
  parentSessionId: string | null
  name: string
  depth: number
  status: 'working' | 'idle' | 'archived' | 'error'
  task: string
  lastActivityAt: number
  model?: string
  durationMs?: number
  answerPreview?: string
  toolUseCount?: number
  tokenCount?: number
  recap?: string
  activity?: { kind: 'waiting' | 'writing' | 'executing'; toolName?: string }
  error?: string
  children: SubagentNode[]
}

export interface ScheduleJob {
  id: string
  cron: string
  prompt: string
  active: boolean
  status?: 'active' | 'paused' | 'completed' | 'cancelled'
  source?: 'cron' | 'heartbeat' | 'rlm_heartbeat'
  runtimeKind?: 'top-level' | 'subagent'
  deliveryMode?: 'steer' | 'follow_up'
  activeSessionId?: string
  sessionId?: string
  label?: string
  schedule?: { kind: 'once' | 'cron' | 'interval'; expression: string; intervalMs?: number }
  createdAt?: string
  updatedAt?: string
  nextRunAt?: string
  lastRunAt?: string
  lastSkippedAt?: string
  lastError?: string
  runCount?: number
}

export interface Heartbeat {
  id: string | null
  schedule: string
  prompt: string
  status: string
}

export interface Checkpoint {
  id: string
  createdAt: number
  label: string
  agentId: string
  dirtyFiles: string[]
}

export interface FileDiff {
  path: string
  status: 'modified' | 'added' | 'deleted' | 'renamed'
  diff: string
}

export interface GitChange {
  path: string
  originalPath?: string
  indexStatus: string
  worktreeStatus: string
  staged: boolean
  unstaged: boolean
}

export interface GitStatus {
  isRepo: boolean
  branch: string | null
  upstream: string | null
  ahead: number
  behind: number
  changes: GitChange[]
}

export interface TerminalDataEvent {
  agentId: string
  data: string
  startOffset: number
  endOffset: number
}

export interface PermissionRule {
  pattern: string
  action: 'allow' | 'deny'
  scope: 'global' | 'project'
  projectPath?: string
}

export interface SessionSummary {
  sessionFile: string
  sessionId: string
  messageCount: number
  workingDirectory: string | null
  mtime: number
  name: string | null
}

export interface SessionStats {
  sessionFile: string
  sessionId: string
  userMessages: number
  assistantMessages: number
  toolCalls: number
  totalMessages: number
  tokens: { input: number; output: number; cacheRead: number; cacheWrite: number; total: number }
  cost: number
  contextUsage: { tokens: number | null; contextWindow: number | null; percent: number | null }
}

export interface SpendPoint {
  date: string
  cost: number
  tokensIn: number
  tokensOut: number
}

export interface SkillInfo {
  name: string
  description: string
  source: 'skill' | 'prompt' | 'extension'
  location?: string
  path?: string
}

export interface AutonomousConfig {
  enabled: boolean
  gates: string[]
  gateRetries: number
  maxContinuations: number
  maxTurns: number
  maxTokens: number
  maxSeconds: number
}

export interface AutonomousProgress {
  turns: number
  maxTurns: number
  tokens: number
  maxTokens: number
  seconds: number
  maxSeconds: number
  continuations: number
  maxContinuations: number
  active: boolean
  gates: { command: string; lastResult: 'pass' | 'fail' | null; attempts: number }[]
}

export interface GoalState {
  active: boolean
  status: 'idle' | 'active' | 'paused' | 'budget_limited' | 'complete' | 'error'
  goalId?: string
  objective?: string
  tokenBudget?: number
  tokensUsed: number
  timeUsedSeconds: number
  continuationsUsed: number
  createdAt?: number
  updatedAt?: number
  lastReason?: string
  lastError?: string
}

export interface ActionQueue {
  steering: string[]
  followUp: string[]
  mutationSupported?: boolean
}

export interface SideQuestionTurn {
  question: string
  answer: string
}

export interface SessionTreeNode {
  entry: {
    id: string
    parentId: string | null
    type: string
    timestamp: string
    message?: unknown
    [key: string]: unknown
  }
  label?: string
  labelTimestamp?: string
  children: SessionTreeNode[]
}

export interface ResourceSourceInfo {
  path: string
  source: string
  scope: 'user' | 'project' | 'temporary'
  origin: 'package' | 'top-level'
  baseDir?: string
}

export interface ResourceItem {
  type: 'skill' | 'prompt' | 'extension' | 'theme' | 'context'
  name: string
  description?: string
  path?: string
  sourceInfo?: ResourceSourceInfo
}

export interface ResourceSnapshot {
  resources: ResourceItem[]
  diagnostics: { type: 'warning' | 'error' | 'collision'; message: string; path?: string }[]
}

export interface ModelCatalog {
  models: { id: string; name?: string; provider: string; reasoning?: boolean }[]
  configuredProviders: string[]
  transport: 'sse' | 'websocket' | 'websocket-cached' | 'auto'
}

export interface TraceInfo {
  path: string
  name: string
  size: number
  modifiedAt: number
}

export interface DaemonDiagnostic {
  command: string
  output: string
  ok: boolean
}

export interface FileActivity {
  path: string
  kind: 'add' | 'change' | 'unlink'
  at: number
}

export interface UiDialog {
  id: string
  agentId: string
  method: 'confirm' | 'select' | 'input' | 'editor'
  title: string
  message?: string
  options?: string[]
  prefill?: string
}

export interface PrimeEvent {
  agentId: string
  type: string
  payload: unknown
}

export interface Toast {
  id: string
  kind: 'info' | 'success' | 'warning' | 'error'
  text: string
}

export type ThemeMode = 'system' | 'light' | 'dark'

export interface ThemeConfig {
  accent: string
  contrast: number
  fonts: {
    code: string | null
    ui: string | null
  }
  ink: string
  opaqueWindows: boolean
  semanticColors: {
    diffAdded: string
    diffRemoved: string
    skill: string
  }
  surface: string
}

export interface AppSettings {
  notifications: boolean
  checkpoints: boolean
  dockBadge: boolean
  thinkingLevel: string
  autoCompaction: boolean
  autoRetry: boolean
  model: string | null
  rlmMaxDepth: number
  transport: 'sse' | 'websocket' | 'websocket-cached' | 'auto'
  autonomous: AutonomousConfig
  themeMode: ThemeMode
  codeThemeId: string
  lightTheme: ThemeConfig
  darkTheme: ThemeConfig
}

export interface AuthProvider {
  id: string
  name: string
  configured: boolean
}

export interface RlmDepthStatus {
  maxDepth: number
  source: string
}

export const IPC = {
  binaryGet: 'binary:get',
  binaryInstall: 'binary:install',
  tabsList: 'tabs:list',
  tabAdd: 'tab:add',
  tabRemove: 'tab:remove',
  tabSelect: 'tab:select',
  agentCreate: 'agent:create',
  agentState: 'agent:state',
  agentCommand: 'agent:command',
  agentMessages: 'agent:messages',
  agentStats: 'agent:stats',
  agentSessions: 'agent:sessions',
  agentResume: 'agent:resume',
  agentSessionDelete: 'agent:session-delete',
  sessionPinsGet: 'session:pins-get',
  sessionPinSet: 'session:pin-set',
  agentCommands: 'agent:commands',
  fleetList: 'fleet:list',
  fleetTree: 'fleet:tree',
  fleetMessages: 'fleet:messages',
  fleetObserve: 'fleet:observe',
  fleetUnobserve: 'fleet:unobserve',
  fleetSend: 'fleet:send',
  fleetSchedules: 'fleet:schedules',
  fleetScheduleAdd: 'fleet:schedule-add',
  fleetScheduleCancel: 'fleet:schedule-cancel',
  fleetHeartbeat: 'fleet:heartbeat',
  fleetHeartbeatAction: 'fleet:heartbeat-action',
  gitCheckpoint: 'git:checkpoint',
  gitList: 'git:list',
  gitRestore: 'git:restore',
  gitDiff: 'git:diff',
  gitDiffFiles: 'git:diff-files',
  gitStatus: 'git:status',
  gitFileDiff: 'git:file-diff',
  gitStage: 'git:stage',
  gitUnstage: 'git:unstage',
  gitStageAll: 'git:stage-all',
  gitUnstageAll: 'git:unstage-all',
  gitCommit: 'git:commit',
  terminalStart: 'terminal:start',
  terminalWrite: 'terminal:write',
  terminalResize: 'terminal:resize',
  terminalRestart: 'terminal:restart',
  terminalClear: 'terminal:clear',
  terminalClose: 'terminal:close',
  terminalData: 'terminal:data',
  terminalExit: 'terminal:exit',
  permissionsList: 'permissions:list',
  permissionsSet: 'permissions:set',
  permissionsRemove: 'permissions:remove',
  permissionsMatch: 'permissions:match',
  dashboardSpend: 'dashboard:spend',
  dashboardModels: 'dashboard:models',
  autonomyGet: 'autonomy:get',
  autonomySet: 'autonomy:set',
  skillsList: 'skills:list',
  skillsInstall: 'skills:install',
  dialogRespond: 'dialog:respond',
  settingsGet: 'settings:get',
  settingsSet: 'settings:set',
  eventStream: 'events',
  toastStream: 'toasts',
  nativeNotify: 'notify',
  revealInFinder: 'reveal',
  openExternal: 'open-external',
  quit: 'app:quit',
  chooseFolder: 'choose-folder',
  chooseSave: 'choose-save',
  chooseFile: 'choose-file',
  authList: 'auth:list',
  authSet: 'auth:set',
  authRemove: 'auth:remove',
  authOpenTui: 'auth:open-tui',
  rlmGet: 'rlm:get',
  rlmSet: 'rlm:set',
  exportHtml: 'session:export-html',
  shareSession: 'session:share',
  changelog: 'app:changelog',
  logsReveal: 'app:logs',
  windowFullscreen: 'window:fullscreen',
  agentHarness: 'agent:harness'
} as const

export type AgentCommand =
  | { type: 'prompt'; message: string; images?: { type: 'image'; data: string; mimeType: string }[]; streamingBehavior?: 'steer' | 'followUp' }
  | { type: 'steer'; message: string }
  | { type: 'follow_up'; message: string }
  | { type: 'abort' }
  | { type: 'compact'; customInstructions?: string }
  | { type: 'new_session' }
  | { type: 'switch_session'; sessionPath: string }
  | { type: 'fork'; entryId?: string }
  | { type: 'clone' }
  | { type: 'set_model'; provider?: string; modelId?: string }
  | { type: 'set_thinking_level'; level: string }
  | { type: 'set_auto_compaction'; enabled: boolean }
  | { type: 'set_auto_retry'; enabled: boolean }
  | { type: 'set_session_name'; name: string }
  | { type: 'get_available_models' }
  | { type: 'get_commands' }
  | { type: 'bash'; command: string }
  | { type: 'refine'; instructions?: string; rollbackId?: string; global?: boolean }
  | { type: 'export_html'; outputPath?: string }
  | { type: 'get_fork_messages' }
  | { type: 'get_last_assistant_text' }
  | { type: 'get_session_stats' }
  | { type: 'set_heartbeat'; schedule: string; prompt: string; deliveryMode?: string }
  | { type: 'get_heartbeat' }
  | { type: 'update_heartbeat'; action: string }
  | { type: 'set_rlm_max_depth'; maxDepth: number; global?: boolean }
  | { type: 'get_rlm_max_depth_status' }

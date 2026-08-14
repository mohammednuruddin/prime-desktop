import type { PrimeApi } from '../../../preload'
import { CODEX_DARK_THEME, PRIME_LIGHT_THEME } from '@shared/themes'

const devSettings = {
  notifications: true,
  checkpoints: true,
  dockBadge: false,
  thinkingLevel: 'high',
  showReasoning: true,
  autoCompaction: true,
  autoRetry: true,
  model: null,
  rlmMaxDepth: 1,
  transport: 'auto' as const,
  autonomous: {
    enabled: false,
    gates: [],
    gateRetries: 2,
    maxContinuations: 8,
    maxTurns: 30,
    maxTokens: 100000,
    maxSeconds: 3600
  },
  themeMode: 'system' as const,
  codeThemeId: 'codex',
  lightTheme: PRIME_LIGHT_THEME,
  darkTheme: CODEX_DARK_THEME
}

const noopSubscription = () => () => {}

export function installDevPrime(): void {
  if (window.prime || !import.meta.env.DEV) return

  const agentId = 'agent-prime'
  const messages = [
    {
      id: 'user-1',
      role: 'user',
      content: 'Review the research and build the evidence-backed implementation. Use a subagent for the Ghana-specific sources.',
      timestamp: Date.now() - 90_000
    },
    {
      id: 'assistant-1',
      role: 'assistant',
      timestamp: Date.now() - 60_000,
      content: [
        { type: 'text', text: 'I will inspect the current implementation, verify the source material, and keep the changes focused.' },
        { type: 'toolCall', id: 'read-1', name: 'read_file', arguments: { path: '/workspace/prime/src/renderer/src/App.tsx' } },
        { type: 'toolCall', id: 'edit-1', name: 'apply_patch', arguments: { path: '/workspace/prime/src/renderer/src/App.tsx' }, result: '+42 -11' },
        { type: 'toolCall', id: 'edit-2', name: 'apply_patch', arguments: { path: '/workspace/prime/src/renderer/src/styles.css' }, result: '+86 -34' },
        {
          type: 'toolCall',
          id: 'subagent-1',
          name: 'ipython',
          arguments: { code: "evidence = await rlm('Find primary Ghana sources and return concise evidence.', name='Ghana evidence subagent')\nevidence" },
          result: "RLMSpawnHandle(rlm_child_id='ghana-evidence', name='Ghana evidence subagent', model='relay-station/gpt-5.6-sol')"
        },
        { type: 'text', text: 'The interaction model now matches the harness: compact activity in the main feed, with the selected subagent opening in a dedicated inspector.' }
      ]
    },
    {
      id: 'child-1',
      role: 'assistant',
      timestamp: Date.now() - 50_000,
      content: '[from child:Ghana evidence subagent]\nAgent-to-agent message received.\nSource: agent_message\nFrom: Ghana evidence subagent, session child-session\nMessage id: agentmsg-test\n\nPrimary sources verified. The evidence set is ready for implementation.',
      details: {
        id: 'agentmsg-test',
        message: 'Primary sources verified. The evidence set is ready for implementation.',
        from: { sessionId: 'ghana-evidence', sessionName: 'Ghana evidence subagent', runtimeKind: 'subagent' },
        fromRelationship: 'child'
      }
    }
  ]

  const api = {
    initial: async () => ({
      tabs: [{ id: 'prime', path: '/workspace/prime', name: 'prime' }],
      activeTabId: 'prime',
      settings: devSettings,
      binary: { status: 'found', path: '/usr/local/bin/prime-agent', version: '0.1.0', error: null }
    }),
    agentState: async () => [{ id: agentId, name: 'prime', path: '/workspace/prime', status: 'idle', model: 'openai-codex/gpt-5.6-sol', thinkingLevel: 'high', messageCount: 2, cost: 0, tokensIn: 1200, tokensOut: 800, contextPercent: 12, contextTokens: 2000, contextWindow: 16000, isStreaming: false, sessionName: 'Prime desktop redesign', sessionId: 'dev-prime' }],
    agentMessages: async () => messages,
    agentCommands: async () => [],
    agentCommand: async (_id: string, command: { type?: string }) => command.type === 'get_available_models'
      ? { models: [{ provider: 'openai-codex', id: 'gpt-5.6-sol' }, { provider: 'anthropic', id: 'claude-opus-5' }] }
      : null,
    agentStats: async () => null,
    agentSessions: async () => [{ sessionFile: 'dev.jsonl', sessionId: 'dev-prime', messageCount: 2, workingDirectory: '/workspace/prime', mtime: Date.now(), name: 'Prime desktop redesign' }],
    settingsGet: async () => devSettings,
    settingsSet: async () => null,
    skillsList: async () => [
      { name: 'browser', description: 'Inspect web applications and verify user flows in a real browser.', source: 'skill', location: 'bundled' },
      { name: 'research', description: 'Collect and compare primary sources before implementation.', source: 'skill', location: 'project' },
      { name: 'release-check', description: 'Run release verification and summarize failures.', source: 'prompt', path: '~/.prime/prompts/release-check.md' }
    ],
    skillsInstall: async () => ({ ok: true }),
    fleetSchedules: async () => ({ [agentId]: [{ id: 'daily-check', cron: '0 9 * * 1-5', prompt: 'Check CI and summarize failures', active: true }] }),
    fleetTree: async () => [],
    fleetHeartbeat: async () => null,
    gitList: async () => [{ id: 'a13fd22', createdAt: Date.now() - 240_000, label: 'Before inspector redesign', agentId, dirtyFiles: ['src/renderer/src/App.tsx', 'src/renderer/src/styles.css'] }],
    gitDiffFiles: async () => [{ path: 'src/renderer/src/styles.css', status: 'modified', diff: '@@ -581,4 +581,8 @@\n-.side-panel { width: 320px; }\n+.side-panel.closed { width: 0; }\n+.side-panel.open { min-width: 360px; }' }],
    gitStatus: async () => ({ isRepo: true, branch: 'main', upstream: 'origin/main', ahead: 1, behind: 0, changes: [{ path: 'src/renderer/src/styles.css', indexStatus: ' ', worktreeStatus: 'M', staged: false, unstaged: true }] }),
    gitFileDiff: async () => '@@ -581,4 +581,8 @@\n-.side-panel { width: 320px; }\n+.side-panel.open { min-width: 360px; }',
    gitStage: async () => ({ isRepo: true, branch: 'main', upstream: 'origin/main', ahead: 1, behind: 0, changes: [] }),
    gitUnstage: async () => ({ isRepo: true, branch: 'main', upstream: 'origin/main', ahead: 1, behind: 0, changes: [] }),
    gitStageAll: async () => ({ isRepo: true, branch: 'main', upstream: 'origin/main', ahead: 1, behind: 0, changes: [] }),
    gitUnstageAll: async () => ({ isRepo: true, branch: 'main', upstream: 'origin/main', ahead: 1, behind: 0, changes: [] }),
    gitCommit: async () => ({ sha: 'a13fd22', summary: 'commit', status: { isRepo: true, branch: 'main', upstream: 'origin/main', ahead: 1, behind: 0, changes: [] } }),
    terminalStart: async () => ({ running: true, pid: 1, buffer: '$ ', cwd: '/workspace/prime', offset: 2 }),
    terminalWrite: async () => null,
    terminalResize: async () => null,
    terminalRestart: async () => ({ running: true, pid: 1, buffer: '$ ', cwd: '/workspace/prime', offset: 2 }),
    terminalClear: async () => null,
    terminalClose: async () => null,
    permissionsList: async () => [{ pattern: 'npm run *', action: 'allow', scope: 'project', projectPath: '/workspace/prime' }],
    dashboardSpend: async () => ({ points: [{ date: '2026-08-08', cost: 0.12, tokensIn: 32000, tokensOut: 9000 }, { date: '2026-08-09', cost: 0.08, tokensIn: 21000, tokensOut: 7000 }], totals: { cost: 0.2, tokensIn: 53000, tokensOut: 16000 } }),
    dashboardModels: async () => ['openai-codex/gpt-5.6-sol'],
    autonomyGet: async () => ({ config: { enabled: false, gates: ['npm run typecheck'], gateRetries: 2, maxContinuations: 4, maxTurns: 20, maxTokens: 120000, maxSeconds: 3600 }, progress: {} }),
    autonomySet: async () => null,
    agentHarness: async (_agentId: string, action: string) => {
      if (action === 'queue') return { steering: [], followUp: [], mutationSupported: true }
      if (action === 'resources') return { skills: [], prompts: [], extensions: [], themes: [], diagnostics: { skills: [], prompts: [], extensions: [], themes: [] } }
      if (action === 'package') return { ok: true, output: 'No packages installed.' }
      if (action === 'mcp_get' || action === 'mcp_set') return { servers: {} }
      if (action === 'trace_list') return { files: [] }
      if (action === 'traces') return { enabled: false }
      if (action === 'model_catalog') return { models: [], configuredProviders: [] }
      if (action === 'refinement_history') return { history: [] }
      if (action === 'heartbeats') return { heartbeats: [] }
      if (action === 'get_tree_full') return { tree: [], leafId: null }
      return {}
    },
    tabSelect: async () => null,
    tabRemove: async () => ({ tabs: [], activeTabId: null }),
    onEvent: noopSubscription,
    onTerminalData: noopSubscription,
    onTerminalExit: noopSubscription,
    onToast: noopSubscription,
    onMenuOpenFolder: noopSubscription
  }

  window.prime = new Proxy(api, {
    get(target, key) {
      if (key in target) return target[key as keyof typeof target]
      return async () => null
    }
  }) as unknown as PrimeApi
}

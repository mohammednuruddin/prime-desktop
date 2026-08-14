import { EventEmitter } from 'events'
import { existsSync, readdirSync } from 'fs'
import { readFile, writeFile, mkdir, stat, readdir } from 'fs/promises'
import { homedir } from 'os'
import { join, basename, resolve } from 'path'
import { RpcClient } from './rpc'
import { DaemonTransport, type DaemonTreeNode } from './daemonTransport'
import { BinaryManager } from './binary'
import {
  commitAll,
  changedFiles,
  isGitRepo,
  checkoutCommit,
  statusShort,
  diffForFile,
  gitStatus as readGitStatus,
  gitFileDiff,
  stageFiles,
  unstageFiles,
  stageAll,
  unstageAll,
  commitStaged
} from './git'
import { getState, setModel as persistModel, setSettings } from './store'
import {
  getAgentTracesEnabled,
  setAgentTracesEnabled,
  writePrimeRlmMaxDepth,
  getMcpServers,
  setMcpServer
} from './primeFiles'
import { modelKeyFromState, parseModelList } from '@shared/models'
import { isInternalStateRestoreMessage } from '@shared/messageVisibility'
import type {
  AgentInfo,
  AgentCommand,
  AppSettings,
  Checkpoint,
  FileDiff,
  GitStatus,
  PrimeEvent,
  SessionSummary,
  UiDialog,
  ProjectTab,
  FleetAgent,
  ScheduleJob,
  Heartbeat,
  SkillInfo,
  SpendPoint,
  AutonomousConfig,
  AutonomousProgress,
  Toast
} from '@shared/types'

const SESSION_DIR = join(homedir(), '.prime', 'agent', 'sessions')
const CHECKPOINT_DIR = join(homedir(), 'Library', 'Application Support', 'PrimeDesktop', 'checkpoints')
const APP_SUPPORT_DIR = join(homedir(), 'Library', 'Application Support', 'PrimeDesktop')
const APP_DAEMON_SOCKET = join(APP_SUPPORT_DIR, 'prime-agent.sock')
const MODELS_FILE = join(homedir(), '.prime', 'agent', 'models.json')

interface Agent {
  id: string
  tabId: string
  path: string
  client: DaemonTransport | null
  info: AgentInfo
  messages: unknown[]
  dialogs: Map<string, UiDialog>
  checkpoints: Checkpoint[]
  starts: number
  queued: { steer: string[]; followUps: string[] }
  skills: SkillInfo[]
  availableModels: unknown[] | null
  commands: unknown[] | null
  stats: unknown
  lastEvent: string
  pendingRuntimeReload: boolean
}

export class AgentManager extends EventEmitter {
  private agents = new Map<string, Agent>()
  private observers = new Map<string, RpcClient>()
  private binary: BinaryManager

  constructor(binary: BinaryManager) {
    super()
    this.binary = binary
  }

  private emitToRenderer(channel: 'events' | 'toasts', payload: unknown): void {
    this.emit('renderer', { channel, payload })
  }

  // ---------- Lifecycle ----------

  async openTab(tab: ProjectTab, settings: AppSettings): Promise<AgentInfo> {
    const existing = [...this.agents.values()].find((a) => a.tabId === tab.id)
    if (existing) {
      existing.info.path = tab.path
      existing.path = tab.path
      return existing.info
    }
    const id = `agent-${tab.id}`
    const agent: Agent = {
      id,
      tabId: tab.id,
      path: tab.path,
      client: null,
      info: {
        id,
        name: basename(tab.path),
        path: tab.path,
        status: 'starting',
        model: null,
        thinkingLevel: null,
        messageCount: 0,
        cost: 0,
        tokensIn: 0,
        tokensOut: 0,
        contextPercent: null,
        contextTokens: null,
        contextWindow: null,
        isStreaming: false,
        sessionName: null,
        sessionId: null
      },      messages: [],
      dialogs: new Map(),
      checkpoints: [],
      starts: 0,
      queued: { steer: [], followUps: [] },
      skills: [],
      availableModels: null,
      commands: null,
      stats: null,
      lastEvent: '',
      pendingRuntimeReload: false
    }
    this.agents.set(id, agent)
    this.publish(agent)
    await this.connect(agent, settings)
    return agent.info
  }

  private async connect(agent: Agent, settings: AppSettings): Promise<void> {
    const binary = this.binary.getBinary()
    await mkdir(APP_SUPPORT_DIR, { recursive: true })
    const client = new DaemonTransport({
      binary,
      socketPath: APP_DAEMON_SOCKET,
      cwd: agent.path,
      settings
    })
    agent.client = client
    agent.starts++
    const startNo = agent.starts

    client.on('event', (ev) => this.handleEvent(agent, ev))
    client.on('process_error', (msg) => {
      if (agent.starts !== startNo) return
      agent.info.status = 'error'
      this.publish(agent)
      this.toast(agent, 'error', `prime-agent failed: ${msg}`)
    })
    client.onExit(() => {
      if (agent.starts !== startNo) return
      agent.info.status = 'stopped'
      this.publish(agent)
    })

    await client.start()

    agent.info.status = 'idle'
    agent.info.thinkingLevel = settings.thinkingLevel
    this.publish(agent)

    try {
      const [state, models] = await Promise.all([
        client.send({ type: 'get_state' }),
        client.send({ type: 'get_available_models' }).catch(() => null)
      ])
      if (agent.starts !== startNo) return
      const s = state as Record<string, unknown>
      agent.info.model = modelKeyFromState(s.model)
      agent.info.sessionName = (s.sessionName as string | null) ?? null
      agent.info.sessionId = (s.sessionId as string | null) ?? null
      agent.info.messageCount = (s.messageCount as number) ?? 0
      agent.info.isStreaming = (s.isStreaming as boolean) ?? false
      const rpcModels = parseModelList(models)
      agent.availableModels = rpcModels.length > 0 ? rpcModels : await localModelCatalog()
    } catch {
      agent.availableModels = await localModelCatalog()
    }

    if (settings.autoCompaction) {
      client.send({ type: 'set_auto_compaction', enabled: true }).catch((err) => {
        console.error('[prime-desktop] set_auto_compaction failed:', err)
      })
    }
    if (settings.autoRetry) {
      client.send({ type: 'set_auto_retry', enabled: true }).catch((err) => {
        console.error('[prime-desktop] set_auto_retry failed:', err)
      })
    }
    if (agent.info.thinkingLevel) {
      client.send({ type: 'set_thinking_level', level: agent.info.thinkingLevel }).catch((err) => {
        console.error('[prime-desktop] set_thinking_level failed:', err)
      })
    }
    client.send({ type: 'get_commands' }).then((cmds) => {
      if (agent.starts !== startNo) return
      const c = cmds as { commands?: unknown[] }
      agent.commands = c.commands ?? []
      this.publish(agent)
    }).catch((err) => {
      console.error('[prime-desktop] get_commands failed:', err)
    })

    const msgs = await client.send({ type: 'get_messages' }).catch(() => null)
    if (agent.starts === startNo && msgs) {
      agent.messages = this.visibleMessages((msgs as { messages: unknown[] }).messages ?? [])
      this.syncStatsFromMessages(agent)
      this.publish(agent)
    }
  }

  // ---------- Event handling ----------

  private handleEvent(agent: Agent, ev: Record<string, unknown>): void {
    const type = ev.type as string
    if (!type) return
    agent.lastEvent = `${type} ${new Date().toLocaleTimeString()}`
    const event: PrimeEvent = { agentId: agent.id, type, payload: ev }

    switch (type) {
      case 'agent_start':
        agent.info.status = 'working'
        agent.info.isStreaming = true
        break
      case 'agent_end':
        agent.info.status = 'idle'
        agent.info.isStreaming = false
        // Prime Agent owns and drains its steering/follow-up queue. The
        // session_action_update event is a read-only snapshot for UI display;
        // replaying an item here re-enqueues it after every turn forever.
        this.refreshStats(agent)
        if (agent.pendingRuntimeReload) {
          agent.pendingRuntimeReload = false
          void agent.client?.send({ type: 'reload' }).then(() => {
            this.toast(agent, 'info', 'Agent messaging runtime refreshed. Retry the follow-up.')
          }).catch((error) => {
            console.error('[prime-desktop] runtime recovery reload failed:', error)
          })
        }
        break
      case 'message_update': {
        const m = ev.message as Record<string, unknown> | undefined
        if (m) this.upsertMessage(agent, m)
        break
      }
      case 'message_start':
      case 'message_end': {
        const m = ev.message as Record<string, unknown> | undefined
        if (m) this.upsertMessage(agent, m)
        break
      }
      case 'custom_message': {
        if (ev.display === false) break
        if (ev.customType === 'ipython_state_restored') break
        if (ev.customType === 'agent_message') {
          this.upsertMessage(agent, { ...ev, role: 'assistant' })
        } else if (
          ev.customType === 'session_slash_command' ||
          ev.customType === 'session_slash_command_result' ||
          ev.customType === 'compaction_outcome'
        ) {
          this.upsertMessage(agent, {
            ...ev,
            role: 'system',
            id: `sys-${String(ev.customType)}-${String(ev.timestamp ?? Date.now())}`,
            content: String(ev.content ?? '')
          })
        }
        break
      }
      case 'session_replaced': {
        agent.info.extensionUi = undefined
        agent.messages = this.visibleMessages((ev.messages as unknown[] | undefined) ?? [])
        const state = (ev.state as Record<string, unknown> | undefined) ?? {}
        agent.info.sessionId = (state.sessionId as string | null) ?? agent.info.sessionId
        agent.info.sessionName = (state.sessionName as string | null) ?? null
        agent.info.isStreaming = (state.isStreaming as boolean) ?? false
        this.syncStatsFromMessages(agent)
        break
      }
      case 'session_resynced': {
        agent.info.extensionUi = undefined
        const snapshot = (ev.snapshot as Record<string, unknown> | undefined) ?? {}
        agent.messages = this.visibleMessages((snapshot.messages as unknown[] | undefined) ?? [])
        const state = (snapshot.state as Record<string, unknown> | undefined) ?? {}
        agent.info.sessionId = (state.sessionId as string | null) ?? agent.info.sessionId
        agent.info.sessionName = (state.sessionName as string | null) ?? null
        agent.info.isStreaming = (state.isStreaming as boolean) ?? false
        this.syncStatsFromMessages(agent)
        break
      }
      case 'session_started':
        agent.info.extensionUi = undefined
        break
      case 'turn_start':
        agent.info.status = 'working'
        break
      case 'turn_end': {
        const m = ev.message as Record<string, unknown> | undefined
        if (m) this.upsertMessage(agent, m)
        const results = ev.toolResults as unknown[] | undefined
        if (results) {
          for (const r of results) this.upsertToolResult(agent, r as Record<string, unknown>)
        }
        this.refreshStats(agent)
        break
      }
      case 'tool_execution_start':
      case 'tool_execution_update':
      case 'tool_execution_end':
        if (
          String(ev.toolName ?? '').toLowerCase() === 'ipython' &&
          JSON.stringify(ev.result ?? ev).includes('host request type \\"agent_message.send\\" is not available')
        ) {
          agent.pendingRuntimeReload = true
        }
        this.emitToRenderer('events', event)
        break
      case 'session_action_update': {
        const a = ev.actions as { steering?: string[]; followUps?: string[]; queuedCount?: number }
        agent.queued.steer = a.steering ?? []
        agent.queued.followUps = a.followUps ?? []
        break
      }
      case 'compaction_start':
        this.toast(agent, 'info', 'Compacting context…')
        break
      case 'compaction_end':
        this.toast(agent, 'success', 'Context compacted')
        break
      case 'auto_retry_start':
        this.toast(agent, 'warning', `Retrying (attempt ${String(ev.attempt)})`)
        break
      case 'extension_ui_request': {
        const id = ev.id as string
        const method = String(ev.method)
        if (['confirm', 'select', 'input', 'editor'].includes(method)) {
          agent.dialogs.set(id, {
            id,
            agentId: agent.id,
            method: method as UiDialog['method'],
            title: (ev.title as string) ?? method,
            message: (ev.message as string) ?? undefined,
            options: (ev.options as string[]) ?? undefined,
            prefill: (ev.prefill as string) ?? undefined
          })
          this.publish(agent)
          this.notifyIfDesired(agent, `Agent needs input: ${(ev.title as string) ?? method}`)
        } else if ((method as string) === 'notify') {
          this.toast(agent, (ev.notifyType as string) === 'error' ? 'error' : 'info', (ev.message as string) ?? '')
        } else {
          const extensionUi = agent.info.extensionUi ?? { statuses: {}, widgets: {} }
          if (method === 'setStatus') {
            const key = String(ev.statusKey ?? 'extension')
            const text = typeof ev.statusText === 'string' ? ev.statusText : undefined
            const statuses = { ...extensionUi.statuses }
            if (text) statuses[key] = text
            else delete statuses[key]
            agent.info.extensionUi = { ...extensionUi, statuses }
          } else if (method === 'setWidget') {
            const key = String(ev.widgetKey ?? 'extension')
            const widgets = { ...extensionUi.widgets }
            const content = Array.isArray(ev.widgetLines) ? ev.widgetLines.map(String) : undefined
            if (content) {
              widgets[key] = {
                lines: content,
                placement: ev.widgetPlacement === 'belowEditor' ? 'belowEditor' : 'aboveEditor'
              }
            } else {
              delete widgets[key]
            }
            agent.info.extensionUi = { ...extensionUi, widgets }
          } else if (method === 'setTitle') {
            agent.info.extensionUi = { ...extensionUi, title: typeof ev.title === 'string' ? ev.title : undefined }
          } else if (method === 'setWorkingMessage') {
            agent.info.extensionUi = { ...extensionUi, workingMessage: typeof ev.message === 'string' ? ev.message : undefined }
          } else if (method === 'setEditorText' || method === 'set_editor_text') {
            agent.info.extensionUi = { ...extensionUi, editorText: typeof ev.text === 'string' ? ev.text : '' }
          }
        }
        break
      }
      case 'parse_error':
        this.toast(agent, 'error', 'Protocol parse error from agent')
        break
    }
    this.publish(agent)
    this.emitToRenderer('events', event)
  }

  private upsertMessage(agent: Agent, m: Record<string, unknown>): void {
    if (isInternalStateRestoreMessage(m)) {
      const key = this.msgKey(m)
      agent.messages = agent.messages.filter((message) => this.msgKey(message as Record<string, unknown>) !== key)
      return
    }
    const entries = m.entries as unknown[] | undefined
    if (entries) {
      for (const e of entries) {
        const em = e as Record<string, unknown>
        if (isInternalStateRestoreMessage(em)) continue
        const idx = agent.messages.findIndex((x) => this.msgKey(x as Record<string, unknown>) === this.msgKey(em))
        if (idx >= 0) agent.messages[idx] = em
        else agent.messages.push(em)
      }
      agent.info.messageCount = agent.messages.filter((x) => (x as Record<string, unknown>).role === 'user').length
      return
    }
    const idx = agent.messages.findIndex((x) => this.msgKey(x as Record<string, unknown>) === this.msgKey(m))
    if (idx >= 0) agent.messages[idx] = m
    else agent.messages.push(m)
    agent.info.messageCount = agent.messages.filter((x) => (x as Record<string, unknown>).role === 'user').length
  }

  private msgKey(m: Record<string, unknown>): string {
    const id = m.id as string | undefined
    if (id) return `id:${id}`
    return `ts:${String(m.timestamp ?? '')}:${String(m.role ?? '')}`
  }

  private upsertToolResult(agent: Agent, r: Record<string, unknown>): void {
    const id = r.toolCallId as string
    const idx = agent.messages.findIndex((x) => (x as Record<string, unknown>).toolCallId === id)
    if (idx >= 0) agent.messages[idx] = r
    else agent.messages.push(r)
  }

  private refreshStats(agent: Agent): void {
    agent.client?.send({ type: 'get_session_stats' }).then((stats) => {
      agent.stats = stats
      const s = stats as Record<string, unknown>
      const tokens = (s.tokens as Record<string, number>) ?? {}
      agent.info.tokensIn = tokens.input ?? 0
      agent.info.tokensOut = tokens.output ?? 0
      agent.info.cost = (s.cost as number) ?? 0
      const ctx = (s.contextUsage as Record<string, unknown>) ?? {}
      agent.info.contextPercent = (ctx.percent as number | null) ?? null
      agent.info.contextTokens = (ctx.tokens as number | null) ?? null
      agent.info.contextWindow = (ctx.contextWindow as number | null) ?? null
      this.publish(agent)
    }).catch(() => {})
  }

  private syncStatsFromMessages(agent: Agent): void {
    agent.info.messageCount = agent.messages.filter((m) => (m as Record<string, unknown>).role === 'user').length
  }

  private visibleMessages(messages: unknown[]): unknown[] {
    return messages.filter((message) => !isInternalStateRestoreMessage(message))
  }

  private notifyIfDesired(agent: Agent, text: string): void {
    void getState().then((s) => {
      if (!s.settings.notifications) return
      this.emitToRenderer('toasts', {
        id: `t${Date.now()}-${Math.random().toString(36).slice(2, 6)}`,
        kind: 'info',
        text: `${agent.info.name}: ${text}`
      } as Toast)
      this.emit('native-notify', { title: agent.info.name, body: text })
    })
  }

  private toast(_agent: Agent, kind: Toast['kind'], text: string): void {
    this.emitToRenderer('toasts', {
      id: `t${Date.now()}-${Math.random().toString(36).slice(2, 6)}`,
      kind,
      text
    } as Toast)
  }

  private publish(agent: Agent): void {
    this.emitToRenderer('events', { agentId: agent.id, type: 'agent_info', payload: agent.info } as PrimeEvent)
  }

  // ---------- Commands ----------

  async runCommand(agentId: string, cmd: AgentCommand, settings: AppSettings): Promise<unknown> {
    const agent = this.agents.get(agentId)
    if (cmd.type === 'get_available_models') {
      if (agent?.client?.running) {
        try {
          const res = await agent.client.send(cmd as Record<string, unknown>)
          const models = parseModelList(res)
          if (models.length > 0) {
            agent.availableModels = models
            return { models }
          }
        } catch {
          /* fall through to cached and local catalogs */
        }
      }
      if (Array.isArray(agent?.availableModels) && agent.availableModels.length > 0) {
        return { models: agent.availableModels }
      }
      return { models: await localModelCatalog() }
    }
    if (!agent || !agent.client?.running) throw new Error('Agent not connected')
    if (cmd.type === 'new_session') return this.newSession(agentId)
    if (cmd.type === 'prompt' && settings.checkpoints) {
      await this.createCheckpoint(agent, 'before-prompt')
    }
    if (cmd.type === 'set_model') {
      const res = await agent.client.send(cmd as Record<string, unknown>)
      const provider = (cmd as { provider?: string }).provider
      const modelId = (cmd as { modelId?: string }).modelId
      if (modelId) agent.info.model = provider ? `${provider}/${modelId}` : modelId
      this.publish(agent)
      return res
    }
    return agent.client.send(cmd as Record<string, unknown>)
  }

  async getMessages(agentId: string): Promise<unknown[]> {
    const agent = this.agents.get(agentId)
    if (!agent) return []
    return agent.messages
  }

  getProjectPath(agentId: string): string | null {
    return this.agents.get(agentId)?.path ?? null
  }

  async getStats(agentId: string): Promise<unknown> {
    const agent = this.agents.get(agentId)
    if (!agent) return null
    if (agent.client) {
      try {
        const stats = await agent.client.send({ type: 'get_session_stats' })
        agent.stats = stats
        const s = stats as Record<string, unknown>
        const tokens = (s.tokens as Record<string, number>) ?? {}
        agent.info.tokensIn = tokens.input ?? 0
        agent.info.tokensOut = tokens.output ?? 0
        agent.info.cost = (s.cost as number) ?? 0
        const ctx = (s.contextUsage as Record<string, unknown>) ?? {}
        agent.info.contextPercent = (ctx.percent as number | null) ?? null
        agent.info.contextTokens = (ctx.tokens as number | null) ?? null
        agent.info.contextWindow = (ctx.contextWindow as number | null) ?? null
        this.publish(agent)
      } catch {
        /* stats unavailable */
      }
    }
    return agent.stats
  }

  async getSessions(agentId?: string): Promise<SessionSummary[]> {
    if (!existsSync(SESSION_DIR)) return []
    const projectPath = agentId ? this.agents.get(agentId)?.path : undefined
    const files = readdirSync(SESSION_DIR).filter((f) => f.endsWith('.jsonl'))
    const out: SessionSummary[] = []
    for (const f of files) {
      const full = join(SESSION_DIR, f)
      const st = await stat(full)
      try {
        const records = (await readFile(full, 'utf8')).split('\n').filter(Boolean)
          .map((line) => JSON.parse(line) as Record<string, unknown>)
        const header = records.find((record) => record.type === 'session') ?? records[0] ?? {}
        const entries = header.entries as unknown[] | undefined
        const firstEntry = entries?.[0] as Record<string, unknown> | undefined
        const dir = String(header.cwd ?? header.workingDirectory ?? firstEntry?.workingDirectory ?? '') || null
        if (projectPath && (!dir || resolve(dir) !== resolve(projectPath))) continue

        const messageRecords = records.filter((record) => record.type === 'message')
        const firstUser = messageRecords.map((record) => record.message as Record<string, unknown> | undefined)
          .find((message) => message?.role === 'user')
        const firstText = extractSessionText(firstUser?.content)
        const explicitName = records.map((record) => record.sessionName ?? (record.type === 'session_name_change' || record.type === 'session_info' ? record.name : undefined))
          .find((value): value is string => typeof value === 'string' && value.trim().length > 0)
        // Starting the desktop creates a resident session before the user
        // sends anything. Empty startup sessions are not chats and should not
        // occupy blank rows in the sidebar.
        if (!firstUser && !explicitName) continue

        out.push({
          sessionFile: full,
          sessionId: String(header.id ?? header.sessionId ?? f.replace('.jsonl', '')),
          messageCount: messageRecords.length || Number(header.messageCount ?? 0),
          workingDirectory: dir,
          mtime: st.mtimeMs,
          name: explicitName ?? sessionTitle(firstText)
        })
      } catch {
        continue
      }
    }
    out.sort((a, b) => b.mtime - a.mtime)
    return out
  }

  async resumeSession(agentId: string, sessionPath: string): Promise<unknown> {
    const agent = this.agents.get(agentId)
    if (!agent?.client) throw new Error('Agent not connected')
    const res = await agent.client.send({ type: 'switch_session', sessionPath })
    const msgs = await agent.client.send({ type: 'get_messages' }).catch(() => null)
    if (msgs) {
      agent.messages = this.visibleMessages((msgs as { messages: unknown[] }).messages ?? [])
    }
    const state = await agent.client.send({ type: 'get_state' }).catch(() => null) as Record<string, unknown> | null
    if (state) {
      agent.info.sessionId = (state.sessionId as string | null) ?? agent.info.sessionId
      agent.info.sessionName = (state.sessionName as string | null) ?? null
      agent.info.messageCount = (state.messageCount as number) ?? agent.messages.length
    }
    this.publish(agent)
    this.emitToRenderer('events', { agentId, type: 'session_resumed', payload: { sessionPath } } as PrimeEvent)
    this.refreshStats(agent)
    return res
  }

  async deleteSession(agentId: string, sessionPath: string): Promise<SessionSummary[]> {
    const agent = this.agents.get(agentId)
    if (!agent?.client) throw new Error('Agent not connected')
    const target = (await this.getSessions(agentId)).find((session) => session.sessionFile === sessionPath)
    if (!target) return this.getSessions(agentId)
    const wasActive = target.sessionId === agent.info.sessionId
    if (wasActive) await this.newSession(agentId)
    try {
      await agent.client.deleteSavedSession(sessionPath)
    } catch (error) {
      if (wasActive) await this.resumeSession(agentId, sessionPath).catch(() => {})
      throw error
    }
    return this.getSessions(agentId)
  }

  async newSession(agentId: string): Promise<unknown> {
    const agent = this.agents.get(agentId)
    if (!agent?.client) throw new Error('Agent not connected')
    const res = await agent.client.send({ type: 'new_session' })
    agent.messages = []
    const state = await agent.client.send({ type: 'get_state' }).catch(() => null) as Record<string, unknown> | null
    agent.info.sessionId = (state?.sessionId as string | null) ?? null
    agent.info.sessionName = (state?.sessionName as string | null) ?? null
    agent.info.messageCount = 0
    this.publish(agent)
    this.emitToRenderer('events', { agentId, type: 'session_started', payload: { sessionId: agent.info.sessionId } } as PrimeEvent)
    return res
  }

  async getCommands(agentId: string): Promise<unknown[]> {
    const agent = this.agents.get(agentId)
    if (agent?.commands) return agent.commands
    if (agent?.client) {
      const cmds = await agent.client.send<{ commands: unknown[] }>({ type: 'get_commands' })
      agent.commands = cmds.commands ?? []
      this.publish(agent)
      return agent.commands
    }
    return []
  }

  async setModelPref(model: string | null): Promise<void> {
    await persistModel(model)
    const s = await getState()
    for (const a of this.agents.values()) {
      if (a.client && model) {
        const [provider, id] = model.includes('/') ? model.split('/') : [null, model]
        a.client.send({ type: 'set_model', provider: provider ?? undefined, modelId: id }).catch(() => {})
      }
    }
    void s
  }

  // ---------- Fleet: schedules, heartbeats, observe, send ----------


  async listSchedules(): Promise<Record<string, ScheduleJob[]>> {
    const out: Record<string, ScheduleJob[]> = {}
    for (const a of this.agents.values()) {
      if (!a.client) continue
      try {
        const res = await a.client.send<{ jobs: unknown[] }>({ type: 'list_schedules' })
        out[a.id] = (res.jobs ?? []).map((j) => {
          const job = j as Record<string, unknown>
          const schedule = job.schedule as Record<string, unknown> | undefined
          const status = String(job.status ?? 'active') as ScheduleJob['status']
          return {
            id: String(job.id ?? ''),
            cron: String(schedule?.expression ?? job.cron ?? ''),
            prompt: String(job.prompt ?? ''),
            active: status === 'active',
            status,
            source: job.source as ScheduleJob['source'],
            runtimeKind: job.runtimeKind as ScheduleJob['runtimeKind'],
            deliveryMode: job.deliveryMode as ScheduleJob['deliveryMode'],
            activeSessionId: typeof job.activeSessionId === 'string' ? job.activeSessionId : undefined,
            sessionId: typeof job.sessionId === 'string' ? job.sessionId : undefined,
            label: typeof job.label === 'string' ? job.label : undefined,
            schedule: schedule as ScheduleJob['schedule'],
            createdAt: typeof job.createdAt === 'string' ? job.createdAt : undefined,
            updatedAt: typeof job.updatedAt === 'string' ? job.updatedAt : undefined,
            nextRunAt: typeof job.nextRunAt === 'string' ? job.nextRunAt : undefined,
            lastRunAt: typeof job.lastRunAt === 'string' ? job.lastRunAt : undefined,
            lastSkippedAt: typeof job.lastSkippedAt === 'string' ? job.lastSkippedAt : undefined,
            lastError: typeof job.lastError === 'string' ? job.lastError : undefined,
            runCount: typeof job.runCount === 'number' ? job.runCount : undefined
          } as ScheduleJob
        })
      } catch {
        out[a.id] = []
      }
    }
    return out
  }

  async addSchedule(agentId: string, schedule: string, prompt: string): Promise<unknown> {
    const agent = this.agents.get(agentId)
    if (!agent?.client) throw new Error('Agent not connected')
    return agent.client.send({ type: 'add_schedule', schedule, prompt })
  }

  async cancelSchedule(agentId: string, jobId: string): Promise<unknown> {
    const agent = this.agents.get(agentId)
    if (!agent?.client) throw new Error('Agent not connected')
    return agent.client.send({ type: 'cancel_schedule', jobId })
  }

  async getHeartbeat(agentId: string): Promise<Heartbeat | null> {
    const agent = this.agents.get(agentId)
    if (!agent?.client) return null
    const res = await agent.client.send<{ heartbeat: unknown } | null>({ type: 'get_heartbeat' }).catch(() => null)
    if (!res || !res.heartbeat) return null
    const h = res.heartbeat as Record<string, unknown>
    const schedule = h.schedule as Record<string, unknown> | undefined
    return {
      id: (h.id as string) ?? null,
      schedule: String(schedule?.expression ?? h.schedule ?? ''),
      prompt: String(h.prompt ?? ''),
      status: String(h.status ?? 'active')
    }
  }

  async setHeartbeat(agentId: string, schedule: string, prompt: string): Promise<unknown> {
    const agent = this.agents.get(agentId)
    if (!agent?.client) throw new Error('Agent not connected')
    return agent.client.send({ type: 'set_heartbeat', schedule, prompt })
  }

  async heartbeatAction(agentId: string, action: string): Promise<unknown> {
    const agent = this.agents.get(agentId)
    if (!agent?.client) throw new Error('Agent not connected')
    return agent.client.send({ type: 'update_heartbeat', action })
  }

  async observeFleet(agentId: string, activeSessionId: string): Promise<{ status: string; observed: FleetAgent }> {
    const binary = this.binary.getBinary()
    const observer = new RpcClient({ binary, cwd: homedir(), args: ['--no-session'] })
    this.observers.set(activeSessionId, observer)
    observer.on('event', (ev) => {
      if ((ev.type as string)?.startsWith('observed_')) {
        this.emitToRenderer('events', { agentId, type: 'fleet_event', payload: ev } as PrimeEvent)
      } else {
        this.emitToRenderer('events', { agentId, type: 'fleet_event', payload: ev } as PrimeEvent)
      }
    })
    await observer.start()
    await observer.send({ type: 'observe', activeSessionId })
    return { status: 'observing', observed: { id: activeSessionId, name: activeSessionId, sessionId: activeSessionId, status: 'observed', projectPath: '', observed: true, lastEvent: '', children: [] } }
  }

  async unobserve(sessionId: string): Promise<void> {
    const o = this.observers.get(sessionId)
    if (o) {
      o.send({ type: 'unobserve', activeSessionId: sessionId }).catch(() => {})
      o.kill()
      this.observers.delete(sessionId)
    }
  }

  async sendMessage(agentId: string, target: string, message: string, mode = 'auto'): Promise<unknown> {
    const agent = this.agents.get(agentId)
    if (!agent?.client) throw new Error('Agent not connected')
    return agent.client.send({ type: 'send_message', targetActiveSessionId: target, message, deliveryMode: mode })
  }

  async getSubagentTree(agentId: string): Promise<import('@shared/types').SubagentNode[]> {
    const agent = this.agents.get(agentId)
    if (!agent?.client) return []
    return agent.client.getSubagentTree()
  }

  async getSubagentMessages(agentId: string, activeSessionId: string): Promise<unknown[]> {
    const agent = this.agents.get(agentId)
    if (!agent?.client) return []
    return agent.client.getSubagentMessages(activeSessionId)
  }

  // ---------- Checkpoints (Approval & Rollback) ----------

  async createCheckpoint(agent: Agent, label: string): Promise<Checkpoint | null> {
    if (!(await isGitRepo(agent.path))) return null
    try {
      const files = await changedFiles(agent.path)
      const sha = await commitAll(agent.path, `[prime-desktop] ${label} checkpoint`)
      if (!sha) return null
      const cp: Checkpoint = {
        id: sha,
        createdAt: Date.now(),
        label,
        agentId: agent.id,
        dirtyFiles: files
      }
      agent.checkpoints.push(cp)
      if (agent.checkpoints.length > 50) agent.checkpoints.shift()
      await mkdir(CHECKPOINT_DIR, { recursive: true })
      await writeFile(join(CHECKPOINT_DIR, `${sha}.json`), JSON.stringify(cp, null, 2))
      return cp
    } catch {
      return null
    }
  }

  async listCheckpoints(agentId: string): Promise<Checkpoint[]> {
    const agent = this.agents.get(agentId)
    if (!agent) {
      try {
        if (existsSync(CHECKPOINT_DIR)) {
          const files = (await readdir(CHECKPOINT_DIR)).filter((f) => f.endsWith('.json'))
          const out: Checkpoint[] = []
          for (const f of files) {
            try {
              out.push(JSON.parse(await readFile(join(CHECKPOINT_DIR, f), 'utf8')))
            } catch {
              /* skip */
            }
          }
          out.sort((a, b) => b.createdAt - a.createdAt)
          return out.filter((c) => c.agentId === agentId)
        }
      } catch {
        /* ignore */
      }
      return []
    }
    return agent.checkpoints
  }

  async restoreCheckpoint(agentId: string, sha: string): Promise<boolean> {
    const agent = this.agents.get(agentId)
    if (!agent) throw new Error('Agent not connected')
    await checkoutCommit(agent.path, sha)
    return true
  }

  async diffFiles(agentId: string): Promise<FileDiff[]> {
    const agent = this.agents.get(agentId)
    if (!agent) throw new Error('Agent not connected')
    if (!(await isGitRepo(agent.path))) return []
    const st = await statusShort(agent.path)
    const out: FileDiff[] = []
    for (const s of st) {
      let diff = ''
      try {
        diff = await diffForFile(agent.path, s.path)
      } catch {
        diff = ''
      }
      out.push({
        path: s.path,
        status: (s.status.startsWith('D') ? 'deleted' : s.status.startsWith('A') ? 'added' : s.status.startsWith('R') ? 'renamed' : 'modified') as FileDiff['status'],
        diff
      })
    }
    return out
  }

  async gitStatus(agentId: string): Promise<GitStatus> {
    const agent = this.agents.get(agentId)
    if (!agent) throw new Error('Agent not connected')
    return readGitStatus(agent.path)
  }

  async gitFileDiff(agentId: string, path: string, staged: boolean): Promise<string> {
    const agent = this.agents.get(agentId)
    if (!agent) throw new Error('Agent not connected')
    return gitFileDiff(agent.path, path, staged)
  }

  async gitStage(agentId: string, paths: string[]): Promise<GitStatus> {
    const agent = this.agents.get(agentId)
    if (!agent) throw new Error('Agent not connected')
    await stageFiles(agent.path, paths)
    return readGitStatus(agent.path)
  }

  async gitUnstage(agentId: string, paths: string[]): Promise<GitStatus> {
    const agent = this.agents.get(agentId)
    if (!agent) throw new Error('Agent not connected')
    await unstageFiles(agent.path, paths)
    return readGitStatus(agent.path)
  }

  async gitStageAll(agentId: string): Promise<GitStatus> {
    const agent = this.agents.get(agentId)
    if (!agent) throw new Error('Agent not connected')
    await stageAll(agent.path)
    return readGitStatus(agent.path)
  }

  async gitUnstageAll(agentId: string): Promise<GitStatus> {
    const agent = this.agents.get(agentId)
    if (!agent) throw new Error('Agent not connected')
    await unstageAll(agent.path)
    return readGitStatus(agent.path)
  }

  async gitCommit(agentId: string, message: string): Promise<{ sha: string; summary: string; status: GitStatus }> {
    const agent = this.agents.get(agentId)
    if (!agent) throw new Error('Agent not connected')
    const result = await commitStaged(agent.path, message)
    return { ...result, status: await readGitStatus(agent.path) }
  }

  // ---------- Autonomy ----------

  async getAutonomy(agentId: string | null): Promise<{ config: AutonomousConfig; progress: AutonomousProgress; goal: unknown }> {
    const config = { ...(await getState()).settings.autonomous }
    const agent = agentId ? this.agents.get(agentId) : undefined
    const state = agent?.client?.running
      ? await agent.client.withConnection((connection) => connection.getState()).catch(() => null)
      : null
    const goal = state?.goal ?? null
    return {
      config,
      goal,
      progress: {
        turns: 0,
        maxTurns: config.maxTurns,
        tokens: Number((goal as Record<string, unknown> | null)?.tokensUsed ?? 0),
        maxTokens: config.maxTokens,
        seconds: Number((goal as Record<string, unknown> | null)?.timeUsedSeconds ?? 0),
        maxSeconds: config.maxSeconds,
        continuations: Number((goal as Record<string, unknown> | null)?.continuationsUsed ?? 0),
        maxContinuations: config.maxContinuations,
        active: Boolean((goal as Record<string, unknown> | null)?.active),
        gates: config.gates.map((command) => ({ command, lastResult: null, attempts: 0 }))
      }
    }
  }

  async setAutonomy(agentId: string | null, patch: Partial<AutonomousConfig>): Promise<{ config: AutonomousConfig; progress: AutonomousProgress; goal: unknown }> {
    const current = (await getState()).settings.autonomous
    const config = { ...current, ...patch }
    await setSettings({ autonomous: config })
    const agent = agentId ? this.agents.get(agentId) : undefined
    if (patch.enabled !== undefined && agent?.client?.running) {
      await agent.client.withConnection((connection) =>
        connection.prompt(`/autonomous ${patch.enabled ? 'on' : 'off'}`)
      ).catch(() => {})
    }
    return this.getAutonomy(agentId)
  }

  // ---------- Dashboard / spend ----------

  async spend(): Promise<{ points: SpendPoint[]; totals: { cost: number; tokensIn: number; tokensOut: number } }> {
    const byDay = new Map<string, SpendPoint>()
    let cost = 0
    let tIn = 0
    let tOut = 0
    for (const a of this.agents.values()) {
      await this.refreshStats(a)
      const s = a.stats as Record<string, unknown> | null
      if (!s) continue
      const tokens = (s.tokens as Record<string, number>) ?? {}
      const c = (s.cost as number) ?? 0
      cost += c
      tIn += tokens.input ?? 0
      tOut += tokens.output ?? 0
      const key = new Date().toISOString().slice(0, 10)
      const cur = byDay.get(key) ?? { date: key, cost: 0, tokensIn: 0, tokensOut: 0 }
      cur.cost += c
      cur.tokensIn += tokens.input ?? 0
      cur.tokensOut += tokens.output ?? 0
      byDay.set(key, cur)
    }
    if (existsSync(SESSION_DIR)) {
      for (const f of readdirSync(SESSION_DIR).filter((f) => f.endsWith('.jsonl'))) {
        try {
          const full = join(SESSION_DIR, f)
          const st = await stat(full)
          if (Date.now() - st.mtimeMs > 24 * 3600 * 1000) continue
          const content = await readFile(full, 'utf8')
          let usage: Record<string, number> = {}
          let day = new Date().toISOString().slice(0, 10)
          for (const line of content.split('\n').slice(-200)) {
            if (!line.trim()) continue
            try {
              const o = JSON.parse(line)
              const u = o.usage ?? o.message?.usage
              if (u?.input || u?.output) usage = u
              if (o.timestamp) day = new Date(o.timestamp).toISOString().slice(0, 10)
            } catch {
              /* skip */
            }
          }
          if (usage.input) {
            const cur = byDay.get(day) ?? { date: day, cost: 0, tokensIn: 0, tokensOut: 0 }
            cur.tokensIn += usage.input ?? 0
            cur.tokensOut += usage.output ?? 0
            byDay.set(day, cur)
          }
        } catch {
          /* skip */
        }
      }
    }
    const points = [...byDay.values()].sort((a, b) => a.date.localeCompare(b.date))
    return { points, totals: { cost, tokensIn: tIn, tokensOut: tOut } }
  }

  // ---------- Skills ----------

  async listSkills(agentId: string): Promise<SkillInfo[]> {
    const agent = this.agents.get(agentId)
    if (!agent) return []
    if (agent.skills.length === 0) {
      if (agent.client?.running) {
        try {
          const snapshot = await agent.client.withConnection((connection) => connection.getResourceSnapshot())
          const resources: SkillInfo[] = [
            ...((snapshot.skills as Record<string, unknown>[] | undefined) ?? []).map((item) => ({
              name: String(item.name ?? ''),
              description: String(item.description ?? ''),
              source: 'skill' as const,
              location: String((item.sourceInfo as Record<string, unknown> | undefined)?.scope ?? ''),
              path: String(item.filePath ?? '')
            })),
            ...((snapshot.prompts as Record<string, unknown>[] | undefined) ?? []).map((item) => ({
              name: String(item.name ?? ''),
              description: String(item.description ?? ''),
              source: 'prompt' as const,
              location: String((item.sourceInfo as Record<string, unknown> | undefined)?.scope ?? ''),
              path: String(item.filePath ?? '')
            })),
            ...((snapshot.extensions as Record<string, unknown>[] | undefined) ?? []).map((item) => ({
              name: String(item.path ?? '').split('/').pop() ?? 'extension',
              description: 'Prime Agent extension',
              source: 'extension' as const,
              location: String((item.sourceInfo as Record<string, unknown> | undefined)?.scope ?? ''),
              path: String(item.path ?? '')
            }))
          ]
          if (resources.length > 0) {
            agent.skills = resources
            return resources
          }
        } catch {
          /* fall back to slash-command metadata */
        }
      }
      const cmds = (await this.getCommands(agentId)) as { name: string; description?: string; source?: string; location?: string; path?: string }[]
      agent.skills = cmds
        .filter((c) => c.source === 'skill' || c.source === 'prompt')
        .map((c) => ({
          name: c.name,
          description: c.description ?? '',
          source: c.source === 'skill' ? 'skill' : 'prompt',
          location: c.location,
          path: c.path
        }))
      this.publish(agent)
    }
    return agent.skills
  }

  async installSkillPackage(source: string): Promise<{ ok: boolean; error?: string }> {
    const agent = [...this.agents.values()][0]
    const binary = this.binary.getBinary()
    const cwd = agent?.path ?? homedir()
    const { execFile } = await import('child_process')
    const { promisify } = await import('util')
    const exec = promisify(execFile)
    try {
      await exec(binary, ['package', 'install', source], { cwd, timeout: 120000 })
      return { ok: true }
    } catch (err) {
      return { ok: false, error: err instanceof Error ? err.message : String(err) }
    }
  }

  // ---------- Dialogs ----------

  async respondDialog(dialogId: string, value: unknown, cancelled = false): Promise<void> {
    for (const a of this.agents.values()) {
      const d = a.dialogs.get(dialogId)
      if (d) {
        const payload: Record<string, unknown> = { type: 'extension_ui_response', id: dialogId }
        if (cancelled) payload.cancelled = true
        else if (d.method === 'confirm') payload.confirmed = Boolean(value)
        else payload.value = value
        a.client?.fire(payload)
        a.dialogs.delete(dialogId)
        this.publish(a)
        return
      }
    }
  }

  async setRlmMaxDepth(agentId: string, maxDepth: number, global = true): Promise<{ maxDepth: number; source: string }> {
    const clamped = Math.max(0, Math.min(10, Math.round(maxDepth)))
    await writePrimeRlmMaxDepth(clamped)
    const agent = this.agents.get(agentId)
    if (agent?.client?.running) {
      const result = await agent.client.withConnection((connection) =>
        connection.setRlmMaxDepth(clamped, { global })
      )
      return { maxDepth: result.maxDepth, source: result.source }
    }
    return { maxDepth: clamped, source: 'global' }
  }

  async getRlmMaxDepth(agentId: string, fallback: number): Promise<{ maxDepth: number; source: string }> {
    const agent = this.agents.get(agentId)
    if (agent?.client?.running) {
      return agent.client.withConnection((connection) => connection.getRlmMaxDepthStatus())
    }
    return { maxDepth: fallback, source: 'global' }
  }

  async harnessAction(agentId: string, action: string, input: Record<string, unknown> = {}): Promise<unknown> {
    if (action === 'traces') {
      const mode = String(input.mode ?? 'status')
      if (mode === 'on' || mode === 'off') await setAgentTracesEnabled(mode === 'on')
      return { enabled: await getAgentTracesEnabled() }
    }
    if (action === 'update') {
      const connected = [...this.agents.values()]
      if (connected.some((item) => item.info.isStreaming || item.info.status === 'working')) {
        throw new Error('Wait for running agents to become idle before updating Prime Agent.')
      }
      const { execFile } = await import('child_process')
      const { promisify } = await import('util')
      const settings = (await getState()).settings
      for (const item of connected) {
        item.client?.kill()
        item.client = null
        item.info.status = 'starting'
        this.publish(item)
      }
      try {
        await this.runPrimeCommand(['shutdown', '--force', '--json'])
        const result = await promisify(execFile)(this.binary.getBinary(), ['update'], {
          timeout: 10 * 60_000,
          env: { ...process.env, ELECTRON_RUN_AS_NODE: undefined, ELECTRON_NO_ASAR: undefined }
        })
        await this.binary.check()
        return { output: `${result.stdout}\n${result.stderr}`.trim() }
      } finally {
        for (const item of connected) {
          await this.connect(item, settings).catch((error) => {
            item.info.status = 'error'
            this.toast(item, 'error', error instanceof Error ? error.message : String(error))
            this.publish(item)
          })
        }
      }
    }
    if (action === 'daemon_status' || action === 'daemon_doctor' || action === 'daemon_recover' || action === 'daemon_shutdown') {
      const args = action === 'daemon_status'
        ? ['status', '--json']
        : action === 'daemon_shutdown'
          ? ['shutdown', '--force', '--json']
          : ['doctor', ...(action === 'daemon_recover' ? ['--fix'] : []), '--json']
      const result = await this.runPrimeCommand(args)
      if (action === 'daemon_recover') {
        const settings = (await getState()).settings
        for (const item of this.agents.values()) {
          if (item.client?.running && item.info.status !== 'error' && item.info.status !== 'stopped') continue
          item.client?.kill()
          item.client = null
          item.info.status = 'starting'
          this.publish(item)
          await this.connect(item, settings).catch((error) => {
            item.info.status = 'error'
            this.toast(item, 'error', error instanceof Error ? error.message : String(error))
            this.publish(item)
          })
        }
      }
      return result
    }
    if (action === 'package') {
      const command = String(input.command ?? 'list')
      if (!['list', 'install', 'remove', 'update'].includes(command)) throw new Error('Unsupported package action')
      const source = String(input.source ?? '').trim()
      if ((command === 'install' || command === 'remove') && !source) throw new Error('Package source is required')
      const result = await this.runPrimeCommand(['package', command, ...(source ? [source] : [])], agentId)
      if (result.ok) {
        for (const running of this.agents.values()) {
          running.skills = []
          if (running.client?.running) void running.client.send({ type: 'reload' }).catch(() => {})
        }
      }
      return result
    }
    if (action === 'mcp_get') return { servers: await getMcpServers() }
    if (action === 'mcp_set') {
      const config = input.remove === true
        ? null
        : {
            url: String(input.url ?? ''),
            oauth: input.oauth !== false,
            enabled: input.enabled !== false,
            bearerTokenEnvVar: typeof input.bearerTokenEnvVar === 'string' ? input.bearerTokenEnvVar : undefined
          }
      return { servers: await setMcpServer(String(input.name ?? ''), config) }
    }
    if (action === 'trace_list') {
      const files = existsSync(SESSION_DIR)
        ? await Promise.all((await readdir(SESSION_DIR))
            .filter((name) => name.endsWith('.jsonl'))
            .map(async (name) => {
              const path = join(SESSION_DIR, name)
              const info = await stat(path)
              return { path, name, size: info.size, modifiedAt: info.mtimeMs }
            }))
        : []
      return { files: files.sort((left, right) => right.modifiedAt - left.modifiedAt).slice(0, 200) }
    }
    if (action === 'trace_preview') {
      const path = resolve(String(input.path ?? ''))
      const root = `${resolve(SESSION_DIR)}/`
      if (!path.startsWith(root) || !path.endsWith('.jsonl')) throw new Error('Trace path is outside the session directory')
      const contents = await readFile(path, 'utf8')
      return {
        path,
        size: Buffer.byteLength(contents),
        preview: contents.slice(0, 200_000),
        truncated: contents.length > 200_000
      }
    }
    const agent = this.agents.get(agentId)
    if (!agent?.client?.running) throw new Error('Unknown or disconnected agent')
    return agent.client.withConnection(
      async (connection) => {
        switch (action) {
          case 'capabilities':
            return {
              queueMutation: typeof connection.mutateQueuedMessage === 'function',
              sessionTree: typeof connection.getSessionTree === 'function',
              sideQuestions: typeof connection.startSideQuestion === 'function',
              childCancellation: typeof connection.cancelRlmChild === 'function',
              resources: typeof connection.getResourceSnapshot === 'function',
              modelCatalog: typeof connection.getModelCatalog === 'function',
              jsonlExport: typeof connection.exportToJsonl === 'function'
            }
          case 'queue':
            return {
              ...(await connection.getQueue()),
              mutationSupported: typeof connection.mutateQueuedMessage === 'function'
            }
          case 'queue_mutate': {
            if (!connection.mutateQueuedMessage) return { status: 'unsupported', ...(await connection.getQueue()) }
            const lane = input.lane === 'followUp' ? 'followUp' : 'steering'
            const index = Math.max(0, Math.floor(Number(input.index)))
            const expectedText = String(input.expectedText ?? '')
            const raw = (input.mutation ?? {}) as Record<string, unknown>
            const mutation = raw.type === 'move'
              ? { type: 'move' as const, direction: raw.direction === -1 ? -1 as const : 1 as const }
              : raw.type === 'replace'
                ? {
                    type: 'replace' as const,
                    text: String(raw.text ?? ''),
                    lane: raw.lane === 'followUp' ? 'followUp' as const : 'steering' as const
                  }
                : { type: 'delete' as const }
            const status = await connection.mutateQueuedMessage(lane, index, expectedText, mutation)
            return { status, ...(await connection.getQueue()), mutationSupported: true }
          }
          case 'queue_clear':
            return connection.clearQueue()
          case 'queue_abort_clear':
            return connection.abortAndClearQueue()
          case 'get_tree': {
            const { tree, leafId } = await connection.getSessionTree()
            return { nodes: flattenTree(tree), leafId }
          }
          case 'get_tree_full':
            return connection.getSessionTree()
          case 'navigate_tree':
            return connection.navigateTree(String(input.targetId ?? ''), {
              summarize: input.summarize !== false,
              customInstructions: typeof input.instructions === 'string' ? input.instructions : undefined,
              label: typeof input.label === 'string' ? input.label : undefined
            })
          case 'label_tree':
            await connection.setSessionEntryLabel(String(input.entryId ?? ''), typeof input.label === 'string' ? input.label : undefined)
            return connection.getSessionTree()
          case 'family': {
            const snapshot = await connection.getInitialSnapshot()
            return { children: snapshot.children ?? [] }
          }
          case 'family_cancel':
            return { cancelled: await connection.cancelRlmChild(String(input.childId ?? '')) }
          case 'system_prompt':
            return { text: await connection.getSystemPrompt() }
          case 'side_question_start': {
            const id = String(input.id ?? `desktop-side-${Date.now()}`)
            const previousTurns = Array.isArray(input.previousTurns)
              ? input.previousTurns.map((turn) => {
                  const item = turn as Record<string, unknown>
                  return { question: String(item.question ?? ''), answer: String(item.answer ?? '') }
                })
              : undefined
            await connection.startSideQuestion(id, String(input.question ?? ''), previousTurns)
            return { id, started: true }
          }
          case 'side_question_abort':
            return { aborted: await connection.abortSideQuestion(String(input.id ?? '')) }
          case 'side_question': {
            const id = `desktop-side-${Date.now()}`
            const result = new Promise<{ answer: string }>((resolve, reject) => {
              const timer = setTimeout(() => {
                off()
                reject(new Error('Side question timed out.'))
              }, 120_000)
              const off = connection.subscribe((event) => {
                if (event.type !== 'side_question_event') return
                const detail = event.event as Record<string, unknown> | undefined
                if (detail?.id !== id || detail.status === 'running') return
                clearTimeout(timer)
                off()
                if (detail.status === 'complete') resolve({ answer: String(detail.answer ?? '') })
                else reject(new Error(String(detail.errorMessage ?? `Side question ${String(detail.status)}`)))
              })
            })
            await connection.startSideQuestion(id, String(input.question ?? ''))
            return result
          }
          case 'fast': {
            const state = await connection.getSessionContext()
            const enabled = state.serviceTier === 'priority'
            await connection.setServiceTier(enabled ? 'default' : 'priority')
            return { enabled: !enabled }
          }
          case 'scoped_models':
            await connection.setScopedModels(
              Array.isArray(input.models)
                ? (input.models as Record<string, unknown>[]).map((model) => ({
                    provider: String(model.provider ?? ''),
                    modelId: String(model.modelId ?? '')
                  }))
                : []
            )
            return { saved: true }
          case 'resources':
            return connection.getResourceSnapshot()
          case 'model_catalog':
            return connection.getModelCatalog()
          case 'set_transport':
            await connection.setTransport(
              ['sse', 'websocket', 'websocket-cached'].includes(String(input.transport))
                ? String(input.transport) as 'sse' | 'websocket' | 'websocket-cached'
                : 'auto'
            )
            return { transport: String(input.transport ?? 'auto') }
          case 'goal_state':
            return { goal: (await connection.getState()).goal }
          case 'goal_command':
            await connection.prompt(String(input.command ?? '/goal status'))
            return { accepted: true }
          case 'autonomous_command':
            await connection.prompt(`/autonomous ${String(input.mode ?? 'status')}`)
            return { accepted: true }
          case 'heartbeats':
            return { heartbeats: await connection.listHeartbeats() }
          case 'heartbeat_manage':
            return connection.manageHeartbeat(
              String(input.activeSessionId ?? ''),
              String(input.jobId ?? ''),
              ['pause', 'resume'].includes(String(input.action))
                ? String(input.action) as 'pause' | 'resume'
                : 'stop'
            )
          case 'heartbeat_set':
            return connection.setHeartbeat(
              String(input.schedule ?? ''),
              String(input.prompt ?? ''),
              input.deliveryMode === 'follow_up' ? 'follow_up' : 'steer'
            )
          case 'refine':
            return connection.refine({
              instructions: typeof input.instructions === 'string' ? input.instructions : undefined,
              rollbackId: typeof input.rollbackId === 'string' ? input.rollbackId : undefined,
              global: input.global === true
            })
          case 'refinement_history': {
            const { tree } = await connection.getSessionTree()
            const history: unknown[] = []
            const visit = (nodes: DaemonTreeNode[]) => {
              for (const node of nodes) {
                if (node.entry.type === 'custom' && node.entry.customType === 'prime-agent.refinement' && node.entry.data) {
                  history.push(node.entry.data)
                }
                visit(node.children)
              }
            }
            visit(tree)
            return { history }
          }
          case 'export_jsonl':
            return { path: await connection.exportToJsonl(typeof input.outputPath === 'string' ? input.outputPath : undefined) }
          case 'reload':
            await connection.reload()
            return { reloaded: true }
          case 'import':
            return connection.importFromJsonl(String(input.path ?? ''), agent.path)
          default:
            throw new Error(`Unknown harness action: ${action}`)
        }
      }
    )
  }

  private async runPrimeCommand(args: string[], agentId?: string): Promise<{ ok: boolean; output: string; data?: unknown }> {
    const { execFile } = await import('child_process')
    const { promisify } = await import('util')
    const agent = agentId ? this.agents.get(agentId) : undefined
    try {
      const result = await promisify(execFile)(this.binary.getBinary(), args, {
        cwd: agent?.path ?? homedir(),
        timeout: 10 * 60_000,
        maxBuffer: 10 * 1024 * 1024,
        env: { ...process.env, ELECTRON_RUN_AS_NODE: undefined, ELECTRON_NO_ASAR: undefined }
      })
      const output = `${result.stdout}\n${result.stderr}`.trim()
      try {
        return { ok: true, output, data: JSON.parse(output) }
      } catch {
        return { ok: true, output }
      }
    } catch (error) {
      const detail = error as Error & { stdout?: string; stderr?: string }
      return {
        ok: false,
        output: `${detail.stdout ?? ''}\n${detail.stderr ?? ''}\n${detail.message}`.trim()
      }
    }
  }

  async exportHtml(agentId: string, outputPath?: string): Promise<string> {
    const agent = this.agents.get(agentId)
    if (!agent?.client) throw new Error('Agent not connected')
    const res = await agent.client.send<{ path?: string }>({ type: 'export_html', outputPath })
    return res.path ?? outputPath ?? ''
  }

  async shareSession(agentId: string): Promise<{ previewUrl: string; gistUrl: string }> {
    const { tmpdir } = await import('os')
    const tmpFile = join(tmpdir(), `prime-session-${Date.now()}.html`)
    await this.exportHtml(agentId, tmpFile)
    const { execFile } = await import('child_process')
    const { promisify } = await import('util')
    const exec = promisify(execFile)
    try {
      await exec('gh', ['auth', 'status'])
    } catch {
      throw new Error('GitHub CLI is not logged in. Run gh auth login first.')
    }
    let gistUrl = ''
    try {
      const out = await exec('gh', ['gist', 'create', '--public=false', tmpFile])
      gistUrl = (out.stdout || out.stderr).trim().split('\n').pop() ?? ''
    } catch (err) {
      throw new Error(err instanceof Error ? err.message : 'Failed to create gist')
    }
    const gistId = gistUrl.split('/').pop() ?? ''
    if (!gistId) throw new Error('Failed to parse gist ID from gh output')
    return {
      gistUrl,
      previewUrl: `https://pi.dev/session/${gistId}`
    }
  }

  // ---------- Cleanup ----------

  closeAgent(agentId: string): void {
    const agent = this.agents.get(agentId)
    if (agent) {
      agent.client?.kill()
      this.agents.delete(agentId)
      for (const [k, v] of this.observers) {
        if (k.startsWith(agentId)) {
          v.kill()
          this.observers.delete(k)
        }
      }
    }
  }

  shutdownAll(): void {
    for (const a of this.agents.values()) a.client?.kill()
    for (const o of this.observers.values()) o.kill()
    this.agents.clear()
    this.observers.clear()
  }

  allAgentInfos(): AgentInfo[] {
    return [...this.agents.values()].map((a) => a.info)
  }
}

function flattenTree(
  roots: DaemonTreeNode[],
  depth = 0,
  out: { entryId: string; parentId: string | null; depth: number; label: string; text: string; role: string }[] = []
): typeof out {
  for (const node of roots) {
    const message = node.entry.message as Record<string, unknown> | undefined
    const role = String(message?.role ?? node.entry.type)
    const text = extractSessionText(message?.content)
    out.push({
      entryId: node.entry.id,
      parentId: node.entry.parentId,
      depth,
      label: node.label ?? '',
      text,
      role
    })
    flattenTree(node.children, depth + 1, out)
  }
  return out
}

async function localModelCatalog(): Promise<ReturnType<typeof parseModelList>> {
  try {
    return parseModelList(JSON.parse(await readFile(MODELS_FILE, 'utf8')))
  } catch {
    return []
  }
}

function extractSessionText(content: unknown): string {
  if (typeof content === 'string') return content.trim()
  if (!Array.isArray(content)) return ''
  return content.map((block) => {
    if (!block || typeof block !== 'object') return ''
    return typeof (block as Record<string, unknown>).text === 'string' ? (block as Record<string, unknown>).text as string : ''
  }).join(' ').trim()
}

function sessionTitle(text: string): string | null {
  if (!text) return null
  const oneLine = text.replace(/\s+/g, ' ').trim()
  return oneLine.length > 58 ? `${oneLine.slice(0, 57).trimEnd()}…` : oneLine
}

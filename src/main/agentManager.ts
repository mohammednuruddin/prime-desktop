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
import { getState, setModel as persistModel } from './store'
import { getAgentTracesEnabled, setAgentTracesEnabled, writePrimeRlmMaxDepth } from './primeFiles'
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
      lastEvent: ''
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
        agent.messages = this.visibleMessages((ev.messages as unknown[] | undefined) ?? [])
        const state = (ev.state as Record<string, unknown> | undefined) ?? {}
        agent.info.sessionId = (state.sessionId as string | null) ?? agent.info.sessionId
        agent.info.sessionName = (state.sessionName as string | null) ?? null
        agent.info.isStreaming = (state.isStreaming as boolean) ?? false
        this.syncStatsFromMessages(agent)
        break
      }
      case 'session_resynced': {
        const snapshot = (ev.snapshot as Record<string, unknown> | undefined) ?? {}
        agent.messages = this.visibleMessages((snapshot.messages as unknown[] | undefined) ?? [])
        const state = (snapshot.state as Record<string, unknown> | undefined) ?? {}
        agent.info.sessionId = (state.sessionId as string | null) ?? agent.info.sessionId
        agent.info.sessionName = (state.sessionName as string | null) ?? null
        agent.info.isStreaming = (state.isStreaming as boolean) ?? false
        this.syncStatsFromMessages(agent)
        break
      }
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
          return {
            id: String(job.id ?? ''),
            cron: String(job.schedule ?? job.cron ?? ''),
            prompt: String(job.prompt ?? ''),
            active: Boolean(job.active ?? true)
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
    return {
      id: (h.id as string) ?? null,
      schedule: String(h.schedule ?? ''),
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

  private autonomyConfigs = new Map<string, AutonomousConfig>()
  private autonomyProgresses = new Map<string, AutonomousProgress>()

  private defaultAutonomyConfig(): AutonomousConfig {
    return { enabled: false, gates: [], gateRetries: 3, maxContinuations: 3, maxTurns: 12, maxTokens: 80000, maxSeconds: 1800 }
  }

  private defaultAutonomyProgress(): AutonomousProgress {
    return { turns: 0, maxTurns: 12, tokens: 0, maxTokens: 80000, seconds: 0, maxSeconds: 1800, continuations: 0, maxContinuations: 3, active: false, gates: [] }
  }

  getAutonomy(agentId: string | null): { config: AutonomousConfig; progress: AutonomousProgress } {
    const id = agentId ?? '__global__'
    return {
      config: { ...(this.autonomyConfigs.get(id) ?? this.defaultAutonomyConfig()) },
      progress: { ...(this.autonomyProgresses.get(id) ?? this.defaultAutonomyProgress()) }
    }
  }

  setAutonomy(agentId: string | null, patch: Partial<AutonomousConfig>): { config: AutonomousConfig; progress: AutonomousProgress } {
    const id = agentId ?? '__global__'
    const current = this.autonomyConfigs.get(id) ?? this.defaultAutonomyConfig()
    this.autonomyConfigs.set(id, { ...current, ...patch })
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
      const { execFile } = await import('child_process')
      const { promisify } = await import('util')
      const result = await promisify(execFile)(this.binary.getBinary(), ['update'], {
        timeout: 10 * 60_000,
        env: { ...process.env, ELECTRON_RUN_AS_NODE: undefined, ELECTRON_NO_ASAR: undefined }
      })
      return { output: `${result.stdout}\n${result.stderr}`.trim() }
    }
    const agent = this.agents.get(agentId)
    if (!agent?.client?.running) throw new Error('Unknown or disconnected agent')
    return agent.client.withConnection(
      async (connection) => {
        switch (action) {
          case 'get_tree': {
            const { tree, leafId } = await connection.getSessionTree()
            return { nodes: flattenTree(tree), leafId }
          }
          case 'navigate_tree':
            return connection.navigateTree(String(input.targetId ?? ''))
          case 'system_prompt':
            return { text: await connection.getSystemPrompt() }
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

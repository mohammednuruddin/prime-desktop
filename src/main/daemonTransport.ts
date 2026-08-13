import { EventEmitter } from 'events'
import { spawn } from 'child_process'
import { realpathSync } from 'fs'
import { dirname, join } from 'path'
import { pathToFileURL } from 'url'
import type { AppSettings } from '@shared/types'

export interface DaemonConnection {
  subscribe(listener: (event: Record<string, unknown>) => void): () => void
  getState(): Promise<Record<string, unknown>>
  getMessages(): Promise<unknown[]>
  getCommands(): Promise<unknown[]>
  getAvailableModels(): Promise<unknown[]>
  getSessionStats(): Promise<Record<string, unknown>>
  getSessionContext(): Promise<Record<string, unknown>>
  getSessionTree(): Promise<{ tree: DaemonTreeNode[]; leafId: string | null }>
  getUserMessagesForForking(): Promise<unknown[]>
  getLastAssistantText(): Promise<string | undefined>
  getSystemPrompt(): Promise<string>
  respondToExtensionUiRequest(id: string, response: Record<string, unknown>): Promise<void>
  prompt(message: string, options?: Record<string, unknown>): Promise<void>
  steer(message: string, images?: unknown[]): Promise<void>
  followUp(message: string, images?: unknown[]): Promise<void>
  abort(): Promise<void>
  executeBash(command: string): Promise<void>
  setModel(provider: string, modelId: string): Promise<unknown>
  setScopedModels(models: { provider: string; modelId: string }[]): Promise<void>
  setThinkingLevel(level: string): Promise<void>
  setServiceTier(tier: 'default' | 'priority'): Promise<void>
  setAutoCompactionEnabled(enabled: boolean): Promise<void>
  setAutoRetryEnabled(enabled: boolean): Promise<void>
  compact(instructions?: string): Promise<unknown>
  refine(options?: Record<string, unknown>): Promise<unknown>
  reload(): Promise<void>
  newSession(options?: Record<string, unknown>): Promise<unknown>
  switchSession(path: string, options?: Record<string, unknown>): Promise<unknown>
  fork(entryId: string, options?: Record<string, unknown>): Promise<unknown>
  navigateTree(targetId: string): Promise<unknown>
  importFromJsonl(path: string, cwd?: string): Promise<unknown>
  exportToHtml(path?: string): Promise<string>
  setSessionName(name: string): Promise<void>
  getRlmMaxDepthStatus(): Promise<{ maxDepth: number; source: string }>
  setRlmMaxDepth(maxDepth: number, options?: { global?: boolean }): Promise<{ maxDepth: number; source: string }>
  listCronJobs(options?: Record<string, unknown>): Promise<unknown[]>
  addCronJob(schedule: string, prompt: string): Promise<unknown>
  cancelCronJob(id: string): Promise<unknown>
  getHeartbeat(): Promise<unknown>
  setHeartbeat(schedule: string, prompt: string, deliveryMode?: string): Promise<unknown>
  updateHeartbeat(action: string): Promise<unknown>
  sendAgentMessage(target: string, message: string): Promise<unknown>
  startSideQuestion(id: string, question: string): Promise<void>
  dispose(): Promise<void>
}

export interface DaemonTreeNode {
  entry: {
    id: string
    parentId: string | null
    type: string
    message?: unknown
    timestamp?: string
  }
  label?: string
  children: DaemonTreeNode[]
}

interface DaemonClientLike {
  connect(timeoutMs?: number): Promise<void>
  request(command: Record<string, unknown>, timeoutMs?: number): Promise<Record<string, unknown>>
  close(): void
}

interface DaemonModules {
  DaemonClient: new (socketPath: string) => DaemonClientLike
  DaemonAgentConnection: {
    attach(
      client: DaemonClientLike,
      activeSessionId: string,
      options?: Record<string, unknown>
    ): Promise<DaemonConnection>
  }
}

interface Options {
  binary: string
  socketPath: string
  cwd: string
  settings: AppSettings
}

let modulesPromise: Promise<DaemonModules> | null = null

function packageRoot(binary: string): string {
  const resolved = realpathSync(binary)
  const marker = join('dist', 'bundle', 'cli.js')
  if (resolved.endsWith(marker)) return dirname(dirname(dirname(resolved)))
  throw new Error('Resident transport requires the npm installation of prime-agent.')
}

async function loadModules(binary: string): Promise<DaemonModules> {
  if (!modulesPromise) {
    modulesPromise = (async () => {
      const root = packageRoot(binary)
      const [daemon, connection] = await Promise.all([
        import(pathToFileURL(join(root, 'dist', 'modes', 'daemon', 'daemon-client.js')).href),
        import(pathToFileURL(join(root, 'dist', 'modes', 'agent-connection', 'daemon-agent-connection.js')).href)
      ])
      return {
        DaemonClient: daemon.DaemonClient,
        DaemonAgentConnection: connection.DaemonAgentConnection
      } as DaemonModules
    })()
  }
  return modulesPromise
}

function childEnv(): NodeJS.ProcessEnv {
  const env = { ...process.env }
  delete env.ELECTRON_RUN_AS_NODE
  delete env.ELECTRON_NO_ASAR
  return env
}

async function connectWithStartup(
  Client: DaemonModules['DaemonClient'],
  binary: string,
  socketPath: string
): Promise<DaemonClientLike> {
  const first = new Client(socketPath)
  try {
    await first.connect(1_500)
    return first
  } catch {
    first.close()
  }

  const daemon = spawn(binary, ['--mode', 'daemon', '--daemon-socket', socketPath], {
    detached: true,
    env: childEnv(),
    stdio: 'ignore'
  })
  daemon.unref()
  let lastError: unknown
  for (let attempt = 0; attempt < 30; attempt++) {
    await new Promise((resolve) => setTimeout(resolve, 100))
    const client = new Client(socketPath)
    try {
      await client.connect(500)
      return client
    } catch (error) {
      lastError = error
      client.close()
    }
  }
  throw lastError instanceof Error ? lastError : new Error('Prime Agent daemon did not start.')
}

function responseData(response: Record<string, unknown>, command: string): Record<string, unknown> {
  if (response.success !== true) throw new Error(String(response.error ?? `${command} failed`))
  return (response.data as Record<string, unknown> | undefined) ?? {}
}

export class DaemonTransport extends EventEmitter {
  private client: DaemonClientLike | null = null
  private connection: DaemonConnection | null = null
  private unsubscribe: (() => void) | null = null
  private exitHandlers = new Set<() => void>()
  private started = false
  private disposed = false

  constructor(private readonly options: Options) {
    super()
  }

  get running(): boolean {
    return this.started && !this.disposed && this.connection !== null
  }

  onExit(fn: () => void): void {
    this.exitHandlers.add(fn)
  }

  async start(): Promise<void> {
    if (this.started) return
    const { DaemonClient, DaemonAgentConnection } = await loadModules(this.options.binary)
    const client = await connectWithStartup(DaemonClient, this.options.binary, this.options.socketPath)
    this.client = client
    try {
      const model = this.options.settings.model
      const slash = model?.indexOf('/') ?? -1
      const config: Record<string, unknown> = {
        cwd: this.options.cwd,
        thinking: this.options.settings.thinkingLevel
      }
      if (model) {
        if (slash > 0) {
          config.provider = model.slice(0, slash)
          config.model = model.slice(slash + 1)
        } else {
          config.model = model
        }
      }
      const created = responseData(
        await client.request({ type: 'create', lifecycle: 'resident', config }, 120_000),
        'create'
      )
      const activeSessionId = String(created.activeSessionId ?? '')
      if (!activeSessionId) throw new Error('Prime Agent daemon did not return an active session.')
      const connection = await DaemonAgentConnection.attach(client, activeSessionId, {
        closeClientOnDispose: true,
        supportsExtensionUi: true,
        sendClientEnv: true
      })
      this.connection = connection
      this.unsubscribe = connection.subscribe((event) => {
        // DaemonAgentConnection wraps transcript events so it can also emit
        // connection-level lifecycle events. Desktop's message pipeline
        // consumes the transcript event shape directly.
        if (event.type === 'session_event' && event.event && typeof event.event === 'object') {
          this.emit('event', event.event)
          return
        }
        if (event.type === 'extension_ui_request' && event.request && typeof event.request === 'object') {
          const request = event.request as Record<string, unknown>
          const payload = request.payload && typeof request.payload === 'object'
            ? request.payload as Record<string, unknown>
            : {}
          this.emit('event', { type: 'extension_ui_request', ...payload, id: request.id, method: request.method })
          return
        }
        this.emit('event', event)
      })
      this.started = true
      await connection.setRlmMaxDepth(this.options.settings.rlmMaxDepth, { global: false })
    } catch (error) {
      client.close()
      this.client = null
      throw error
    }
  }

  async withConnection<T>(task: (connection: DaemonConnection) => Promise<T>): Promise<T> {
    if (!this.connection || !this.running) throw new Error('Agent is not connected')
    return task(this.connection)
  }

  async send<T = unknown>(cmd: Record<string, unknown>): Promise<T> {
    const c = this.connection
    if (!c || !this.running) throw new Error('Agent is not connected')
    const type = String(cmd.type ?? '')
    let result: unknown
    switch (type) {
      case 'get_state':
        result = await c.getState()
        break
      case 'get_messages':
        result = { messages: await c.getMessages() }
        break
      case 'get_commands':
        result = { commands: await c.getCommands() }
        break
      case 'get_available_models':
        result = { models: await c.getAvailableModels() }
        break
      case 'get_session_stats':
        result = await c.getSessionStats()
        break
      case 'prompt':
        result = await c.prompt(String(cmd.message ?? ''), {
          images: cmd.images,
          streamingBehavior: cmd.streamingBehavior
        })
        break
      case 'steer':
        result = await c.steer(String(cmd.message ?? ''), cmd.images as unknown[] | undefined)
        break
      case 'follow_up':
        result = await c.followUp(String(cmd.message ?? ''), cmd.images as unknown[] | undefined)
        break
      case 'abort':
        result = await c.abort()
        break
      case 'compact':
        result = await c.compact(cmd.customInstructions as string | undefined)
        break
      case 'refine':
        result = await c.refine({
          instructions: cmd.instructions,
          rollbackId: cmd.rollbackId,
          global: cmd.global
        })
        break
      case 'new_session':
        result = await c.newSession()
        break
      case 'switch_session':
        result = await c.switchSession(String(cmd.sessionPath ?? ''))
        break
      case 'fork': {
        const tree = await c.getSessionTree()
        const entryId = String(cmd.entryId ?? tree.leafId ?? '')
        if (!entryId) throw new Error('No message is available to fork.')
        result = await c.fork(entryId)
        break
      }
      case 'clone': {
        const tree = await c.getSessionTree()
        if (!tree.leafId) throw new Error('No message is available to clone.')
        result = await c.fork(tree.leafId)
        break
      }
      case 'set_model': {
        const modelId = String(cmd.modelId ?? '')
        let provider = String(cmd.provider ?? '')
        if (!provider) {
          const models = await c.getAvailableModels() as Record<string, unknown>[]
          provider = String(models.find((model) => model.id === modelId)?.provider ?? '')
        }
        if (!provider || !modelId) throw new Error('A provider and model are required.')
        result = await c.setModel(provider, modelId)
        break
      }
      case 'set_thinking_level':
        result = await c.setThinkingLevel(String(cmd.level ?? 'medium'))
        break
      case 'set_auto_compaction':
        result = await c.setAutoCompactionEnabled(Boolean(cmd.enabled))
        break
      case 'set_auto_retry':
        result = await c.setAutoRetryEnabled(Boolean(cmd.enabled))
        break
      case 'set_session_name':
        result = await c.setSessionName(String(cmd.name ?? ''))
        break
      case 'bash':
        result = await c.executeBash(String(cmd.command ?? ''))
        break
      case 'export_html':
        result = { path: await c.exportToHtml(cmd.outputPath as string | undefined) }
        break
      case 'get_fork_messages':
        result = { messages: await c.getUserMessagesForForking() }
        break
      case 'get_last_assistant_text':
        result = { text: await c.getLastAssistantText() }
        break
      case 'set_rlm_max_depth':
        result = await c.setRlmMaxDepth(Number(cmd.maxDepth), { global: Boolean(cmd.global) })
        break
      case 'get_rlm_max_depth_status':
        result = await c.getRlmMaxDepthStatus()
        break
      case 'list_schedules':
        result = { jobs: await c.listCronJobs({ includeInactive: true }) }
        break
      case 'add_schedule':
        result = await c.addCronJob(String(cmd.schedule ?? ''), String(cmd.prompt ?? ''))
        break
      case 'cancel_schedule':
        result = await c.cancelCronJob(String(cmd.jobId ?? ''))
        break
      case 'get_heartbeat':
        result = { heartbeat: await c.getHeartbeat() }
        break
      case 'set_heartbeat':
        result = await c.setHeartbeat(
          String(cmd.schedule ?? ''),
          String(cmd.prompt ?? ''),
          cmd.deliveryMode as string | undefined
        )
        break
      case 'update_heartbeat':
        result = await c.updateHeartbeat(String(cmd.action ?? ''))
        break
      case 'send_message':
        result = await c.sendAgentMessage(String(cmd.targetActiveSessionId ?? ''), String(cmd.message ?? ''))
        break
      case 'extension_ui_response': {
        const response = { ...cmd }
        delete response.type
        delete response.id
        result = await c.respondToExtensionUiRequest(String(cmd.id ?? ''), response)
        break
      }
      default:
        throw new Error(`Unsupported daemon transport command: ${type}`)
    }
    return result as T
  }

  fire(cmd: Record<string, unknown>): void {
    void this.send(cmd).catch((error) => this.emit('process_error', error instanceof Error ? error.message : String(error)))
  }

  stop(): void {
    void this.dispose()
  }

  kill(): void {
    void this.dispose()
  }

  private async dispose(): Promise<void> {
    if (this.disposed) return
    this.disposed = true
    this.unsubscribe?.()
    this.unsubscribe = null
    try {
      await this.connection?.dispose()
    } finally {
      this.connection = null
      this.client?.close()
      this.client = null
      for (const fn of this.exitHandlers) fn()
    }
  }
}

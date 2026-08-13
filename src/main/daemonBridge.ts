import { realpathSync } from 'fs'
import { dirname, join } from 'path'
import { pathToFileURL } from 'url'

interface AgentLike {
  info: {
    sessionId: string | null
  }
}

interface DaemonModules {
  DaemonClient: new (socketPath: string) => {
    connect(timeoutMs?: number): Promise<void>
    request(command: Record<string, unknown>, timeoutMs?: number): Promise<Record<string, unknown>>
    close(): void
  }
  DaemonAgentConnection: {
    attach(
      client: unknown,
      activeSessionId: string,
      options?: Record<string, unknown>
    ): Promise<DaemonConnection>
  }
}

interface DaemonConnection {
  subscribe(listener: (event: Record<string, unknown>) => void): () => void
  getSessionContext(): Promise<Record<string, unknown>>
  getSessionTree(): Promise<{ tree: DaemonTreeNode[]; leafId: string | null }>
  navigateTree(targetId: string): Promise<Record<string, unknown>>
  getSystemPrompt(): Promise<string>
  startSideQuestion(id: string, question: string): Promise<void>
  setScopedModels(models: { provider: string; modelId: string }[]): Promise<void>
  setServiceTier(tier: 'default' | 'priority'): Promise<void>
  reload(): Promise<void>
  getRlmMaxDepthStatus(): Promise<{ maxDepth: number; source: string }>
  setRlmMaxDepth(maxDepth: number, options?: { global?: boolean }): Promise<{ maxDepth: number; source: string }>
  importFromJsonl(path: string, cwd?: string): Promise<Record<string, unknown>>
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

let modulesPromise: Promise<DaemonModules> | null = null

function packageRoot(binary: string): string {
  const resolved = realpathSync(binary)
  const marker = `${join('dist', 'bundle', 'cli.js')}`
  if (resolved.endsWith(marker)) return dirname(dirname(dirname(resolved)))
  throw new Error('Advanced harness controls require the npm installation of prime-agent.')
}

async function loadModules(binary: string): Promise<DaemonModules> {
  if (!modulesPromise) {
    modulesPromise = (async () => {
      const root = packageRoot(binary)
      const daemonClientUrl = pathToFileURL(join(root, 'dist', 'modes', 'daemon', 'daemon-client.js')).href
      const connectionUrl = pathToFileURL(
        join(root, 'dist', 'modes', 'agent-connection', 'daemon-agent-connection.js')
      ).href
      const [daemon, connection] = await Promise.all([
        import(daemonClientUrl),
        import(connectionUrl)
      ])
      return {
        DaemonClient: daemon.DaemonClient,
        DaemonAgentConnection: connection.DaemonAgentConnection
      } as DaemonModules
    })()
  }
  return modulesPromise
}

function sessionsFromList(response: Record<string, unknown>): Record<string, unknown>[] {
  if (response.success !== true) {
    throw new Error(String(response.error ?? 'Prime Agent daemon did not list sessions.'))
  }
  const data = response.data as Record<string, unknown> | undefined
  return Array.isArray(data?.sessions) ? data.sessions as Record<string, unknown>[] : []
}

export async function withDaemonConnection<T>(
  binary: string,
  socketPath: string,
  agent: AgentLike,
  task: (connection: DaemonConnection) => Promise<T>
): Promise<T> {
  const sessionId = agent.info.sessionId
  if (!sessionId) throw new Error('This chat does not have a live Prime Agent session yet.')

  const { DaemonClient, DaemonAgentConnection } = await loadModules(binary)
  const client = new DaemonClient(socketPath)
  await client.connect()
  let connection: DaemonConnection | null = null
  try {
    const sessions = sessionsFromList(await client.request({ type: 'list', includeClientOwned: true }, 30_000))
    const summary = sessions.find((item) => String(item.sessionId ?? '') === sessionId)
    const activeSessionId = summary?.activeSessionId
    if (typeof activeSessionId !== 'string' || !activeSessionId) {
      throw new Error('The current chat is not resident in the Prime Agent daemon.')
    }
    connection = await DaemonAgentConnection.attach(client, activeSessionId, {
      closeClientOnDispose: true,
      supportsExtensionUi: false
    })
    return await task(connection)
  } finally {
    if (connection) await connection.dispose().catch(() => {})
    else client.close()
  }
}

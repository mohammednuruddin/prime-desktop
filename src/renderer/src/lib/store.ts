import type { AppSettings, BinaryState, ProjectTab, ViewId } from '@shared/types'
import { CODEX_DARK_THEME, PRIME_LIGHT_THEME } from '@shared/themes'

export interface Block {
  type: 'text' | 'thinking' | 'toolCall' | 'image' | 'subagent'
  text?: string
  thinking?: string
  id?: string
  name?: string
  arguments?: unknown
  status?: 'pending' | 'running' | 'done' | 'error'
  result?: string
  isError?: boolean
  agentName?: string
  agentId?: string
  message?: string
}

export interface RenderMessage {
  id: string
  role: 'user' | 'assistant' | 'toolResult' | 'system'
  content: string | Block[]
  model?: string
  stopReason?: string
  timestamp?: number
  streaming?: boolean
  toolCallId?: string
  isError?: boolean
}

export interface ToolExecState {
  toolCallId: string
  toolName: string
  args: Record<string, unknown> | string
  output: string
  status: 'pending' | 'running' | 'done' | 'error'
  isError?: boolean
}

export interface FleetEntry {
  id: string
  at: number
  agentId: string
  label: string
  text: string
  payload?: Record<string, unknown>
  parentText?: string
  childText?: string
  status?: 'running' | 'done' | 'error'
  ownerAgentId?: string
  ownerSessionId?: string | null
  parentAgentId?: string | null
  depth?: number
}

export interface AppState {
  ready: boolean
  binary: BinaryState | null
  tabs: ProjectTab[]
  activeTabId: string | null
  agents: Record<string, import('@shared/types').AgentInfo>
  messages: Record<string, RenderMessage[]>
  toolExecs: Record<string, Record<string, ToolExecState>>
  dialogs: Record<string, import('@shared/types').UiDialog[]>
  toasts: { id: string; kind: 'info' | 'success' | 'warning' | 'error'; text: string }[]
  view: ViewId
  settings: AppSettings
  sessions: import('@shared/types').SessionSummary[]
  spend: { points: import('@shared/types').SpendPoint[]; totals: { cost: number; tokensIn: number; tokensOut: number } } | null
  autonomy: { config: import('@shared/types').AutonomousConfig; progress: import('@shared/types').AutonomousProgress } | null
  schedules: Record<string, import('@shared/types').ScheduleJob[]>
  heartbeats: Record<string, import('@shared/types').Heartbeat | null>
  checkpoints: Record<string, import('@shared/types').Checkpoint[]>
  diffs: Record<string, import('@shared/types').FileDiff[]>
  skills: Record<string, import('@shared/types').SkillInfo[]>
  permissions: import('@shared/types').PermissionRule[]
  fleet: FleetEntry[]
  models: string[]
  activeAgentId: string | null
}

export const initialState: AppState = {
  ready: false,
  binary: null,
  tabs: [],
  activeTabId: null,
  agents: {},
  messages: {},
  toolExecs: {},
  dialogs: {},
  toasts: [],
  view: 'chat',
  settings: {
    notifications: true,
    checkpoints: true,
    dockBadge: false,
    thinkingLevel: 'medium',
    showReasoning: true,
    autoCompaction: true,
    autoRetry: true,
    model: null,
    rlmMaxDepth: 1,
    transport: 'auto',
    autonomous: {
      enabled: false,
      gates: [],
      gateRetries: 2,
      maxContinuations: 8,
      maxTurns: 30,
      maxTokens: 100000,
      maxSeconds: 3600
    },
    themeMode: 'system',
    codeThemeId: 'codex',
    lightTheme: PRIME_LIGHT_THEME,
    darkTheme: CODEX_DARK_THEME
  },
  sessions: [],
  spend: null,
  autonomy: null,
  schedules: {},
  heartbeats: {},
  checkpoints: {},
  diffs: {},
  skills: {},
  permissions: [],
  fleet: [],
  models: [],
  activeAgentId: null
}

export function agentIdForTab(state: AppState): string | null {
  if (state.activeTabId) return `agent-${state.activeTabId}`
  return null
}

export function normalizeBlocks(msg: Record<string, unknown>): RenderMessage {
  const content = msg.content as string | unknown[] | undefined
  const id = (msg.id as string) ?? `m-${String((msg.timestamp as number) ?? '')}-${String(msg.role ?? '')}`
  const base: RenderMessage = {
    id,
    role: (msg.role as RenderMessage['role']) ?? 'assistant',
    model: msg.model as string | undefined,
    stopReason: msg.stopReason as string | undefined,
    timestamp: msg.timestamp as number | undefined,
    isError: Boolean(msg.isError),
    streaming: false,
    content: '',
    toolCallId: msg.toolCallId as string | undefined
  }
  if (typeof content === 'string') {
    const child = parseChildMessage(content, msg.details)
    base.content = child ? [child] : content
    return base
  }
  if (Array.isArray(content)) {
    const blocks: Block[] = content.map((c) => {
      const b = c as Record<string, unknown>
      switch (b.type) {
        case 'text':
          return parseChildMessage(String(b.text ?? ''), b.details) ?? { type: 'text', text: String(b.text ?? '') }
        case 'thinking':
          return { type: 'thinking', thinking: String(b.thinking ?? '') }
        case 'toolCall': {
          const tool: Block = {
            type: 'toolCall',
            id: b.id as string,
            name: b.name as string,
            arguments: b.arguments,
            status: (b.status as Block['status']) ?? 'done',
            result: typeof b.result === 'string' ? b.result : undefined,
            isError: Boolean(b.isError)
          }
          return tool
        }
        case 'toolResult': {
          return {
            type: 'toolCall',
            id: b.toolCallId as string,
            name: (b.toolName as string) ?? 'tool',
            result: extractText(b.content),
            status: b.isError ? 'error' : 'done',
            isError: Boolean(b.isError)
          }
        }
        case 'image':
          return { type: 'image' }
        default:
          return { type: 'text', text: '' }
      }
    })
    base.content = blocks
    return base
  }
  base.content = ''
  return base
}

function toolCallIdOf(payload: Record<string, unknown>): string {
  const nested = payload.toolCall
  if (nested && typeof nested === 'object' && 'id' in nested) {
    return String((nested as { id?: unknown }).id ?? '')
  }
  return String(payload.toolCallId ?? payload.id ?? '')
}

export function patchToolExecs(
  prev: Record<string, ToolExecState>,
  event: 'start' | 'update' | 'end',
  payload: Record<string, unknown>
): Record<string, ToolExecState> {
  const id = toolCallIdOf(payload)
  if (!id) return prev
  const cur = prev[id]
  const alreadyDone = cur?.status === 'done' || cur?.status === 'error'
  if (event === 'start') {
    if (alreadyDone) return prev
    return {
      ...prev,
      [id]: {
        toolCallId: id,
        toolName: String(payload.toolName ?? cur?.toolName ?? 'tool'),
        args: (payload.args as Record<string, unknown>) ?? cur?.args ?? {},
        output: cur?.output ?? '',
        status: 'running'
      }
    }
  }
  const partial = payload.partialResult as { content?: unknown } | undefined
  const result = payload.result as { content?: unknown } | undefined
  const output = extractText((event === 'end' ? result?.content : partial?.content) ?? payload.content)
  if (event === 'update') {
    if (alreadyDone) return prev
    return {
      ...prev,
      [id]: {
        toolCallId: id,
        toolName: String(payload.toolName ?? cur?.toolName ?? 'tool'),
        args: (payload.args as Record<string, unknown>) ?? cur?.args ?? {},
        output: output || cur?.output || '',
        status: 'running'
      }
    }
  }
  return {
    ...prev,
    [id]: {
      toolCallId: id,
      toolName: String(payload.toolName ?? cur?.toolName ?? 'tool'),
      args: (payload.args as Record<string, unknown>) ?? cur?.args ?? {},
      output: output || cur?.output || '',
      status: payload.isError ? 'error' : 'done',
      isError: Boolean(payload.isError)
    }
  }
}

export function finishToolExecs(
  prev: Record<string, ToolExecState>,
  results?: Record<string, unknown>[]
): Record<string, ToolExecState> {
  let next = prev
  if (results) {
    for (const result of results) {
      next = patchToolExecs(next, 'end', result)
    }
  }
  let changed = next !== prev
  const copy = changed ? { ...next } : { ...prev }
  for (const [id, exec] of Object.entries(copy)) {
    if (exec.status === 'running' || exec.status === 'pending') {
      copy[id] = { ...exec, status: 'done' }
      changed = true
    }
  }
  return changed ? copy : prev
}

export function extractText(content: unknown): string {
  if (typeof content === 'string') return content
  if (Array.isArray(content)) {
    return content
      .map((c) => {
        const b = c as Record<string, unknown>
        if (typeof b.text === 'string') return b.text
        return ''
      })
      .join('\n')
  }
  return ''
}

export function mergeMessage(prev: RenderMessage[], msg: Record<string, unknown>): RenderMessage[] {
  const norm = normalizeBlocks(msg)
  if (norm.role === 'toolResult' && norm.toolCallId) {
    const copy = [...prev]
    for (let i = copy.length - 1; i >= 0; i--) {
      const content = copy[i].content
      if (!Array.isArray(content)) continue
      const blockIndex = content.findIndex((block) => block.type === 'toolCall' && block.id === norm.toolCallId)
      if (blockIndex < 0) continue
      const result = extractText(msg.content)
      const blocks = [...content]
      blocks[blockIndex] = { ...blocks[blockIndex], result, status: msg.isError ? 'error' : 'done', isError: Boolean(msg.isError) }
      copy[i] = { ...copy[i], content: blocks }
      return copy
    }
  }
  const idx = prev.findIndex((m) => m.id === norm.id)
  if (idx === -1) return [...prev, norm]
  const copy = [...prev]
  copy[idx] = norm
  return copy
}

function parseChildMessage(text: string, rawDetails: unknown): Block | null {
  const details = rawDetails && typeof rawDetails === 'object' ? rawDetails as Record<string, unknown> : null
  const from = details?.from && typeof details.from === 'object' ? details.from as Record<string, unknown> : null
  if (details?.fromRelationship === 'child' && typeof details.message === 'string') {
    return {
      type: 'subagent',
      agentName: String(from?.sessionName ?? 'Subagent'),
      agentId: String(from?.sessionId ?? details.id ?? ''),
      message: details.message
    }
  }
  const match = text.match(/^\[from child:([^\]]+)]\n[\s\S]*?\n\n([\s\S]+)$/)
  if (!match) return null
  return { type: 'subagent', agentName: match[1], agentId: match[1], message: match[2].trim() }
}

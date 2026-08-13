import type { ViewId } from './types'

export interface SlashCommandDef {
  name: string
  description: string
  argumentHint?: string
  takesArgument?: boolean
  aliases?: string[]
}

export const BUILTIN_SLASH_COMMANDS: SlashCommandDef[] = [
  { name: 'settings', description: 'Open settings' },
  { name: 'model', description: 'Switch models', argumentHint: '[search]', takesArgument: true },
  { name: 'effort', description: 'Set reasoning / thinking level', argumentHint: '[level]', takesArgument: true, aliases: ['thinking'] },
  { name: 'fast', description: 'Toggle OpenAI Fast mode' },
  { name: 'scoped-models', description: 'Limit models available for cycling' },
  { name: 'export', description: 'Export this session as HTML', argumentHint: '[path]', takesArgument: true },
  { name: 'import', description: 'Resume a session from a JSONL file', argumentHint: '<path.jsonl>', takesArgument: true },
  { name: 'share', description: 'Share session as a private GitHub gist' },
  { name: 'copy', description: 'Copy the last assistant message' },
  { name: 'btw', description: 'Ask a side question', argumentHint: '<question>', takesArgument: true, aliases: ['side'] },
  { name: 'name', description: 'Set or show the session display name', argumentHint: '[name]', takesArgument: true, aliases: ['rename'] },
  { name: 'session', description: 'Show session file, ID, and message counts' },
  { name: 'system-prompt', description: 'Show the system prompt sent to the model' },
  { name: 'logs', description: 'Reveal Prime Agent logs' },
  { name: 'traces', description: 'Preview or configure session traces', argumentHint: '[status|on|off]', takesArgument: true },
  { name: 'context', description: 'Show token, cost, and context usage', aliases: ['usage'] },
  { name: 'changelog', description: 'Show recent Prime Agent changes' },
  { name: 'update', description: 'How to update Prime Agent', argumentHint: '[source|--self]', takesArgument: true },
  { name: 'hotkeys', description: 'Show keyboard shortcuts' },
  { name: 'fork', description: 'Fork from a previous user message' },
  { name: 'clone', description: 'Duplicate this session at the current position' },
  { name: 'tree', description: 'Jump to another point in the session tree' },
  { name: 'login', description: 'Add a provider API key or open TUI login' },
  { name: 'logout', description: 'Remove a stored provider credential' },
  { name: 'mcp', description: 'Open MCP / skills connections', argumentHint: '[list|login <name>]', takesArgument: true },
  { name: 'new', description: 'Start a new session', argumentHint: '[prompt]', takesArgument: true, aliases: ['clear'] },
  { name: 'compact', description: 'Compact context to save tokens', argumentHint: '[instructions]', takesArgument: true },
  { name: 'refine', description: 'Refine continual harness state', argumentHint: '[instructions|rollback <id>]', takesArgument: true },
  { name: 'goal', description: 'Set, pause, resume, or clear a persistent goal', argumentHint: '[objective|status|pause|resume|clear]', takesArgument: true },
  { name: 'autonomous', description: 'Toggle autonomous continuation', argumentHint: '[on|off|status]', takesArgument: true },
  { name: 'rlm-max-depth', description: 'Set recursive subagent depth (0–10)', argumentHint: '[<int> [--global]]', takesArgument: true },
  { name: 'heartbeat', description: 'Set or control a persistent heartbeat', argumentHint: '[status|pause|resume|stop|<instruction>]', takesArgument: true },
  { name: 'heartbeats', description: 'View all heartbeats and schedules' },
  { name: 'reload', description: 'Reload commands, skills, and prompts' },
  { name: 'fullscreen', description: 'Toggle fullscreen', argumentHint: '[on|off]', takesArgument: true },
  { name: 'quit', description: 'Quit Prime Desktop' },
  { name: 'resume', description: 'Resume a previous session' }
]

const ALIAS_TO_NAME = new Map<string, string>()
const BY_NAME = new Map<string, SlashCommandDef>()
for (const command of BUILTIN_SLASH_COMMANDS) {
  BY_NAME.set(command.name, command)
  for (const alias of command.aliases ?? []) ALIAS_TO_NAME.set(alias, command.name)
}

export function resolveSlashName(name: string): string {
  return ALIAS_TO_NAME.get(name) ?? name
}

export function getSlashCommand(name: string): SlashCommandDef | undefined {
  return BY_NAME.get(resolveSlashName(name))
}

export function isBuiltinSlash(name: string): boolean {
  return BY_NAME.has(name) || ALIAS_TO_NAME.has(name)
}

export function parseSlash(text: string): { name: string; args: string } | null {
  if (!text.startsWith('/')) return null
  const match = /^\/(\S+)(?:\s+([\s\S]*))?$/.exec(text)
  if (!match) return null
  return { name: match[1], args: (match[2] ?? '').trim() }
}

export type SlashOverlayId =
  | 'fork'
  | 'tree'
  | 'login'
  | 'logout'
  | 'name'
  | 'resume'
  | 'heartbeat'
  | 'session'
  | 'usage'
  | 'hotkeys'
  | 'changelog'
  | 'system-prompt'
  | 'model'
  | 'effort'
  | 'depth'
  | 'heartbeat'
  | 'scoped-models'

export type SlashDispatch =
  | { action: 'prompt'; message: string; toast?: string }
  | { action: 'rpc'; command: Record<string, unknown>; reload?: boolean; toast?: string }
  | { action: 'navigate'; view: ViewId; toast?: string }
  | { action: 'overlay'; overlay: SlashOverlayId; args: string }
  | { action: 'copy' }
  | { action: 'quit' }
  | { action: 'new-session'; prompt?: string }
  | { action: 'reload'; toast?: string }
  | { action: 'fullscreen'; mode?: 'on' | 'off' | 'toggle' }
  | { action: 'export'; path?: string }
  | { action: 'share' }
  | { action: 'compact'; instructions?: string }
  | { action: 'refine'; instructions?: string; rollbackId?: string; global?: boolean }
  | { action: 'clone' }
  | { action: 'name'; name: string }
  | { action: 'depth'; value: number; global?: boolean }
  | { action: 'heartbeat-action'; verb: 'status' | 'pause' | 'resume' | 'stop' | 'clear' | 'set'; args: string }
  | { action: 'logs' }
  | { action: 'import'; path?: string }
  | { action: 'btw'; question: string }
  | { action: 'fast' }
  | { action: 'update' }
  | { action: 'notice'; text: string }
  | { action: 'traces'; args: string }
  | { action: 'pass-through'; message: string }

function parseRefine(args: string): Extract<SlashDispatch, { action: 'refine' }> {
  let rest = args.trim()
  let global = false
  if (/^--global(?=\s|$)/.test(rest)) {
    global = true
    rest = rest.replace(/^--global(?=\s|$)/, '').trim()
  }
  const rollback = /^rollback\s+(\S+)/.exec(rest)
  if (rollback) return { action: 'refine', rollbackId: rollback[1], global }
  return { action: 'refine', instructions: rest || undefined, global }
}

export function dispatchSlash(raw: string): SlashDispatch | null {
  const parsed = parseSlash(raw.trim())
  if (!parsed) return null
  const name = resolveSlashName(parsed.name)
  const args = parsed.args
  if (!isBuiltinSlash(parsed.name)) return { action: 'pass-through', message: raw }

  switch (name) {
    case 'settings':
      return { action: 'navigate', view: 'settings' }
    case 'model':
      return { action: 'overlay', overlay: 'model', args }
    case 'effort':
      return { action: 'overlay', overlay: 'effort', args }
    case 'fast':
      return { action: 'fast' }
    case 'scoped-models':
      return { action: 'overlay', overlay: 'scoped-models', args }
    case 'export':
      return { action: 'export', path: args || undefined }
    case 'import':
      return { action: 'import', path: args || undefined }
    case 'share':
      return { action: 'share' }
    case 'copy':
      return { action: 'copy' }
    case 'btw':
      return args ? { action: 'btw', question: args } : { action: 'notice', text: 'Usage: /btw <question>' }
    case 'name':
      return args ? { action: 'name', name: args } : { action: 'overlay', overlay: 'name', args: '' }
    case 'session':
      return { action: 'overlay', overlay: 'session', args: '' }
    case 'system-prompt':
      return { action: 'overlay', overlay: 'system-prompt', args: '' }
    case 'logs':
      return { action: 'logs' }
    case 'traces':
      return { action: 'traces', args }
    case 'context':
      return { action: 'overlay', overlay: 'usage', args: '' }
    case 'changelog':
      return { action: 'overlay', overlay: 'changelog', args: '' }
    case 'update':
      return { action: 'update' }
    case 'hotkeys':
      return { action: 'overlay', overlay: 'hotkeys', args: '' }
    case 'fork':
      return { action: 'overlay', overlay: 'fork', args }
    case 'clone':
      return { action: 'clone' }
    case 'tree':
      return { action: 'overlay', overlay: 'tree', args }
    case 'login':
      return { action: 'overlay', overlay: 'login', args }
    case 'logout':
      return { action: 'overlay', overlay: 'logout', args }
    case 'mcp':
      return { action: 'navigate', view: 'skills', toast: 'Skills & MCP connections' }
    case 'new':
      return { action: 'new-session', prompt: args || undefined }
    case 'compact':
      return { action: 'compact', instructions: args || undefined }
    case 'refine':
      return parseRefine(args)
    case 'goal':
      return { action: 'prompt', message: `/goal${args ? ` ${args}` : ''}` }
    case 'autonomous':
      return { action: 'prompt', message: `/autonomous${args ? ` ${args}` : ' status'}` }
    case 'rlm-max-depth': {
      if (!args) return { action: 'overlay', overlay: 'depth', args: '' }
      const tokens = args.split(/\s+/)
      const global = tokens.includes('--global')
      const num = tokens.find((t) => /^\d+$/.test(t))
      if (!num) return { action: 'overlay', overlay: 'depth', args }
      return { action: 'depth', value: Number(num), global }
    }
    case 'heartbeat': {
      const verb = args.split(/\s+/)[0]?.toLowerCase() ?? ''
      if (!args || verb === 'status') return { action: 'heartbeat-action', verb: 'status', args }
      if (verb === 'pause' || verb === 'resume' || verb === 'stop' || verb === 'clear') {
        return { action: 'heartbeat-action', verb, args }
      }
      return { action: 'heartbeat-action', verb: 'set', args }
    }
    case 'heartbeats':
      return { action: 'navigate', view: 'fleet', toast: 'Automations' }
    case 'reload':
      return { action: 'reload', toast: 'Reloaded commands and skills' }
    case 'fullscreen':
      return { action: 'fullscreen', mode: args === 'on' ? 'on' : args === 'off' ? 'off' : 'toggle' }
    case 'quit':
      return { action: 'quit' }
    case 'resume':
      return { action: 'overlay', overlay: 'resume', args }
    default:
      return { action: 'pass-through', message: raw }
  }
}

export const HOTKEYS: { keys: string; action: string }[] = [
  { keys: 'Enter', action: 'Send, or steer while the agent is working' },
  { keys: 'Shift+Enter', action: 'New line' },
  { keys: 'Escape', action: 'Close palettes without interrupting' },
  { keys: '/', action: 'Open slash commands' },
  { keys: '!command', action: 'Run a shell command and send the output' },
  { keys: '⌘O', action: 'Open a project folder' },
  { keys: '⌘N', action: 'New chat (sidebar)' },
  { keys: '⌘Q', action: 'Quit' }
]

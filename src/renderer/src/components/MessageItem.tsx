import { useState, useMemo } from 'react'
import type { Block, FleetEntry, RenderMessage, ToolExecState } from '../lib/store'
import SubagentMark from './SubagentMark'

const USER_COLLAPSE_THRESHOLD = 280

interface Props {
  message: RenderMessage
  toolExecs: Record<string, ToolExecState>
  onOpenSubagent?: (entry: FleetEntry) => void
}

/* ─── Helpers ──────────────────────────────────────────── */

type ToolCategory = 'file-edit' | 'file-create' | 'file-read' | 'shell' | 'search' | 'other'

function codeFromArgs(args: unknown): string {
  if (typeof args === 'string') return args
  if (!args || typeof args !== 'object') return ''
  return String((args as Record<string, unknown>).code ?? '')
}

function pathsInCode(code: string): string[] {
  return [...code.matchAll(/['"]((?:\/|\.\/|\.\.\/)?[^'"]+\.[A-Za-z0-9]{1,8})['"]/g)].map((m) => m[1])
}

function basename(path: string): string {
  const parts = path.split('/')
  return parts[parts.length - 1] || path
}

function classifyIpython(code: string): ToolCategory {
  const c = code.trim()
  if (!c) return 'other'
  if (/\b(?:await\s+)?rlm\s*\(/.test(c)) return 'other'
  if (/subprocess|os\.system|Popen|get_ipython\(\)\.system|\bshell\s*\(|run_terminal|^\s*!/m.test(c)) return 'shell'
  if (/write_text|write_bytes|open\([^)]*['"]w|apply_patch|search_replace|write_file|Path\([^)]+\)\.write/.test(c)) return 'file-edit'
  if (/read_text|read_bytes|read_file|open\(|Path\(/.test(c) && pathsInCode(c).length > 0) return 'file-read'
  if (/web_search|httpx|requests\.|urllib|duckduckgo|search_web/.test(c)) return 'search'
  return 'shell'
}

function classifyTool(name: string, args: unknown): ToolCategory {
  const n = name.toLowerCase()
  if (n === 'ipython' || n === 'python' || n === 'repl' || n === 'execute') return classifyIpython(codeFromArgs(args))
  if (n.includes('write') || n.includes('edit') || n.includes('patch') || n.includes('apply_diff') || n.includes('replace') || n.includes('multi_replace')) return 'file-edit'
  if (n.includes('create') || n.includes('new_file')) return 'file-create'
  if (n.includes('read') || n.includes('view') || n.includes('cat')) return 'file-read'
  if (n.includes('bash') || n.includes('cmd') || n.includes('exec') || n.includes('shell') || n.includes('run')) return 'shell'
  if (n.includes('search') || n.includes('web') || n.includes('grep') || n.includes('find')) return 'search'
  return 'other'
}

function extractFilename(args: unknown): string {
  if (!args || typeof args !== 'object') return ''
  const r = args as Record<string, unknown>
  const raw = String(r.TargetFile ?? r.target_file ?? r.path ?? r.file ?? r.AbsolutePath ?? r.absolute_path ?? r.SearchPath ?? '')
  if (raw) {
    const parts = raw.split('/')
    return parts[parts.length - 1] || raw
  }
  const paths = pathsInCode(codeFromArgs(args))
  return paths.length ? basename(paths[paths.length - 1]) : ''
}

function extractFullPath(args: unknown): string {
  if (!args || typeof args !== 'object') return ''
  const r = args as Record<string, unknown>
  const raw = String(r.TargetFile ?? r.target_file ?? r.path ?? r.file ?? r.AbsolutePath ?? r.absolute_path ?? r.SearchPath ?? '')
  if (raw) return raw
  const paths = pathsInCode(codeFromArgs(args))
  return paths[paths.length - 1] ?? ''
}

function extractCommand(args: unknown): string {
  if (!args || typeof args !== 'object') return ''
  const r = args as Record<string, unknown>
  const direct = String(r.command ?? r.CommandLine ?? r.cmd ?? '')
  if (direct) return direct
  const code = codeFromArgs(args)
  const list = code.match(/subprocess\.run\(\s*\[([^\]]+)\]/)
  if (list) return list[1].replace(/['"]/g, '').replace(/,/g, ' ').replace(/\s+/g, ' ').trim()
  const sys = code.match(/os\.system\(\s*['"]([^'"]+)/)
  if (sys) return sys[1]
  const bang = code.match(/^\s*!\s*(.+)$/m)
  if (bang) return bang[1].trim()
  const first = code.split('\n').map((l) => l.trim()).find(Boolean) ?? ''
  return first.length > 72 ? first.slice(0, 71) + '…' : first
}

function extractQuery(args: unknown): string {
  if (!args || typeof args !== 'object') return ''
  const r = args as Record<string, unknown>
  const direct = String(r.query ?? r.Query ?? r.search ?? '')
  if (direct) return direct
  const code = codeFromArgs(args)
  return code.match(/(?:search|query)\(\s*['"]([^'"]+)/i)?.[1]
    ?? code.match(/['"]([^'"]{3,80})['"]/)?.[1]
    ?? ''
}

/** Parse diff stats from tool output (+N -N) */
function parseDiffStats(output: string): { added: number; removed: number } | null {
  const match = output.match(/\+(\d+)\s+-(\d+)/)
  if (match) return { added: parseInt(match[1]), removed: parseInt(match[2]) }
  const lines = output.split('\n')
  let added = 0, removed = 0
  for (const line of lines) {
    if (line.startsWith('+') && !line.startsWith('+++')) added++
    if (line.startsWith('-') && !line.startsWith('---')) removed++
  }
  if (added || removed) return { added, removed }
  return null
}



/* ─── Icons ────────────────────────────────────────────── */

function PencilIcon(): JSX.Element {
  return (
    <svg viewBox="0 0 24 24" width="14" height="14" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
      <path d="M11 4H4a2 2 0 00-2 2v14a2 2 0 002 2h14a2 2 0 002-2v-7" />
      <path d="M18.5 2.5a2.121 2.121 0 013 3L12 15l-4 1 1-4 9.5-9.5z" />
    </svg>
  )
}

function TerminalIcon(): JSX.Element {
  return (
    <svg viewBox="0 0 24 24" width="14" height="14" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
      <polyline points="4 17 10 11 4 5" />
      <line x1="12" y1="19" x2="20" y2="19" />
    </svg>
  )
}

function GlobeIcon(): JSX.Element {
  return (
    <svg viewBox="0 0 24 24" width="14" height="14" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
      <circle cx="12" cy="12" r="10" />
      <line x1="2" y1="12" x2="22" y2="12" />
      <path d="M12 2a15.3 15.3 0 014 10 15.3 15.3 0 01-4 10 15.3 15.3 0 01-4-10 15.3 15.3 0 014-10z" />
    </svg>
  )
}

function FileIcon(): JSX.Element {
  return (
    <svg viewBox="0 0 24 24" width="14" height="14" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
      <path d="M13 2H6a2 2 0 00-2 2v16a2 2 0 002 2h12a2 2 0 002-2V9z" />
      <polyline points="13 2 13 9 20 9" />
    </svg>
  )
}

function ToolIcon(): JSX.Element {
  return (
    <svg viewBox="0 0 24 24" width="14" height="14" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
      <path d="M14.7 6.3a1 1 0 000 1.4l1.6 1.6a1 1 0 001.4 0l3.77-3.77a6 6 0 01-7.94 7.94l-6.91 6.91a2.12 2.12 0 01-3-3l6.91-6.91a6 6 0 017.94-7.94l-3.76 3.76z" />
    </svg>
  )
}

function CopyIcon(): JSX.Element {
  return (
    <svg viewBox="0 0 24 24" width="14" height="14" fill="none" stroke="currentColor" strokeWidth="1.5">
      <rect x="9" y="9" width="13" height="13" rx="2" ry="2" />
      <path d="M5 15H4a2 2 0 01-2-2V4a2 2 0 012-2h9a2 2 0 012 2v1" />
    </svg>
  )
}

function ChevronIcon({ open }: { open: boolean }): JSX.Element {
  return (
    <svg viewBox="0 0 24 24" width="12" height="12" fill="none" stroke="currentColor" strokeWidth="2" style={{ transform: open ? 'rotate(180deg)' : 'rotate(0deg)', transition: 'transform 0.15s ease' }}>
      <polyline points="6 9 12 15 18 9" />
    </svg>
  )
}

function SpinnerIcon(): JSX.Element {
  return <span className="tool-inline-spin" />
}

function isSubagentTool(name: string): boolean {
  const value = name.toLowerCase()
  return value.includes('spawn_agent') || value.includes('subagent') || value.includes('delegate')
}

function isRlmSpawn(exec: ToolExecState): boolean {
  if (isSubagentTool(exec.toolName)) return true
  if (exec.toolName.toLowerCase() !== 'ipython' || typeof exec.args !== 'object' || !exec.args) return false
  const code = String((exec.args as Record<string, unknown>).code ?? '')
  return /\b(?:await\s+)?rlm\s*\(/.test(code)
}

function subagentEntry(exec: ToolExecState): FleetEntry {
  const args = typeof exec.args === 'object' && exec.args ? exec.args : {}
  const code = String(args.code ?? '')
  const codeName = code.match(/\bname\s*=\s*['"]([^'"]+)['"]/)?.[1]
  const resultName = exec.output.match(/\bname=['"]([^'"]+)['"]/)?.[1]
  const resultId = exec.output.match(/rlm_child_id=['"]([^'"]+)['"]/)?.[1]
  const prompt = code.match(/\brlm\s*\(\s*(['"])([\s\S]*?)\1/)?.[2]
  const name = String(args.name ?? args.task_name ?? args.taskName ?? args.agent_name ?? codeName ?? resultName ?? 'Subagent')
  return {
    id: exec.toolCallId,
    at: Date.now(),
    agentId: String(args.agent_id ?? args.agentId ?? resultId ?? exec.toolCallId),
    label: name,
    text: String(args.task ?? args.prompt ?? args.message ?? prompt ?? ''),
    parentText: String(args.task ?? args.prompt ?? args.message ?? prompt ?? ''),
    childText: '',
    status: exec.status === 'error' ? 'error' : 'running',
    payload: { ...args, output: exec.output, status: exec.status }
  }
}

/* ─── DiffStat component ──────────────────────────────── */

function DiffStat({ added, removed }: { added: number; removed: number }): JSX.Element {
  return (
    <span className="diff-stat">
      {added > 0 && <span className="diff-added">+{added}</span>}
      {removed > 0 && <span className="diff-removed">{'\u00a0'}-{removed}</span>}
    </span>
  )
}

/* ─── Tool rendering components ───────────────────────── */

interface ToolDisplayInfo {
  block: Block
  exec: ToolExecState
  category: 'file-edit' | 'file-create' | 'file-read' | 'shell' | 'search' | 'other'
}

function InlineToolItem({ info, isOpen, onToggle }: { info: ToolDisplayInfo; isOpen: boolean; onToggle: () => void }): JSX.Element {
  const { exec, category } = info
  const isRunning = exec.status === 'running'
  const isError = exec.status === 'error' || exec.isError

  switch (category) {
    case 'file-edit':
    case 'file-create': {
      const filename = extractFilename(exec.args)
      const stats = exec.output ? parseDiffStats(exec.output) : null
      const verb = category === 'file-create' ? 'Created' : 'Edited'
      return (
        <div className={`tool-inline ${isError ? 'error' : ''}`}>
          <div className="tool-inline-row" onClick={onToggle}>
            <div className="tool-inline-left">
              {isRunning ? <SpinnerIcon /> : <PencilIcon />}
              <span className="tool-inline-verb">{verb}</span>
              <span className="tool-inline-filename">{filename || exec.toolName}</span>
              {stats && <DiffStat added={stats.added} removed={stats.removed} />}
              {isError && <span className="tool-inline-error-dot">●</span>}
            </div>
            {exec.output && <ChevronIcon open={isOpen} />}
          </div>
          {isOpen && exec.output && (
            <div className="tool-inline-detail">
              <pre className="tool-inline-output">{exec.output}</pre>
            </div>
          )}
        </div>
      )
    }

    case 'file-read': {
      const filename = extractFilename(exec.args)
      return (
        <div className={`tool-inline ${isError ? 'error' : ''}`}>
          <div className="tool-inline-row" onClick={onToggle}>
            <div className="tool-inline-left">
              {isRunning ? <SpinnerIcon /> : <FileIcon />}
              <span className="tool-inline-verb">Read</span>
              <span className="tool-inline-filename">{filename || exec.toolName}</span>
            </div>
            {exec.output && <ChevronIcon open={isOpen} />}
          </div>
          {isOpen && exec.output && (
            <div className="tool-inline-detail">
              <pre className="tool-inline-output">{exec.output.slice(0, 2000)}</pre>
            </div>
          )}
        </div>
      )
    }

    case 'shell': {
      const cmd = extractCommand(exec.args)
      const truncCmd = cmd.length > 60 ? cmd.slice(0, 60) + '\u2026' : cmd
      return (
        <div className={`tool-inline ${isError ? 'error' : ''}`}>
          <div className="tool-inline-row" onClick={onToggle}>
            <div className="tool-inline-left">
              {isRunning ? <SpinnerIcon /> : <TerminalIcon />}
              <span className="tool-inline-verb">Ran</span>
              <code className="tool-inline-cmd">{truncCmd || exec.toolName}</code>
              {isError && <span className="tool-inline-error-dot">●</span>}
            </div>
            <ChevronIcon open={isOpen} />
          </div>
          {isOpen && (
            <div className="tool-inline-detail">
              {cmd.length > 60 && <code className="tool-inline-full-cmd">{cmd}</code>}
              {exec.output && <pre className="tool-inline-output">{exec.output}</pre>}
            </div>
          )}
        </div>
      )
    }

    case 'search': {
      const query = extractQuery(exec.args)
      return (
        <div className={`tool-inline ${isError ? 'error' : ''}`}>
          <div className="tool-inline-row" onClick={onToggle}>
            <div className="tool-inline-left">
              {isRunning ? <SpinnerIcon /> : <GlobeIcon />}
              <span className="tool-inline-verb">Searched the web for</span>
              <span className="tool-inline-query">{query}</span>
            </div>
            {exec.output && <ChevronIcon open={isOpen} />}
          </div>
          {isOpen && exec.output && (
            <div className="tool-inline-detail">
              <pre className="tool-inline-output">{exec.output.slice(0, 3000)}</pre>
            </div>
          )}
        </div>
      )
    }

    default: {
      const filename = extractFilename(exec.args) || extractCommand(exec.args) || extractQuery(exec.args)
      return (
        <div className={`tool-inline ${isError ? 'error' : ''}`}>
          <div className="tool-inline-row" onClick={onToggle}>
            <div className="tool-inline-left">
              {isRunning ? <SpinnerIcon /> : <ToolIcon />}
              <span className="tool-inline-verb">{exec.toolName}</span>
              {filename && <span className="tool-inline-filename">{filename}</span>}
              {isError && <span className="tool-inline-error-dot">●</span>}
            </div>
            {exec.output && <ChevronIcon open={isOpen} />}
          </div>
          {isOpen && exec.output && (
            <div className="tool-inline-detail">
              <pre className="tool-inline-output">{exec.output.slice(0, 3000)}</pre>
            </div>
          )}
        </div>
      )
    }
  }
}

function SubagentToolItem({ info, onOpen }: { info: ToolDisplayInfo; onOpen: (entry: FleetEntry) => void }): JSX.Element {
  const entry = subagentEntry(info.exec)
  return (
    <button className="subagent-inline" onClick={() => onOpen(entry)}>
      <SubagentMark seed={entry.label !== 'Subagent' ? entry.label : entry.agentId} />
      <span className="subagent-inline-name">{entry.label}</span>
      <span className="subagent-inline-status">{info.exec.status === 'error' ? 'failed' : 'working'}</span>
    </button>
  )
}

function SubagentReplyItem({ block, onOpen }: { block: Block; onOpen: (entry: FleetEntry) => void }): JSX.Element {
  const entry: FleetEntry = {
    id: block.agentId ?? block.agentName ?? 'subagent',
    at: Date.now(),
    agentId: block.agentId ?? block.agentName ?? 'subagent',
    label: block.agentName ?? 'Subagent',
    text: block.message ?? '',
    parentText: '',
    childText: block.message ?? '',
    status: 'done',
    payload: { name: block.agentName ?? 'Subagent', message: block.message ?? '', status: 'done' }
  }
  return (
    <button className="subagent-inline reply" onClick={() => onOpen(entry)}>
      <SubagentMark seed={entry.label !== 'Subagent' ? entry.label : entry.agentId} />
      <span className="subagent-inline-name">{entry.label}</span>
      <span className="subagent-inline-status">replied</span>
      <span className="subagent-inline-preview">{entry.text}</span>
    </button>
  )
}

/* ─── Grouped file edits ──────────────────────────────── */

function EditedFilesGroup({ items, openTools, onToggle }: {
  items: ToolDisplayInfo[]
  openTools: Record<string, boolean>
  onToggle: (id: string) => void
}): JSX.Element {
  const [expanded, setExpanded] = useState(false)
  const VISIBLE_COUNT = 3

  const totalStats = items.reduce((acc, info) => {
    const stats = info.exec.output ? parseDiffStats(info.exec.output) : null
    return { added: acc.added + (stats?.added ?? 0), removed: acc.removed + (stats?.removed ?? 0) }
  }, { added: 0, removed: 0 })

  const visibleItems = expanded ? items : items.slice(0, VISIBLE_COUNT)
  const remainingCount = items.length - VISIBLE_COUNT

  if (items.length === 1) {
    const info = items[0]
    const id = info.block.id ?? ''
    return <InlineToolItem info={info} isOpen={openTools[id] ?? false} onToggle={() => onToggle(id)} />
  }

  return (
    <div className="edited-files-group">
      <div className="edited-files-header" onClick={() => setExpanded((v) => !v)}>
        <div className="edited-files-header-left">
          <PencilIcon />
          <span className="edited-files-title">Edited {items.length} files</span>
          <DiffStat added={totalStats.added} removed={totalStats.removed} />
        </div>
        <div className="edited-files-header-right">
          <ChevronIcon open={expanded} />
        </div>
      </div>

      {expanded && (
        <div className="edited-files-list">
          {visibleItems.map((info) => {
            const filename = extractFilename(info.exec.args)
            const fullPath = extractFullPath(info.exec.args)
            const stats = info.exec.output ? parseDiffStats(info.exec.output) : null
            const displayPath = fullPath.includes('/src/') ? fullPath.split('/src/').pop()! : filename

            return (
              <div key={info.block.id} className="edited-file-row">
                <span className="edited-file-path">{displayPath || filename}</span>
                {stats && <DiffStat added={stats.added} removed={stats.removed} />}
              </div>
            )
          })}

          {!expanded && remainingCount > 0 && (
            <button className="edited-files-more" onClick={() => setExpanded(true)}>
              Show {remainingCount} more file{remainingCount > 1 ? 's' : ''} <ChevronIcon open={false} />
            </button>
          )}
        </div>
      )}
    </div>
  )
}

/* ─── User bubble with collapse ──────────────────────── */

function UserBubble({ text, isLong }: { text: string; isLong: boolean }): JSX.Element {
  const [expanded, setExpanded] = useState(!isLong)
  const preview = isLong ? text.slice(0, USER_COLLAPSE_THRESHOLD) + '…' : text

  return (
    <div className="msg user">
      <div className={`msg-bubble user ${isLong && !expanded ? 'msg-bubble-collapsed' : ''}`}>
        {expanded ? text : preview}
        {isLong && !expanded && (
          <div className="msg-bubble-overflow">
            <button className="msg-expand-btn" onClick={() => setExpanded(true)}>Show more</button>
          </div>
        )}
        {isLong && expanded && (
          <div className="msg-bubble-overflow">
            <button className="msg-expand-btn" onClick={() => setExpanded(false)}>Show less</button>
          </div>
        )}
      </div>
    </div>
  )
}

/* ─── Main MessageItem ────────────────────────────────── */

export default function MessageItem({ message, toolExecs, onOpenSubagent }: Props): JSX.Element {
  const [showThinking, setShowThinking] = useState(false)
  const [openTools, setOpenTools] = useState<Record<string, boolean>>({})
  const [copied, setCopied] = useState(false)

  const toggleTool = (id: string) => setOpenTools((o) => ({ ...o, [id]: !o[id] }))

  if (message.role === 'user') {
    const text = typeof message.content === 'string'
      ? message.content
      : Array.isArray(message.content)
        ? message.content.map((b) => (b.type === 'text' ? b.text : '')).join('')
        : ''
    const isLong = text.length > USER_COLLAPSE_THRESHOLD
    return <UserBubble text={text} isLong={isLong} />
  }

  if (message.role === 'system') {
    const isCompaction = (message as unknown as Record<string, unknown>).compaction === true
      || (typeof message.content === 'string' && message.content.toLowerCase().includes('compacted'))
    return (
      <div className="sys-msg">
        <svg viewBox="0 0 24 24" width="13" height="13" fill="none" stroke="currentColor" strokeWidth="1.8">
          {isCompaction ? (
            <path d="M21.5 2v6h-6M21.34 15.57a10 10 0 11-.57-8.38" />
          ) : (
            <path d="M4 7h16M4 12h10M4 17h7" />
          )}
        </svg>
        <span>{typeof message.content === 'string' ? message.content : 'Command'}</span>
      </div>
    )
  }

  if (message.role === 'toolResult') {
    return <></>
  }

  const blocks = Array.isArray(message.content) ? message.content : [{ type: 'text', text: message.content }] as Block[]
  const hasThinking = blocks.some((b) => b.type === 'thinking')

  const toolInfos = useMemo(() => {
    return blocks
      .filter((b) => b.type === 'toolCall')
      .map((b) => {
        const live = b.id ? toolExecs[b.id] : undefined
        const exec: ToolExecState = live ?? {
          toolCallId: b.id ?? '',
          toolName: b.name ?? 'tool',
          args: (b.arguments as Record<string, unknown>) ?? {},
          output: b.result ?? '',
          status: b.status ?? 'done',
          isError: b.isError
        }
        return { block: b, exec, category: classifyTool(exec.toolName, exec.args) } as ToolDisplayInfo
      })
  }, [blocks, toolExecs])

  const renderGroups = useMemo(() => {
    const groups: Array<{ type: 'text' | 'thinking' | 'tool' | 'file-edit-group' | 'subagent'; block?: Block; tools?: ToolDisplayInfo[]; toolInfo?: ToolDisplayInfo }> = []
    let toolIndex = 0

    for (const b of blocks) {
      if (b.type === 'text' && b.text && b.text.trim()) {
        groups.push({ type: 'text', block: b })
      } else if (b.type === 'thinking' && b.thinking) {
        groups.push({ type: 'thinking', block: b })
      } else if (b.type === 'subagent') {
        groups.push({ type: 'subagent', block: b })
      } else if (b.type === 'toolCall') {
        const info = toolInfos[toolIndex++]
        if (!info) continue

        if (info.category === 'file-edit' || info.category === 'file-create') {
          const last = groups[groups.length - 1]
          if (last && last.type === 'file-edit-group' && last.tools) {
            last.tools.push(info)
          } else {
            groups.push({ type: 'file-edit-group', tools: [info] })
          }
        } else {
          groups.push({ type: 'tool', toolInfo: info })
        }
      }
    }
    return groups
  }, [blocks, toolInfos])

  // Don't render empty assistant message containers
  if (renderGroups.length === 0 && !message.streaming) {
    return <></>
  }

  const copyMessage = () => {
    const textContent = blocks
      .filter((b) => b.type === 'text')
      .map((b) => b.text ?? '')
      .join('\n\n')
    void navigator.clipboard.writeText(textContent)
    setCopied(true)
    setTimeout(() => setCopied(false), 1500)
  }

  const timestamp = message.timestamp
    ? new Date(message.timestamp).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' })
    : null

  return (
    <div className="msg assistant">
      {hasThinking && (
        <div className="msg-head-thinking">
          <button className="thinking-toggle" onClick={() => setShowThinking((v) => !v)}>
            {message.streaming ? 'Thinking' : 'Thought'}
          </button>
        </div>
      )}

      {renderGroups.map((group, i) => {
        if (group.type === 'text' && group.block) {
          return (
            <div key={i} className="msg-text">
              <Markdown text={group.block.text ?? ''} />
              {message.streaming && i === renderGroups.length - 1 && <span className="caret">{'\u258D'}</span>}
            </div>
          )
        }

        if (group.type === 'thinking' && group.block) {
          if (!showThinking) return <span key={i} className="thinking-hidden" />
          return (
            <details key={i} className="thinking-block" open>
              <summary>Thought</summary>
              <pre className="thinking-pre">{group.block.thinking}</pre>
            </details>
          )
        }

        if (group.type === 'file-edit-group' && group.tools) {
          return (
            <EditedFilesGroup
              key={i}
              items={group.tools}
              openTools={openTools}
              onToggle={toggleTool}
            />
          )
        }

        if (group.type === 'tool' && group.toolInfo) {
          const id = group.toolInfo.block.id ?? ''
          if (isRlmSpawn(group.toolInfo.exec) && onOpenSubagent) {
            return <SubagentToolItem key={i} info={group.toolInfo} onOpen={onOpenSubagent} />
          }
          return (
            <InlineToolItem
              key={i}
              info={group.toolInfo}
              isOpen={openTools[id] ?? false}
              onToggle={() => toggleTool(id)}
            />
          )
        }

        if (group.type === 'subagent' && group.block && onOpenSubagent) {
          return <SubagentReplyItem key={i} block={group.block} onOpen={onOpenSubagent} />
        }

        return null
      })}

      {!message.streaming && blocks.some((b) => b.type === 'text' && b.text) && (
        <div className="turn-footer">
          <button className="turn-footer-btn" onClick={copyMessage} title="Copy message">
            <CopyIcon />
            {copied && <span className="turn-footer-copied">Copied!</span>}
          </button>
          {timestamp && <span className="turn-footer-time">{timestamp}</span>}
        </div>
      )}
    </div>
  )
}

/* ─── Inline renderer — safe React, no dangerouslySetInnerHTML ── */

function renderInline(text: string): (JSX.Element | string)[] {
  const re = /(`[^`]+`|\*\*[^*]+\*\*)/g
  const nodes: (JSX.Element | string)[] = []
  let last = 0
  let key = 0
  let m: RegExpExecArray | null
  while ((m = re.exec(text)) !== null) {
    if (m.index > last) nodes.push(text.slice(last, m.index))
    const tok = m[0]
    if (tok.startsWith('`')) {
      nodes.push(<code key={key++} className="md-inline-code">{tok.slice(1, -1)}</code>)
    } else {
      nodes.push(<strong key={key++}>{tok.slice(2, -2)}</strong>)
    }
    last = m.index + tok.length
  }
  if (last < text.length) nodes.push(text.slice(last))
  return nodes
}

/* ─── Markdown renderer — token-based, handles blank lines inside code fences ── */

function Markdown({ text }: { text: string }): JSX.Element {
  const [copiedIndex, setCopiedIndex] = useState<number | null>(null)

  type Token =
    | { type: 'code'; lang: string; code: string; i: number }
    | { type: 'heading'; level: number; content: string; i: number }
    | { type: 'ul'; items: string[]; i: number }
    | { type: 'ol'; items: string[]; i: number }
    | { type: 'para'; content: string; i: number }

  const tokens = useMemo<Token[]>(() => {
    const out: Token[] = []
    let n = 0
    // Extract fenced code blocks first so blank lines inside them are preserved
    const fenceRe = /```(\w*)\n([\s\S]*?)```/g
    let last = 0
    let m: RegExpExecArray | null

    const pushText = (chunk: string) => {
      for (const para of chunk.split(/\n{2,}/)) {
        const p = para.trim()
        if (!p) continue
        const h = p.match(/^(#{1,3})\s+(.+)/)
        if (h) { out.push({ type: 'heading', level: h[1].length, content: h[2], i: n++ }); continue }
        if (p.startsWith('- ') || p.startsWith('* ')) {
          out.push({ type: 'ul', items: p.split('\n').filter(Boolean).map((l) => l.replace(/^[-*]\s+/, '')), i: n++ })
          continue
        }
        if (/^\d+\.\s/.test(p)) {
          out.push({ type: 'ol', items: p.split('\n').filter(Boolean).map((l) => l.replace(/^\d+\.\s+/, '')), i: n++ })
          continue
        }
        out.push({ type: 'para', content: p, i: n++ })
      }
    }

    while ((m = fenceRe.exec(text)) !== null) {
      if (m.index > last) pushText(text.slice(last, m.index))
      out.push({ type: 'code', lang: m[1], code: m[2], i: n++ })
      last = m.index + m[0].length
    }
    if (last < text.length) pushText(text.slice(last))
    return out
  }, [text])

  const handleCopy = (code: string, idx: number) => {
    void navigator.clipboard.writeText(code)
    setCopiedIndex(idx)
    setTimeout(() => setCopiedIndex(null), 1500)
  }

  return (
    <>
      {tokens.map((tok) => {
        if (tok.type === 'code') {
          return (
            <div key={tok.i} className="codeblock">
              <div className="codeblock-head">
                <span>{tok.lang || 'code'}</span>
                <button className="code-copy" onClick={() => handleCopy(tok.code, tok.i)}>
                  {copiedIndex === tok.i ? 'copied!' : 'copy'}
                </button>
              </div>
              <pre className="codeblock-pre">{tok.code}</pre>
            </div>
          )
        }
        if (tok.type === 'heading') {
          return <div key={tok.i} className={`md-h${tok.level}`}>{renderInline(tok.content)}</div>
        }
        if (tok.type === 'ul') {
          return (
            <ul key={tok.i} className="md-ul">
              {tok.items.map((item, j) => <li key={j}>{renderInline(item)}</li>)}
            </ul>
          )
        }
        if (tok.type === 'ol') {
          return (
            <ol key={tok.i} className="md-ol">
              {tok.items.map((item, j) => <li key={j}>{renderInline(item)}</li>)}
            </ol>
          )
        }
        return <p key={tok.i} className="md-p">{renderInline(tok.content)}</p>
      })}
    </>
  )
}

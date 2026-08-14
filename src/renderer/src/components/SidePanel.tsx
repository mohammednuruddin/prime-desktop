import { useState, useEffect, useRef, useCallback } from 'react'
import type { CSSProperties } from 'react'
import { extractText, mergeMessage, type FleetEntry, type RenderMessage } from '../lib/store'
import type { SessionTreeNode, SubagentNode } from '@shared/types'
import { isInternalStateRestoreMessage } from '@shared/messageVisibility'
import SubagentMark from './SubagentMark'
import MessageItem from './MessageItem'
import TerminalPanel from './TerminalPanel'
import GitPanel from './GitPanel'

export type SidePanelTab = 'subagents' | 'sidechat' | 'timeline' | 'terminal' | 'git'

interface Props {
  open: boolean
  onToggle: () => void
  fleet: FleetEntry[]
  tree: SubagentNode[]
  agentId: string | null
  activeTab?: SidePanelTab
  onTabChange?: (tab: SidePanelTab) => void
  selectedEntry: FleetEntry | null
  onSelectEntry: (entry: FleetEntry | null) => void
  showReasoning?: boolean
}

/* ─── Tab icons ──────────────────────────────────────── */
function SubagentIcon(): JSX.Element {
  return (
    <svg viewBox="0 0 24 24" width="13" height="13" fill="none" stroke="currentColor" strokeWidth="1.8">
      <circle cx="12" cy="8" r="4" />
      <path d="M6 20v-1a6 6 0 0112 0v1" />
      <path d="M18 12l3 3-3 3" />
    </svg>
  )
}

function TerminalIcon2(): JSX.Element {
  return (
    <svg viewBox="0 0 24 24" width="13" height="13" fill="none" stroke="currentColor" strokeWidth="1.8">
      <polyline points="4 17 10 11 4 5" />
      <line x1="12" y1="19" x2="20" y2="19" />
    </svg>
  )
}

function GitIcon(): JSX.Element {
  return (
    <svg viewBox="0 0 24 24" width="13" height="13" fill="none" stroke="currentColor" strokeWidth="1.8">
      <circle cx="18" cy="18" r="3" /><circle cx="6" cy="6" r="3" /><circle cx="6" cy="18" r="3" />
      <path d="M6 9v6M15.43 8.57l-8.86 8.86" />
    </svg>
  )
}

function TimelineIcon(): JSX.Element {
  return (
    <svg viewBox="0 0 24 24" width="13" height="13" fill="none" stroke="currentColor" strokeWidth="1.8">
      <circle cx="6" cy="5" r="2" /><circle cx="18" cy="12" r="2" /><circle cx="6" cy="19" r="2" />
      <path d="M8 5h2a4 4 0 014 4v0a3 3 0 003 3M8 19h2a4 4 0 004-4v0a3 3 0 013-3" />
    </svg>
  )
}

function SideChatIcon(): JSX.Element {
  return (
    <svg viewBox="0 0 24 24" width="13" height="13" fill="none" stroke="currentColor" strokeWidth="1.8">
      <path d="M5 5.5h14v10H9l-4 3v-13z" />
    </svg>
  )
}

function ChevronLeft(): JSX.Element {
  return (
    <svg viewBox="0 0 24 24" width="14" height="14" fill="none" stroke="currentColor" strokeWidth="2">
      <polyline points="15 18 9 12 15 6" />
    </svg>
  )
}

function entryName(entry: FleetEntry): string {
  const payload = entry.payload ?? {}
  const name = payload.name ?? payload.agentName ?? payload.taskName ?? payload.sessionName
  return typeof name === 'string' ? name : entry.label.replace(/^observed_/, '').replaceAll('_', ' ')
}

function entryBody(entry: FleetEntry): string {
  const payload = entry.payload ?? {}
  const body = payload.message ?? payload.text ?? payload.summary ?? payload.output ?? payload.result
  if (typeof body === 'string') return body
  return entry.text
}

function parentBody(entry: FleetEntry): string {
  return entry.parentText || String(entry.payload?.task ?? entry.payload?.prompt ?? entry.payload?.message ?? '')
}

function childBody(entry: FleetEntry): string {
  return entry.childText ?? (entry.status === 'done' ? entryBody(entry) : '')
}

function SubagentsTab({ fleet, tree, selected, agentId, onSelect, showReasoning = true }: { fleet: FleetEntry[]; tree: SubagentNode[]; selected: FleetEntry | null; agentId: string | null; onSelect: (entry: FleetEntry | null) => void; showReasoning?: boolean }): JSX.Element {
  if (selected) {
    const selectedNode = findNodePath(tree, selected)?.at(-1) ?? null
    const currentEntry = selectedNode ? {
      ...selected,
      agentId: selectedNode.id,
      label: selectedNode.name,
      status: selectedNode.status === 'working' ? 'running' : selectedNode.status === 'error' ? 'error' : 'done',
      payload: {
        ...(selected.payload ?? {}),
        name: selectedNode.name,
        task: selectedNode.task,
        activeSessionId: selectedNode.activeSessionId,
        sessionId: selectedNode.sessionId
      }
    } satisfies FleetEntry : selected
    return (
      <SubagentChat
        agentId={agentId}
        entry={currentEntry}
        node={selectedNode}
        children={selectedNode?.children ?? []}
        fleet={fleet}
        onSelect={onSelect}
        showReasoning={showReasoning}
      />
    )
  }

  if (tree.length === 0 && fleet.length === 0) {
    return (
      <div className="sp-empty">
        <SubagentIcon />
        <span>No subagent activity yet</span>
      </div>
    )
  }
  const total = tree.length > 0 ? flattenTree(tree).length : fleet.length
  const working = tree.length > 0 ? flattenTree(tree).filter((node) => node.status === 'working').length : fleet.filter((entry) => entry.status === 'running').length
  return (
    <div className="sp-scroll">
      <div className="sp-agent-summary">
        <span>Active · {working}</span>
        <span>{total} total</span>
      </div>
      {tree.length > 0 ? tree.map((node) => (
        <SubagentTreeRow key={node.id} node={node} fleet={fleet} depth={0} onSelect={onSelect} />
      )) : fleet.map((entry) => (
        <FallbackAgentRow key={entry.id} entry={entry} onSelect={onSelect} />
      ))}
    </div>
  )
}

function SubagentTreeRow({ node, fleet, depth, onSelect }: { node: SubagentNode; fleet: FleetEntry[]; depth: number; onSelect: (entry: FleetEntry) => void }): JSX.Element {
  const entry = entryFromNode(node, fleet)
  return (
    <div className="sp-tree-branch">
      <button
        className={`sp-tree-row ${node.status}`}
        style={{ '--tree-depth': depth } as CSSProperties}
        onClick={() => onSelect(entry)}
      >
        <span className="sp-tree-guide" aria-hidden="true" />
        <SubagentMark seed={node.id} />
        <span className="sp-tree-copy">
          <span className="sp-tree-name">{node.name}</span>
          <span className="sp-tree-status">{node.status === 'idle' ? 'Completed' : titleCase(node.status)}</span>
        </span>
        <span className={`sp-tree-state ${node.status}`} aria-label={node.status} />
        <span className="sp-tree-time">{elapsed(node.lastActivityAt)}</span>
      </button>
      {node.children.map((child) => (
        <SubagentTreeRow key={child.id} node={child} fleet={fleet} depth={depth + 1} onSelect={onSelect} />
      ))}
    </div>
  )
}

function FallbackAgentRow({ entry, onSelect }: { entry: FleetEntry; onSelect: (entry: FleetEntry) => void }): JSX.Element {
  return (
    <button className="sp-tree-row idle" style={{ '--tree-depth': 0 } as CSSProperties} onClick={() => onSelect(entry)}>
      <span className="sp-tree-guide" aria-hidden="true" />
      <SubagentMark seed={entry.label !== 'Subagent' ? entry.label : entry.agentId} />
      <span className="sp-tree-copy">
        <span className="sp-tree-name">{entryName(entry)}</span>
        <span className="sp-tree-status">{entry.status === 'running' ? 'Working' : 'Completed'}</span>
      </span>
      <span className={`sp-tree-state ${entry.status === 'running' ? 'working' : 'idle'}`} />
      <span className="sp-tree-time">{elapsed(entry.at)}</span>
    </button>
  )
}

function flattenTree(tree: SubagentNode[]): SubagentNode[] {
  return tree.flatMap((node) => [node, ...flattenTree(node.children)])
}

function elapsed(at: number): string {
  const seconds = Math.max(0, Math.floor((Date.now() - at) / 1000))
  if (seconds < 60) return `${seconds}s`
  if (seconds < 3600) return `${Math.floor(seconds / 60)}m`
  return `${Math.floor(seconds / 3600)}h`
}

function titleCase(value: string): string {
  return value.charAt(0).toUpperCase() + value.slice(1)
}

function entryFromNode(node: SubagentNode, fleet: FleetEntry[]): FleetEntry {
  const stored = fleet.find((entry) => (
    entry.agentId === node.id
    || entry.agentId === node.sessionId
    || entry.payload?.sessionId === node.sessionId
  )) ?? uniqueLegacyEntry(fleet, node.name)
  return {
    ...(stored ?? {}),
    id: stored?.id ?? node.id,
    at: stored?.at ?? node.lastActivityAt,
    agentId: node.id,
    label: node.name,
    text: stored?.text ?? node.task,
    parentText: stored?.parentText || node.task,
    status: node.status === 'working' ? 'running' : node.status === 'error' ? 'error' : 'done',
    parentAgentId: node.parentSessionId,
    depth: node.depth,
    payload: {
      ...(stored?.payload ?? {}),
      name: node.name,
      task: node.task,
      activeSessionId: node.activeSessionId,
      sessionId: node.sessionId
    }
  }
}

function findNodePath(tree: SubagentNode[], entry: FleetEntry): SubagentNode[] | null {
  const ids = new Set(
    [entry.agentId, entry.payload?.activeSessionId, entry.payload?.sessionId]
      .filter((value): value is string => typeof value === 'string' && value.length > 0)
  )
  const exact = findNodePathByIds(tree, ids)
  if (exact || !isLegacyEntry(entry)) return exact
  const named = findNodePathsByName(tree, entry.label)
  return named.length === 1 ? named[0] : null
}

function findNodePathByIds(tree: SubagentNode[], ids: Set<string>): SubagentNode[] | null {
  for (const node of tree) {
    if (ids.has(node.id) || ids.has(node.sessionId) || (node.activeSessionId ? ids.has(node.activeSessionId) : false)) return [node]
    const childPath = findNodePathByIds(node.children, ids)
    if (childPath) return [node, ...childPath]
  }
  return null
}

function findNodePathsByName(tree: SubagentNode[], name: string, parents: SubagentNode[] = []): SubagentNode[][] {
  return tree.flatMap((node) => {
    const path = [...parents, node]
    return [
      ...(node.name === name ? [path] : []),
      ...findNodePathsByName(node.children, name, path)
    ]
  })
}

function isLegacyEntry(entry: FleetEntry): boolean {
  return !entry.agentId || entry.agentId === 'subagent' || entry.agentId === entry.label
}

function uniqueLegacyEntry(fleet: FleetEntry[], name: string): FleetEntry | undefined {
  const matches = fleet.filter((entry) => entry.label === name && isLegacyEntry(entry))
  return matches.length === 1 ? matches[0] : undefined
}

function SubagentChat({
  agentId,
  entry,
  node,
  children,
  fleet,
  onSelect,
  showReasoning = true
}: {
  agentId: string | null
  entry: FleetEntry
  node: SubagentNode | null
  children: SubagentNode[]
  fleet: FleetEntry[]
  onSelect: (entry: FleetEntry) => void
  showReasoning?: boolean
}): JSX.Element {
  const target = node?.activeSessionId ?? node?.sessionId
    ?? (typeof entry.payload?.activeSessionId === 'string' ? entry.payload.activeSessionId : '')
  const taskText = parentBody(entry).trim()
  const [messages, setMessages] = useState<RenderMessage[]>([])
  const [draft, setDraft] = useState('')
  const [sending, setSending] = useState(false)
  const [awaitingReply, setAwaitingReply] = useState(false)
  const [error, setError] = useState('')
  const detailBodyRef = useRef<HTMLDivElement>(null)
  const loadFailures = useRef(0)
  const awaitingReplyRef = useRef(false)
  const assistantSignatureAtSend = useRef('')
  const latestLoad = useRef(0)
  const loadInFlight = useRef(false)
  const loadKey = `${agentId ?? ''}:${target}`
  const activeLoadKey = useRef(loadKey)
  activeLoadKey.current = loadKey

  const load = useCallback(() => {
    const key = `${agentId ?? ''}:${target}`
    if (!agentId || !target || key !== activeLoadKey.current || loadInFlight.current) return
    const requestId = ++latestLoad.current
    loadInFlight.current = true
    void window.prime.fleetMessages(agentId, target)
      .then((items: unknown[]) => {
        if (requestId !== latestLoad.current || key !== activeLoadKey.current) return
        const next = (items as Record<string, unknown>[])
          .filter((item) => !isInternalStateRestoreMessage(item))
          .reduce<RenderMessage[]>(mergeMessage, [])
          .filter((message) => message.role === 'user' || message.role === 'assistant')
        setMessages((current) => {
          const serverUserText = new Set(
            next.filter((message) => message.role === 'user').map((message) => extractText(message.content).trim())
          )
          const pending = current.filter((message) => (
            message.id.startsWith('local-')
            && !serverUserText.has(extractText(message.content).trim())
          ))
          return [...next, ...pending]
        })
        const assistantSignature = next
          .filter((message) => message.role === 'assistant')
          .map((message) => `${message.id}:${extractText(message.content)}`)
          .join('|')
        if (awaitingReplyRef.current && assistantSignature !== assistantSignatureAtSend.current) {
          awaitingReplyRef.current = false
          setAwaitingReply(false)
        }
        loadFailures.current = 0
        setError('')
      })
      .catch(() => {
        if (requestId !== latestLoad.current || key !== activeLoadKey.current) return
        loadFailures.current += 1
        if (loadFailures.current >= 2) setError('Couldn’t refresh this conversation. Retrying…')
      })
      .finally(() => {
        if (requestId === latestLoad.current && key === activeLoadKey.current) loadInFlight.current = false
      })
  }, [agentId, target])

  useEffect(() => {
    latestLoad.current += 1
    loadInFlight.current = false
    setMessages([])
    setDraft('')
    setError('')
    setAwaitingReply(false)
    awaitingReplyRef.current = false
    loadFailures.current = 0
    load()
    const timer = window.setInterval(load, 1400)
    return () => {
      latestLoad.current += 1
      window.clearInterval(timer)
    }
  }, [load])

  useEffect(() => {
    const body = detailBodyRef.current
    if (body && body.scrollHeight > body.clientHeight) {
      body.scrollTo({ top: body.scrollHeight, behavior: 'smooth' })
    }
  }, [messages.length])

  const send = async () => {
    const text = draft.trim()
    if (!agentId || !target || !text || sending) return
    setDraft('')
    setSending(true)
    const assistantSignature = messages
      .filter((message) => message.role === 'assistant')
      .map((message) => `${message.id}:${extractText(message.content)}`)
      .join('|')
    assistantSignatureAtSend.current = assistantSignature
    awaitingReplyRef.current = true
    setAwaitingReply(true)
    setMessages((current) => [...current, {
      id: `local-${Date.now()}`,
      role: 'user',
      content: text,
      timestamp: Date.now()
    }])
    try {
      await window.prime.fleetSend(agentId, target, text, 'auto')
      window.setTimeout(load, 250)
    } catch (reason) {
      awaitingReplyRef.current = false
      setAwaitingReply(false)
      setError(reason instanceof Error ? reason.message : String(reason))
      setDraft(text)
    } finally {
      setSending(false)
    }
  }

  const hasUserMessage = messages.some((message) => message.role === 'user')
  const conversationMessages: RenderMessage[] = messages.length > 0
    ? [
        ...(!hasUserMessage && taskText ? [{
          id: `task-${entry.id}`,
          role: 'user' as const,
          content: taskText,
          timestamp: entry.at
        }] : []),
        ...messages.filter((message) => {
          const text = extractText(message.content).trim()
          return text && !(!hasUserMessage && message.role === 'assistant' && text === taskText)
        })
      ]
    : [
        ...(parentBody(entry) ? [{
          id: `parent-${entry.id}`,
          role: 'user' as const,
          content: parentBody(entry),
          timestamp: entry.at
        }] : []),
        ...(childBody(entry) ? [{
          id: `child-${entry.id}`,
          role: 'assistant' as const,
          content: childBody(entry).trim(),
          timestamp: entry.at
        }] : [])
      ]

  return (
    <div className="sp-agent-detail">
      {node && (
        <div className="sp-agent-facts">
          <div>
            {node.model && <span>{node.model}</span>}
            {typeof node.tokenCount === 'number' && <span>{node.tokenCount.toLocaleString()} tokens</span>}
            {typeof node.toolUseCount === 'number' && <span>{node.toolUseCount} tools</span>}
            {typeof node.durationMs === 'number' && <span>{Math.round(node.durationMs / 1000)}s</span>}
            {node.activity && <span>{node.activity.kind}{node.activity.toolName ? ` · ${node.activity.toolName}` : ''}</span>}
          </div>
          {node.status === 'working' && agentId && (
            <button
              type="button"
              onClick={() => {
                if (window.confirm(`Cancel ${node.name}?`)) {
                  void window.prime.agentHarness(agentId, 'family_cancel', { childId: node.id })
                }
              }}
            >
              Cancel
            </button>
          )}
        </div>
      )}
      <div className="sp-detail-body" ref={detailBodyRef}>
        <div className="sp-chat-feed">
          {conversationMessages.map((message) => (
            <MessageItem
              key={message.id}
              message={message}
              toolExecs={{}}
              onOpenSubagent={onSelect}
              showReasoning={showReasoning}
            />
          ))}
          {conversationMessages.length === 0 && entry.status === 'running' && <div className="sp-chat-state"><span className="sp-stream-dot" /> Working</div>}
          {awaitingReply && (
            <div className="assistant-pending sp-assistant-pending" role="status">
              <span className="assistant-pending-shimmer">Thinking…</span>
            </div>
          )}
          {error && <div className="sp-chat-error">{error}</div>}
          {children.length > 0 && (
            <div className="sp-nested-agents">
              <div className="sp-nested-label">Subagents</div>
              {children.map((child) => (
                <button
                  key={child.id}
                  className="sp-nested-agent"
                  type="button"
                  onClick={() => onSelect(entryFromNode(child, fleet))}
                >
                  <SubagentMark seed={child.id} />
                  <span className="sp-nested-agent-copy">
                    <span>{child.name}</span>
                    <small>{child.status === 'idle' ? 'Completed' : titleCase(child.status)}</small>
                  </span>
                  <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M9 6l6 6-6 6" /></svg>
                </button>
              ))}
            </div>
          )}
        </div>
      </div>
      <div className="sp-chat-composer">
        <textarea
          value={draft}
          onChange={(event) => setDraft(event.target.value)}
          onKeyDown={(event) => {
            if (event.key === 'Enter' && !event.shiftKey) {
              event.preventDefault()
              void send()
            }
          }}
          placeholder={target ? `Message ${entryName(entry)}` : 'This subagent is no longer available'}
          disabled={!target || sending}
          rows={1}
        />
        <div className="sp-chat-toolbar">
          <button className="sp-chat-send" type="button" disabled={!target || !draft.trim() || sending} onClick={() => void send()} aria-label="Send follow-up">
            <svg viewBox="0 0 24 24"><path d="M12 19V5M6 11l6-6 6 6" /></svg>
          </button>
        </div>
      </div>
    </div>
  )
}

/* ─── Main SidePanel ─────────────────────────────────── */
const TABS: { id: SidePanelTab; label: string; Icon: () => JSX.Element }[] = [
  { id: 'subagents', label: 'Subagents', Icon: SubagentIcon },
  { id: 'sidechat', label: 'Side chat', Icon: SideChatIcon },
  { id: 'timeline', label: 'Timeline', Icon: TimelineIcon },
  { id: 'terminal', label: 'Terminal', Icon: TerminalIcon2 },
  { id: 'git', label: 'Git', Icon: GitIcon },
]

export default function SidePanel({ open, onToggle, fleet, tree, agentId, activeTab = 'subagents', onTabChange, selectedEntry, onSelectEntry, showReasoning = true }: Props): JSX.Element {
  const [tab, setTab] = useState<SidePanelTab>(activeTab)
  const [width, setWidth] = useState(() => Math.max(360, Math.min(520, window.innerWidth - 232 - 360)))
  const dragging = useRef(false)

  useEffect(() => { if (activeTab) setTab(activeTab) }, [activeTab])

  const changeTab = (t: SidePanelTab) => {
    setTab(t)
    onTabChange?.(t)
  }

  useEffect(() => {
    const move = (event: MouseEvent) => {
      if (!dragging.current) return
      const maxWidth = Math.max(360, Math.min(760, window.innerWidth - 232 - 360))
      setWidth(Math.max(360, Math.min(maxWidth, window.innerWidth - event.clientX)))
    }
    const stop = () => {
      if (!dragging.current) return
      dragging.current = false
      document.body.classList.remove('resizing-panel')
    }
    window.addEventListener('mousemove', move)
    window.addEventListener('mouseup', stop)
    return () => {
      window.removeEventListener('mousemove', move)
      window.removeEventListener('mouseup', stop)
    }
  }, [])

  useEffect(() => {
    const clampWidth = () => {
      const maxWidth = Math.max(360, Math.min(760, window.innerWidth - 232 - 360))
      setWidth((current) => Math.min(current, maxWidth))
    }
    window.addEventListener('resize', clampWidth)
    clampWidth()
    return () => window.removeEventListener('resize', clampWidth)
  }, [])

  const selectedPath = selectedEntry ? findNodePath(tree, selectedEntry) : null

  return (
    <aside className={`side-panel ${open ? 'open' : 'closed'}`} style={open ? { width } : undefined}>
      {open && (
        <>
          <div
            className="sp-resize-handle"
            onMouseDown={() => {
              dragging.current = true
              document.body.classList.add('resizing-panel')
            }}
          />
          {/* Browser-style tab bar */}
          <div className="sp-tabbar">
            {selectedEntry && tab === 'subagents' ? (
              <div className="sp-selected-title">
                <button className="sp-back" onClick={() => onSelectEntry(null)} title="Back to subagents">
                  <ChevronLeft />
                </button>
                <div className="sp-agent-breadcrumbs" aria-label="Subagent path">
                  {selectedPath?.length ? selectedPath.map((node, index) => (
                    <span className="sp-agent-crumb-wrap" key={node.id}>
                      {index > 0 && <span className="sp-agent-crumb-separator">/</span>}
                      <button
                        className={`sp-agent-crumb ${index === selectedPath.length - 1 ? 'current' : ''}`}
                        type="button"
                        onClick={() => onSelectEntry(entryFromNode(node, fleet))}
                        title={node.name}
                      >
                        <SubagentMark seed={node.id} />
                        <span>{node.name}</span>
                      </button>
                    </span>
                  )) : (
                    <span className="sp-agent-crumb current">
                      <SubagentMark seed={selectedEntry.label !== 'Subagent' ? selectedEntry.label : selectedEntry.agentId} />
                      <span>{entryName(selectedEntry)}</span>
                    </span>
                  )}
                </div>
              </div>
            ) : TABS.map(({ id, label, Icon }) => (
                <button
                  key={id}
                  className={`sp-tab ${tab === id ? 'active' : ''}`}
                  onClick={() => changeTab(id)}
                >
                  <Icon />
                  <span>{label}</span>
                  {id === 'subagents' && (tree.length > 0 || fleet.length > 0) && (
                    <span className="sp-tab-badge">{Math.min(99, tree.length > 0 ? flattenTree(tree).length : fleet.length)}</span>
                  )}
                </button>
              ))}
            <div className="sp-tabbar-fill" />
            <button className="sp-close" onClick={onToggle} title="Close inspector" aria-label="Close inspector">×</button>
          </div>

          {/* Content */}
          <div className="sp-content">
            {tab === 'subagents' && <SubagentsTab fleet={fleet} tree={tree} selected={selectedEntry} agentId={agentId} onSelect={onSelectEntry} showReasoning={showReasoning} />}
            <div id="side-thread-panel-slot" className={`side-thread-slot ${tab === 'sidechat' ? 'active' : ''}`} />
            {tab === 'timeline' && <TimelineTab agentId={agentId} />}
            {tab === 'terminal' && <TerminalPanel agentId={agentId} />}
            {tab === 'git' && <GitPanel agentId={agentId} />}
          </div>
        </>
      )}
    </aside>
  )
}

function TimelineTab({ agentId }: { agentId: string | null }): JSX.Element {
  const [tree, setTree] = useState<SessionTreeNode[]>([])
  const [leafId, setLeafId] = useState<string | null>(null)
  const [loading, setLoading] = useState(false)

  const load = useCallback(() => {
    if (!agentId) return
    setLoading(true)
    void window.prime.agentHarness(agentId, 'get_tree_full')
      .then((result) => {
        const value = result as { tree?: SessionTreeNode[]; leafId?: string | null }
        setTree(value.tree ?? [])
        setLeafId(value.leafId ?? null)
      })
      .finally(() => setLoading(false))
  }, [agentId])

  useEffect(() => {
    setTree([])
    load()
    if (!agentId) return
    const off = window.prime.onEvent((raw) => {
      const event = raw as { agentId?: string; type?: string }
      if (event.agentId === agentId && ['session_replaced', 'session_resynced', 'message_end'].includes(event.type ?? '')) load()
    })
    return off
  }, [agentId, load])

  if (!agentId) return <div className="sp-empty">Open a project to inspect its timeline.</div>
  const rows = flattenSessionTree(tree)
  return (
    <div className="timeline-tab sp-scroll">
      <div className="timeline-toolbar">
        <span>{rows.length} entries</span>
        <button type="button" onClick={load} disabled={loading}>{loading ? 'Loading…' : 'Refresh'}</button>
        <button
          type="button"
          onClick={async () => {
            const outputPath = await window.prime.chooseSave('session.jsonl')
            if (outputPath) await window.prime.agentHarness(agentId, 'export_jsonl', { outputPath })
          }}
        >
          Export JSONL
        </button>
      </div>
      {rows.map(({ node, depth }) => {
        const text = sessionEntryText(node)
        const current = node.entry.id === leafId
        return (
          <div className={`timeline-row ${current ? 'current' : ''}`} key={node.entry.id} style={{ '--timeline-depth': depth } as CSSProperties}>
            <span className="timeline-guide" aria-hidden="true" />
            <div className="timeline-copy">
              <div>
                <span className="timeline-kind">{node.entry.type.replaceAll('_', ' ')}</span>
                {node.label && <span className="timeline-label">{node.label}</span>}
                {current && <span className="timeline-current">current</span>}
              </div>
              {text && <span className="timeline-text">{text}</span>}
            </div>
            <div className="timeline-actions">
              <button
                type="button"
                onClick={() => {
                  const label = window.prompt('Entry label', node.label ?? '')
                  if (label !== null) {
                    void window.prime.agentHarness(agentId, 'label_tree', { entryId: node.entry.id, label: label.trim() || undefined }).then(load)
                  }
                }}
              >
                Label
              </button>
              {!current && (
                <button
                  type="button"
                  onClick={() => {
                    if (window.confirm('Navigate the active session to this branch?')) {
                      void window.prime.agentHarness(agentId, 'navigate_tree', { targetId: node.entry.id }).then(load)
                    }
                  }}
                >
                  Open
                </button>
              )}
            </div>
          </div>
        )
      })}
      {!loading && rows.length === 0 && <div className="sp-empty">No timeline entries yet.</div>}
    </div>
  )
}

function flattenSessionTree(tree: SessionTreeNode[], depth = 0): { node: SessionTreeNode; depth: number }[] {
  return tree.flatMap((node) => [{ node, depth }, ...flattenSessionTree(node.children ?? [], depth + 1)])
}

function sessionEntryText(node: SessionTreeNode): string {
  const entry = node.entry
  if (entry.type === 'message' && entry.message && typeof entry.message === 'object') {
    return extractText((entry.message as Record<string, unknown>).content).slice(0, 180)
  }
  const candidate = entry.summary ?? entry.name ?? entry.customType
  return typeof candidate === 'string' ? candidate.slice(0, 180) : ''
}

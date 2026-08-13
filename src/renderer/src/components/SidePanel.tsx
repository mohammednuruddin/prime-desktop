import { useState, useEffect, useRef } from 'react'
import type { FleetEntry } from '../lib/store'
import SubagentMark from './SubagentMark'
import TerminalPanel from './TerminalPanel'
import GitPanel from './GitPanel'

export type SidePanelTab = 'subagents' | 'terminal' | 'git'

interface Props {
  open: boolean
  onToggle: () => void
  fleet: FleetEntry[]
  agentId: string | null
  activeTab?: SidePanelTab
  onTabChange?: (tab: SidePanelTab) => void
  selectedEntry: FleetEntry | null
  onSelectEntry: (entry: FleetEntry | null) => void
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
  return entry.parentText ?? String(entry.payload?.task ?? entry.payload?.prompt ?? entry.payload?.message ?? '')
}

function childBody(entry: FleetEntry): string {
  return entry.childText ?? (entry.status === 'done' ? entryBody(entry) : '')
}

function SubagentsTab({ fleet, selected, onSelect }: { fleet: FleetEntry[]; selected: FleetEntry | null; onSelect: (entry: FleetEntry | null) => void }): JSX.Element {
  const bottomRef = useRef<HTMLDivElement>(null)
  useEffect(() => { bottomRef.current?.scrollIntoView({ behavior: 'smooth' }) }, [fleet.length])

  if (selected) {
    return (
      <div className="sp-agent-detail">
        <div className="sp-detail-body">
          <div className="sp-chat-feed">
            {parentBody(selected) && (
              <div className="sp-chat-turn parent">
                <div className="sp-chat-bubble">{parentBody(selected)}</div>
              </div>
            )}
            <div className="sp-chat-turn child">
              {childBody(selected) ? (
                <div className="sp-chat-message">{childBody(selected)}</div>
              ) : selected.status === 'error' ? (
                <div className="sp-chat-state error">Subagent failed</div>
              ) : (
                <div className="sp-chat-state"><span className="sp-stream-dot" /> Working</div>
              )}
            </div>
          </div>
        </div>
      </div>
    )
  }

  if (fleet.length === 0) {
    return (
      <div className="sp-empty">
        <SubagentIcon />
        <span>No subagent activity yet</span>
      </div>
    )
  }
  return (
    <div className="sp-scroll">
      {fleet.map((entry) => (
        <button key={entry.id} className="sp-entry" onClick={() => onSelect(entry)}>
          <div className="sp-entry-meta">
            <SubagentMark seed={entry.label !== 'Subagent' ? entry.label : entry.agentId} />
            <span className="sp-entry-label">{entryName(entry)}</span>
            <span className="sp-entry-time">{new Date(entry.at).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit', second: '2-digit' })}</span>
          </div>
          {entryBody(entry) && <div className="sp-entry-text">{entryBody(entry).slice(0, 300)}</div>}
        </button>
      ))}
      <div ref={bottomRef} />
    </div>
  )
}

/* ─── Main SidePanel ─────────────────────────────────── */
const TABS: { id: SidePanelTab; label: string; Icon: () => JSX.Element }[] = [
  { id: 'subagents', label: 'Subagents', Icon: SubagentIcon },
  { id: 'terminal', label: 'Terminal', Icon: TerminalIcon2 },
  { id: 'git', label: 'Git', Icon: GitIcon },
]

export default function SidePanel({ open, onToggle, fleet, agentId, activeTab = 'subagents', onTabChange, selectedEntry, onSelectEntry }: Props): JSX.Element {
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
                <SubagentMark seed={selectedEntry.label !== 'Subagent' ? selectedEntry.label : selectedEntry.agentId} />
                <span>{entryName(selectedEntry)}</span>
              </div>
            ) : TABS.map(({ id, label, Icon }) => (
                <button
                  key={id}
                  className={`sp-tab ${tab === id ? 'active' : ''}`}
                  onClick={() => changeTab(id)}
                >
                  <Icon />
                  <span>{label}</span>
                  {id === 'subagents' && fleet.length > 0 && (
                    <span className="sp-tab-badge">{fleet.length > 99 ? '99+' : fleet.length}</span>
                  )}
                </button>
              ))}
            <div className="sp-tabbar-fill" />
            <button className="sp-close" onClick={onToggle} title="Close inspector" aria-label="Close inspector">×</button>
          </div>

          {/* Content */}
          <div className="sp-content">
            {tab === 'subagents' && <SubagentsTab fleet={fleet} selected={selectedEntry} onSelect={onSelectEntry} />}
            {tab === 'terminal' && <TerminalPanel agentId={agentId} />}
            {tab === 'git' && <GitPanel agentId={agentId} />}
          </div>
        </>
      )}
    </aside>
  )
}

import { useState, useEffect } from 'react'
import type { AppState } from '../lib/store'
import type { ViewId, SessionSummary } from '@shared/types'
import CloudMark from './CloudMark'

interface Props {
  state: AppState
  activeAgentId: string | null
  onView: (v: ViewId) => void
  onNewChat?: () => void
  onSelectTab?: (tabId: string) => void
  onCloseTab?: (tabId: string) => void
  onOpenFolder?: () => void
  collapsed?: boolean
  onToggle?: () => void
}

export default function Sidebar({ state, activeAgentId, onView, onNewChat, onSelectTab, onCloseTab, onOpenFolder, collapsed = false, onToggle }: Props): JSX.Element {
  const [sessionsByAgent, setSessionsByAgent] = useState<Record<string, SessionSummary[]>>({})
  const [dropdownOpen, setDropdownOpen] = useState(false)
  const [expandedTabs, setExpandedTabs] = useState<Set<string>>(new Set())
  const [sessionLoading, setSessionLoading] = useState<string | null>(null)
  const [sessionError, setSessionError] = useState<string | null>(null)
  const [selectedSession, setSelectedSession] = useState<string | null>(null)

  const agent = activeAgentId ? state.agents[activeAgentId] : null
  const working = agent && (agent.status === 'working' || agent.isStreaming)

  useEffect(() => {
    let disposed = false
    const load = (agentId: string) => void window.prime.agentSessions(agentId)
      .then((items) => {
        if (disposed) return
        setSessionsByAgent((previous) => ({ ...previous, [agentId]: items }))
        setSessionError(null)
      })
      .catch(() => {
        if (!disposed) setSessionError('Chats could not be loaded')
      })
    const loadAll = () => {
      for (const tab of state.tabs) load(`agent-${tab.id}`)
    }
    loadAll()
    const timer = window.setInterval(loadAll, 15000)
    const off = window.prime.onEvent((raw) => {
      const event = raw as { agentId?: string; type?: string }
      if (event.agentId && (
        event.type === 'turn_end'
        || event.type === 'session_resumed'
        || event.type === 'session_started'
        || event.type === 'session_replaced'
      )) load(event.agentId)
    })
    return () => {
      disposed = true
      window.clearInterval(timer)
      off()
    }
  }, [state.tabs])

  // Auto-expand active tab
  useEffect(() => {
    if (state.activeTabId) {
      setExpandedTabs((prev) => new Set([...prev, state.activeTabId!]))
    }
  }, [state.activeTabId])

  function toggleTab(tabId: string) {
    setExpandedTabs((prev) => {
      const next = new Set(prev)
      if (next.has(tabId)) next.delete(tabId)
      else next.add(tabId)
      return next
    })
  }

  if (collapsed) {
    return <aside className="sidebar collapsed" aria-hidden="true" />
  }

  return (
    <aside className="sidebar">
      <div className="sidebar-drag" />
      <div className="sidebar-window-controls">
        <button className="sidebar-titlebar-btn" title="Collapse sidebar" onClick={onToggle}>
          <CollapseIcon collapsed={false} />
        </button>
        <button className="sidebar-titlebar-btn" title="Back" disabled>
          <BackIcon />
        </button>
        <button className="sidebar-titlebar-btn" title="Forward" disabled>
          <ForwardIcon />
        </button>
      </div>
      <div className="sidebar-header">
        <div className="sidebar-header-row">
          <div className="sidebar-brand-wrap">
            <button className="sidebar-brand-btn" onClick={() => setDropdownOpen((v) => !v)}>
              <CloudMark size={18} />
              <span className="brand-name">Prime</span>
              <svg className="brand-chevron" viewBox="0 0 24 24" width="11" height="11" fill="none" stroke="currentColor" strokeWidth="2">
                <path d="M6 9l6 6 6-6" />
              </svg>
            </button>

            {dropdownOpen && (
              <>
                <div className="dropdown-overlay" onClick={() => setDropdownOpen(false)} />
                <div className="sidebar-dropdown">
                  <div className="dropdown-label">Open projects</div>
                  {state.tabs.map((t) => (
                    <button
                      key={t.id}
                      className={`dropdown-item ${t.id === state.activeTabId ? 'active' : ''}`}
                      onClick={() => { onSelectTab?.(t.id); setDropdownOpen(false) }}
                    >
                      <FolderIcon />
                      <span>{t.name}</span>
                    </button>
                  ))}
                  <div className="dropdown-divider" />
                  <button className="dropdown-item" onClick={() => { onOpenFolder?.(); setDropdownOpen(false) }}>
                    <PlusIcon />
                    <span>Open project…</span>
                  </button>
                </div>
              </>
            )}
          </div>

          <button className="sidebar-icon-btn" title="Search" onClick={() => onView('chat')}>
            <SearchIcon />
          </button>
        </div>
      </div>

      {/* New Chat */}
      <div className="sidebar-quick-nav">
        <button
          className="quick-nav-btn"
          onClick={() => { onView('chat'); onNewChat?.() }}
        >
          <EditIcon />
          <span>New chat</span>
        </button>
      </div>

      <div className="sidebar-nav-links">
        <button
          className={`nav-link-btn ${state.view === 'approval' ? 'active' : ''}`}
          onClick={() => onView('approval')}
        >
          <GitIcon />
          <span>Review</span>
        </button>
        <button
          className={`nav-link-btn ${state.view === 'fleet' ? 'active' : ''}`}
          onClick={() => onView('fleet')}
        >
          <ClockIcon />
          <span>Automations</span>
        </button>
        <button
          className={`nav-link-btn ${state.view === 'skills' ? 'active' : ''}`}
          onClick={() => onView('skills')}
        >
          <AtIcon />
          <span>Skills</span>
        </button>
      </div>

      {state.tabs.length > 0 && (
        <div className="sidebar-section">
          {state.tabs.map((tab) => {
            const isActive = tab.id === state.activeTabId
            const isExpanded = expandedTabs.has(tab.id)
            const tabAgentId = `agent-${tab.id}`
            const tabAgent = state.agents[tabAgentId]
            const sessions = sessionsByAgent[tabAgentId] ?? []

            return (
              <div key={tab.id} className="project-group">
                <div className={`folder-row ${isActive ? 'active' : ''}`}>
                  <button
                    className="folder-row-main"
                    onClick={() => {
                      onSelectTab?.(tab.id)
                      onView('chat')
                      toggleTab(tab.id)
                    }}
                  >
                    <FolderIcon />
                    <span className="folder-name">{tab.name}</span>
                    <svg
                      className={`folder-chevron ${isExpanded ? 'open' : ''}`}
                      viewBox="0 0 24 24" width="10" height="10"
                      fill="none" stroke="currentColor" strokeWidth="2"
                    >
                      <path d="M6 9l6 6 6-6" />
                    </svg>
                  </button>
                  <button
                    className="folder-close"
                    title="Close project"
                    onClick={(e) => {
                      e.stopPropagation()
                      onCloseTab?.(tab.id)
                    }}
                  >
                    ×
                  </button>
                </div>

                {isExpanded && (
                  <div className="folder-sub-items">
                    {sessions.length === 0 ? (
                      <div className="sub-chat-item muted">No chats yet</div>
                    ) : (
                      sessions.map((s) => {
                        const isCurrentSession = isActive && (
                          s.sessionId === tabAgent?.sessionId
                          || (!tabAgent?.sessionId && selectedSession === s.sessionFile)
                        )
                        const title = sessionDisplayName(s)
                        return (
                          <button
                            key={s.sessionFile}
                            className={`sub-chat-item ${isCurrentSession ? 'active' : ''}`}
                            aria-current={isCurrentSession ? 'page' : undefined}
                            onClick={() => {
                              onSelectTab?.(tab.id)
                              onView('chat')
                              setSelectedSession(s.sessionFile)
                              setSessionLoading(s.sessionFile)
                              setSessionError(null)
                              void window.prime.agentResume(tabAgentId, s.sessionFile)
                                .catch(() => setSessionError('This session could not be opened'))
                                .finally(() => setSessionLoading(null))
                            }}
                            title={title}
                          >
                            <span className="session-item-title">
                              {title}{sessionLoading === s.sessionFile ? ' · Opening' : ''}
                            </span>
                          </button>
                        )
                      })
                  )}
                  {sessionError && <div className="session-error" role="status">{sessionError}</div>}
                  </div>
                )}
              </div>
            )
          })}
        </div>
      )}

      <div className="sidebar-spacer" />

      {/* Footer */}
      <div className="sidebar-footer">
        {agent && (
          <div className="sidebar-agent-chip">
            <span className={`status-dot ${working ? 'working' : 'idle'}`} />
            <span className="agent-chip-text">{working ? 'Working…' : 'Ready'}</span>
          </div>
        )}
        <button className="sidebar-user-btn" onClick={() => onView('settings')}>
          <div className="user-avatar">P</div>
          <span className="user-name">Prime</span>
          <QuestionIcon />
        </button>
      </div>
    </aside>
  )
}

function sessionDisplayName(session: SessionSummary): string {
  const name = typeof session.name === 'string' ? session.name.trim() : ''
  return name || 'New thread'
}

/* ─── Icons ──────────────────────────────────────────────────── */

function FolderIcon() {
  return (
    <svg viewBox="0 0 24 24" width="13" height="13" fill="none" stroke="currentColor" strokeWidth="1.7">
      <path d="M22 19a2 2 0 01-2 2H4a2 2 0 01-2-2V5a2 2 0 012-2h5l2 3h9a2 2 0 012 2z" />
    </svg>
  )
}

function EditIcon() {
  return (
    <svg viewBox="0 0 24 24" width="13" height="13" fill="none" stroke="currentColor" strokeWidth="1.8">
      <path d="M11 4H4a2 2 0 00-2 2v14a2 2 0 002 2h14a2 2 0 002-2v-7" />
      <path d="M18.5 2.5a2.121 2.121 0 013 3L12 15l-4 1 1-4 9.5-9.5z" />
    </svg>
  )
}

function SearchIcon() {
  return (
    <svg viewBox="0 0 24 24" width="15" height="15" fill="none" stroke="currentColor" strokeWidth="1.8">
      <circle cx="11" cy="11" r="8" />
      <path d="M21 21l-4.35-4.35" />
    </svg>
  )
}

function CollapseIcon({ collapsed }: { collapsed: boolean }) {
  return <svg viewBox="0 0 24 24" width="15" height="15" fill="none" stroke="currentColor" strokeWidth="1.7"><rect x="3" y="4" width="18" height="16" rx="2" /><path d={collapsed ? 'M9 4v16M14 9l3 3-3 3' : 'M15 4v16M10 9l-3 3 3 3'} /></svg>
}

function BackIcon() {
  return <svg viewBox="0 0 24 24" width="15" height="15" fill="none" stroke="currentColor" strokeWidth="1.7"><path d="M15 18l-6-6 6-6" /></svg>
}

function ForwardIcon() {
  return <svg viewBox="0 0 24 24" width="15" height="15" fill="none" stroke="currentColor" strokeWidth="1.7"><path d="M9 6l6 6-6 6" /></svg>
}

function GitIcon() {
  return (
    <svg viewBox="0 0 24 24" width="13" height="13" fill="none" stroke="currentColor" strokeWidth="1.8">
      <circle cx="18" cy="18" r="3" />
      <circle cx="6" cy="6" r="3" />
      <circle cx="6" cy="18" r="3" />
      <path d="M6 9v6M15.5 6.5L8.5 13.5" />
    </svg>
  )
}

function ClockIcon() {
  return (
    <svg viewBox="0 0 24 24" width="13" height="13" fill="none" stroke="currentColor" strokeWidth="1.8">
      <circle cx="12" cy="12" r="10" />
      <polyline points="12 6 12 12 16 14" />
    </svg>
  )
}

function AtIcon() {
  return (
    <svg viewBox="0 0 24 24" width="13" height="13" fill="none" stroke="currentColor" strokeWidth="1.8">
      <circle cx="12" cy="12" r="4" />
      <path d="M16 8v5a3 3 0 006 0v-1a10 10 0 10-3.927.94" />
    </svg>
  )
}

function PlusIcon() {
  return (
    <svg viewBox="0 0 24 24" width="13" height="13" fill="none" stroke="currentColor" strokeWidth="2">
      <path d="M12 5v14M5 12h14" />
    </svg>
  )
}

function QuestionIcon() {
  return (
    <svg viewBox="0 0 24 24" width="13" height="13" fill="none" stroke="currentColor" strokeWidth="1.8">
      <circle cx="12" cy="12" r="10" />
      <path d="M9.09 9a3 3 0 015.83 1c0 2-3 3-3 3" />
      <line x1="12" y1="17" x2="12.01" y2="17" />
    </svg>
  )
}

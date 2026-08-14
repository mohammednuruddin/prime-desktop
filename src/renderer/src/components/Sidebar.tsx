import { useState, useEffect } from 'react'
import type { AppState } from '../lib/store'
import type { ViewId, SessionSummary } from '@shared/types'
import CloudMark from './CloudMark'
import WorkingMark from './WorkingMark'

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
  const [pinnedSessions, setPinnedSessions] = useState<Set<string>>(new Set())
  const [confirmDelete, setConfirmDelete] = useState<string | null>(null)
  const [deletingSession, setDeletingSession] = useState<string | null>(null)

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

  useEffect(() => {
    void window.prime.sessionPinsGet().then((paths: string[]) => setPinnedSessions(new Set(paths)))
  }, [])

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

  function openSession(tabId: string, agentId: string, session: SessionSummary) {
    onSelectTab?.(tabId)
    onView('chat')
    setSelectedSession(session.sessionFile)
    setSessionLoading(session.sessionFile)
    setSessionError(null)
    void window.prime.agentResume(agentId, session.sessionFile)
      .catch(() => setSessionError('This session could not be opened'))
      .finally(() => setSessionLoading(null))
  }

  function setPinned(sessionFile: string, pinned: boolean) {
    setPinnedSessions((previous) => {
      const next = new Set(previous)
      if (pinned) next.add(sessionFile)
      else next.delete(sessionFile)
      return next
    })
    void window.prime.sessionPinSet(sessionFile, pinned).catch(() => {
      setPinnedSessions((previous) => {
        const next = new Set(previous)
        if (pinned) next.delete(sessionFile)
        else next.add(sessionFile)
        return next
      })
    })
  }

  function deleteSession(agentId: string, session: SessionSummary) {
    setDeletingSession(session.sessionFile)
    setSessionError(null)
    void window.prime.agentSessionDelete(agentId, session.sessionFile)
      .then((items: SessionSummary[]) => {
        setSessionsByAgent((previous) => ({ ...previous, [agentId]: items }))
        setPinnedSessions((previous) => {
          const next = new Set(previous)
          next.delete(session.sessionFile)
          return next
        })
        setConfirmDelete(null)
      })
      .catch(() => setSessionError('This chat could not be deleted'))
      .finally(() => setDeletingSession(null))
  }

  if (collapsed) {
    return <aside className="sidebar collapsed" aria-hidden="true" />
  }

  const pinnedRows = state.tabs.flatMap((tab) => {
    const agentId = `agent-${tab.id}`
    return (sessionsByAgent[agentId] ?? [])
      .filter((session) => pinnedSessions.has(session.sessionFile))
      .map((session) => ({ tab, agentId, session }))
  })

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
          <span>Resources</span>
        </button>
        <button
          className={`nav-link-btn ${state.view === 'diagnostics' ? 'active' : ''}`}
          onClick={() => onView('diagnostics')}
        >
          <GitIcon />
          <span>Daemon</span>
        </button>
      </div>

      {state.tabs.length > 0 && (
        <div className="sidebar-section">
          {pinnedRows.length > 0 && (
            <div className="sidebar-pinned-group">
              <div className="section-title">Pinned</div>
              {pinnedRows.map(({ tab, agentId: pinnedAgentId, session }) => {
                const tabAgent = state.agents[pinnedAgentId]
                const isCurrentSession = tab.id === state.activeTabId && (
                  session.sessionId === tabAgent?.sessionId
                  || (!tabAgent?.sessionId && selectedSession === session.sessionFile)
                )
                return (
                  <SessionRow
                    key={session.sessionFile}
                    session={session}
                    active={isCurrentSession}
                    pinned
                    loading={sessionLoading === session.sessionFile}
                    working={isCurrentSession && isAgentWorking(tabAgent)}
                    confirmingDelete={confirmDelete === session.sessionFile}
                    deleting={deletingSession === session.sessionFile}
                    onOpen={() => openSession(tab.id, pinnedAgentId, session)}
                    onPin={() => setPinned(session.sessionFile, false)}
                    onRequestDelete={() => setConfirmDelete(session.sessionFile)}
                    onCancelDelete={() => setConfirmDelete(null)}
                    onConfirmDelete={() => deleteSession(pinnedAgentId, session)}
                  />
                )
              })}
            </div>
          )}
          {state.tabs.map((tab) => {
            const isActive = tab.id === state.activeTabId
            const isExpanded = expandedTabs.has(tab.id)
            const tabAgentId = `agent-${tab.id}`
            const tabAgent = state.agents[tabAgentId]
            const sessions = sessionsByAgent[tabAgentId] ?? []
            const unpinnedSessions = sessions.filter((session) => !pinnedSessions.has(session.sessionFile))

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
                      unpinnedSessions.map((s) => {
                        const isCurrentSession = isActive && (
                          s.sessionId === tabAgent?.sessionId
                          || (!tabAgent?.sessionId && selectedSession === s.sessionFile)
                        )
                        return (
                          <SessionRow
                            key={s.sessionFile}
                            session={s}
                            active={isCurrentSession}
                            pinned={false}
                            loading={sessionLoading === s.sessionFile}
                            working={isCurrentSession && isAgentWorking(tabAgent)}
                            confirmingDelete={confirmDelete === s.sessionFile}
                            deleting={deletingSession === s.sessionFile}
                            onOpen={() => openSession(tab.id, tabAgentId, s)}
                            onPin={() => setPinned(s.sessionFile, true)}
                            onRequestDelete={() => setConfirmDelete(s.sessionFile)}
                            onCancelDelete={() => setConfirmDelete(null)}
                            onConfirmDelete={() => deleteSession(tabAgentId, s)}
                          />
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

function SessionRow({
  session,
  active,
  pinned,
  loading,
  working,
  confirmingDelete,
  deleting,
  onOpen,
  onPin,
  onRequestDelete,
  onCancelDelete,
  onConfirmDelete
}: {
  session: SessionSummary
  active: boolean
  pinned: boolean
  loading: boolean
  working: boolean
  confirmingDelete: boolean
  deleting: boolean
  onOpen: () => void
  onPin: () => void
  onRequestDelete: () => void
  onCancelDelete: () => void
  onConfirmDelete: () => void
}): JSX.Element {
  const title = sessionDisplayName(session)
  return (
    <div className={`sub-chat-item ${active ? 'active' : ''} ${confirmingDelete ? 'confirming-delete' : ''}`} aria-current={active ? 'page' : undefined}>
      <button className="session-item-main" type="button" onClick={onOpen} title={title}>
        {(working || loading) && <WorkingMark label={loading ? 'Opening' : 'Working'} />}
        <span className="session-item-title">{title}</span>
      </button>
      <div className={`session-row-actions ${confirmingDelete ? 'confirming' : ''}`}>
        {confirmingDelete ? (
          <>
            <button className="session-action confirm" type="button" onClick={onConfirmDelete} disabled={deleting} title="Confirm delete" aria-label={`Delete ${title}`}>
              <CheckIcon />
            </button>
            <button className="session-action cancel" type="button" onClick={onCancelDelete} disabled={deleting} title="Cancel" aria-label="Cancel delete">
              <CrossIcon />
            </button>
          </>
        ) : (
          <>
            <button className={`session-action pin ${pinned ? 'pinned' : ''}`} type="button" onClick={onPin} title={pinned ? 'Unpin chat' : 'Pin chat'} aria-label={pinned ? `Unpin ${title}` : `Pin ${title}`}>
              <PinIcon />
            </button>
            <button className="session-action delete" type="button" onClick={onRequestDelete} title="Delete chat" aria-label={`Delete ${title}`}>
              <TrashIcon />
            </button>
          </>
        )}
      </div>
    </div>
  )
}

function isAgentWorking(agent?: AppState['agents'][string]): boolean {
  return Boolean(agent && (agent.status === 'working' || agent.isStreaming))
}

function sessionDisplayName(session: SessionSummary): string {
  const name = typeof session.name === 'string' ? session.name.trim() : ''
  return name || 'New thread'
}

function PinIcon() {
  return <svg viewBox="0 0 24 24"><path d="M14.5 4.5l5 5-3 1.5-3.5 3.5v4l-1 1-3.5-5.5-4-4 1-1h4L9 5.5l1.5-3 4 2z" /><path d="M8.5 15.5L4 20" /></svg>
}

function TrashIcon() {
  return <svg viewBox="0 0 24 24"><path d="M4 7h16M9 7V4h6v3M7 7l1 13h8l1-13M10 11v5M14 11v5" /></svg>
}

function CheckIcon() {
  return <svg viewBox="0 0 24 24"><path d="M5 12.5l4.5 4.5L19 7.5" /></svg>
}

function CrossIcon() {
  return <svg viewBox="0 0 24 24"><path d="M7 7l10 10M17 7L7 17" /></svg>
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

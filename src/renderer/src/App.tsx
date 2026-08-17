import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import { initialState, mergeMessage, finishToolExecs, patchToolExecs, type AppState, type FleetEntry, type RenderMessage } from './lib/store'
import TabBar from './components/TabBar'
import Sidebar from './components/Sidebar'
import SidePanel, { type SidePanelTab } from './components/SidePanel'
import ChatView from './views/ChatView'
import FleetView from './views/FleetView'
import ApprovalView from './views/ApprovalView'
import DashboardView from './views/DashboardView'
import SkillsView from './views/SkillsView'
import SettingsView from './views/SettingsView'
import DiagnosticsView from './views/DiagnosticsView'
import Toasts from './components/Toasts'
import DialogHost from './components/DialogHost'
import WelcomeScreen from './components/WelcomeScreen'
import type { PrimeEvent, SubagentNode, Toast } from '@shared/types'
import type { AccessMode } from './components/AccessPicker'
import { applyAppTheme } from './lib/theme'

export default function App(): JSX.Element {
  const [state, setState] = useState<AppState>(initialState)
  const stateRef = useRef(state)
  stateRef.current = state

  const [sidePanelOpen, setSidePanelOpen] = useState(false)
  const [sidebarCollapsed, setSidebarCollapsed] = useState(false)
  const [sidePanelTab, setSidePanelTab] = useState<SidePanelTab>('subagents')
  const [selectedFleetEntry, setSelectedFleetEntry] = useState<FleetEntry | null>(null)
  const [subagentTree, setSubagentTree] = useState<SubagentNode[]>([])
  const [accessMode, setAccessMode] = useState<AccessMode>('ask')

  useEffect(() => {
    const openSideChat = () => {
      setSidePanelTab('sidechat')
      setSidePanelOpen(true)
    }
    window.addEventListener('prime:open-side-chat', openSideChat)
    return () => window.removeEventListener('prime:open-side-chat', openSideChat)
  }, [])

  useEffect(() => {
    const media = window.matchMedia('(prefers-color-scheme: dark)')
    const apply = () => applyAppTheme(state.settings, media.matches)
    apply()
    media.addEventListener('change', apply)
    return () => media.removeEventListener('change', apply)
  }, [state.settings])

  const mutate = useCallback((fn: (s: AppState) => AppState) => {
    setState((prev) => fn(prev))
  }, [])

  useEffect(() => {
    const cleanups: (() => void)[] = []

    void window.prime.initial().then((init) => {
      const data = init as {
        tabs: { id: string; path: string; name: string }[]
        activeTabId: string | null
        settings: AppState['settings']
        binary: AppState['binary']
      }
      mutate((s) => ({
        ...s,
        ready: true,
        tabs: data.tabs,
        activeTabId: data.activeTabId,
        settings: data.settings,
        binary: data.binary
      }))
      for (const tab of data.tabs) {
        void refreshAgent(`agent-${tab.id}`)
      }
    })

    async function refreshAgent(agentId: string) {
      const [infos, msgs] = await Promise.all([
        window.prime.agentState().catch(() => []),
        window.prime.agentMessages(agentId).catch(() => [])
      ])
      const infoList = infos as { id: string; [k: string]: unknown }[]
      const my = infoList.find((i) => i.id === agentId)
      if (my) {
        mutate((s) => ({ ...s, agents: { ...s.agents, [agentId]: my as never } }))
      }
      if (msgs && Array.isArray(msgs)) {
        const reduced = (msgs as Record<string, unknown>[]).reduce<RenderMessage[]>(mergeMessage, [])
        mutate((s) => ({ ...s, messages: { ...s.messages, [agentId]: reduced } }))
      }
    }

    cleanups.push(
      window.prime.onEvent((raw) => {
        const e = raw as PrimeEvent & { payload: Record<string, unknown> }
        handleEvent(e)
      }),
      window.prime.onToast((raw) => {
        const t = raw as Toast
        mutate((s) => ({ ...s, toasts: [...s.toasts.slice(-4), t] }))
        setTimeout(() => {
          mutate((s) => ({ ...s, toasts: s.toasts.filter((x) => x.id !== t.id) }))
        }, 5000)
      }),
      window.prime.onMenuOpenFolder(() => {
        void openFolder()
      })
    )

    return () => cleanups.forEach((c) => c())
  }, [])

  async function handleEvent(e: PrimeEvent & { payload: Record<string, unknown> }) {
    const { agentId, type, payload } = e

    switch (type) {
      case 'binary_state': {
        mutate((s) => ({ ...s, binary: payload as never }))
        return
      }
      case 'agent_info': {
        const info = payload as never
        mutate((s) => ({ ...s, agents: { ...s.agents, [agentId]: info } }))
        return
      }
      case 'message_update': {
        const ev = payload.assistantMessageEvent as Record<string, unknown> | undefined
        const msg = payload.message as Record<string, unknown> | undefined
        if (msg) {
          mutate((s) => {
            const cur = s.messages[agentId] ?? []
            const norm = mergeMessage([], msg)[0]
            if (ev?.type === 'text_delta' || ev?.type === 'thinking_delta') {
              const merged = mergeMessage(cur, { ...msg })
              const idx = merged.findIndex((m) => m.id === norm.id)
              if (idx >= 0) merged[idx] = { ...merged[idx], streaming: true }
              return { ...s, messages: { ...s.messages, [agentId]: merged } }
            }
            if (ev?.type === 'done' || ev?.type === 'error') {
              const merged = mergeMessage(cur, msg)
              const idx = merged.findIndex((m) => m.id === norm.id)
              if (idx >= 0) merged[idx] = { ...merged[idx], streaming: false }
              return { ...s, messages: { ...s.messages, [agentId]: merged } }
            }
            return { ...s, messages: { ...s.messages, [agentId]: mergeMessage(cur, msg) } }
          })
        }
        return
      }
      case 'message_start':
      case 'message_end': {
        const msg = payload.message as Record<string, unknown> | undefined
        if (msg) {
          mutate((s) => ({
            ...s,
            messages: { ...s.messages, [agentId]: mergeMessage(s.messages[agentId] ?? [], msg) }
          }))
        }
        return
      }
      case 'custom_message': {
        if (payload.customType === 'agent_message' && payload.display !== false) {
          mutate((s) => ({
            ...s,
            messages: { ...s.messages, [agentId]: mergeMessage(s.messages[agentId] ?? [], { ...payload, role: 'assistant' }) }
          }))
        }
        return
      }
      case 'turn_end': {
        const msg = payload.message as Record<string, unknown> | undefined
        const results = payload.toolResults as Record<string, unknown>[] | undefined
        mutate((s) => {
          let messages = s.messages[agentId] ?? []
          if (msg) messages = mergeMessage(messages, msg)
          if (results) {
            for (const r of results) {
              const rmsg: Record<string, unknown> = {
                id: `tr-${r.toolCallId}`,
                role: 'toolResult',
                toolCallId: r.toolCallId,
                toolName: r.toolName,
                content: r.content,
                isError: r.isError
              }
              messages = mergeMessage(messages, rmsg)
              const tool = messages.find((m) => m.id === `tr-${r.toolCallId}`)
              if (tool) {
                const blocks = Array.isArray(tool.content) ? [...tool.content] : []
                void blocks
              }
            }
          }
          return {
            ...s,
            messages: { ...s.messages, [agentId]: messages },
            toolExecs: { ...s.toolExecs, [agentId]: finishToolExecs(s.toolExecs[agentId] ?? {}, results) }
          }
        })
        void refreshStats(agentId)
        return
      }
      case 'tool_execution_start':
      case 'tool_execution_update':
      case 'tool_execution_end': {
        const kind = type === 'tool_execution_start' ? 'start' : type === 'tool_execution_update' ? 'update' : 'end'
        mutate((s) => ({
          ...s,
          toolExecs: {
            ...s.toolExecs,
            [agentId]: patchToolExecs(s.toolExecs[agentId] ?? {}, kind, payload)
          }
        }))
        return
      }
      case 'extension_ui_request': {
        const method = payload.method as string
        if (['confirm', 'select', 'input', 'editor'].includes(method)) {
          const dialog = {
            id: payload.id as string,
            agentId,
            method: method as 'confirm' | 'select' | 'input' | 'editor',
            title: (payload.title as string) ?? method,
            message: payload.message as string | undefined,
            options: payload.options as string[] | undefined,
            prefill: payload.prefill as string | undefined
          }
          mutate((s) => ({
            ...s,
            dialogs: { ...s.dialogs, [agentId]: [...(s.dialogs[agentId] ?? []), dialog] }
          }))
        }
        return
      }
      case 'compaction_end': {
        // Inject a system compaction marker into the active agent's messages
        const compactMsg: RenderMessage = {
          id: `compact-${Date.now()}`,
          role: 'system',
          content: 'Context automatically compacted',
          compaction: true
        } as RenderMessage & { compaction: boolean }
        mutate((s) => ({
          ...s,
          messages: { ...s.messages, [agentId]: [...(s.messages[agentId] ?? []), compactMsg] }
        }))
        return
      }
      case 'session_action_update': {
        return
      }
      case 'fleet_event': {
        const entry: FleetEntry = {
          id: `${Date.now()}-${Math.random().toString(36).slice(2, 6)}`,
          at: Date.now(),
          agentId,
          label: (payload.type as string) ?? 'event',
          text: summarizeEvent(payload),
          payload,
          ownerAgentId: agentId,
          ownerSessionId: stateRef.current.agents[agentId]?.sessionId ?? null
        }
        mutate((s) => ({ ...s, fleet: [...s.fleet.slice(-199), entry] }))
        return
      }
      default:
        return
    }
  }

  async function refreshStats(agentId: string) {
    const stats = await window.prime.agentStats(agentId).catch(() => null)
    if (stats) {
      void stats
    }
  }

  async function openFolder() {
    const path = await window.prime.chooseFolder()
    if (!path) return
    const res = await window.prime.tabAdd(path)
    const tabs = res.tabs
    mutate((s) => ({
      ...s,
      tabs,
      activeTabId: res.activeTabId,
      agents: s.agents,
      messages: { ...s.messages, [`agent-${res.activeTabId}`]: [] },
      activeAgentId: `agent-${res.activeTabId}`
    }))
  }

  const activeAgentId = useMemo(() => {
    if (state.activeTabId) return `agent-${state.activeTabId}`
    const first = Object.keys(state.agents)[0]
    return first ?? null
  }, [state.activeTabId, state.agents])

  const activeInfo = activeAgentId ? state.agents[activeAgentId] : null
  const activeTab = state.tabs.find((t) => t.id === state.activeTabId) ?? null
  const activeFleet = useMemo(() => {
    if (!activeAgentId) return []
    return state.fleet.filter((entry) => (
      entry.ownerAgentId === activeAgentId
      && (!entry.ownerSessionId || entry.ownerSessionId === activeInfo?.sessionId)
    ))
  }, [state.fleet, activeAgentId, activeInfo?.sessionId])

  useEffect(() => {
    if (!activeAgentId) {
      setSubagentTree([])
      return
    }
    let disposed = false
    let latestLoad = 0
    const load = () => {
      const requestId = ++latestLoad
      void window.prime.fleetTree(activeAgentId)
        .then((tree: SubagentNode[]) => {
          if (!disposed && requestId === latestLoad) setSubagentTree(tree)
        })
        .catch(() => {})
    }
    setSelectedFleetEntry(null)
    setSubagentTree([])
    load()
    const timer = window.setInterval(load, 2500)
    const off = window.prime.onEvent((raw) => {
      const event = raw as { agentId?: string; type?: string }
      if (event.agentId !== activeAgentId) return
      if (
        event.type === 'tool_execution_start'
        || event.type === 'tool_execution_end'
        || event.type === 'custom_message'
        || event.type === 'turn_end'
        || event.type === 'session_replaced'
        || event.type === 'session_resynced'
      ) load()
    })
    return () => {
      disposed = true
      latestLoad += 1
      window.clearInterval(timer)
      off()
    }
  }, [activeAgentId, activeInfo?.sessionId])

  if (!state.ready) {
    return <div className="boot">Prime<span className="boot-dot" /></div>
  }

  const noTabs = state.tabs.length === 0

  return (
    <div className="app">
      <Sidebar
        state={state}
        activeAgentId={activeAgentId}
        collapsed={sidebarCollapsed}
        onToggle={() => setSidebarCollapsed((v) => !v)}
        onView={(v) => mutate((s) => ({ ...s, view: v }))}
        onNewChat={() => {
          mutate((s) => ({ ...s, view: 'chat' }))
          if (activeAgentId) {
            void window.prime.agentCommand(activeAgentId, { type: 'new_session' } as never)
          }
        }}
        onSelectTab={(id) => {
          mutate((s) => ({ ...s, activeTabId: id, view: 'chat' }))
          void window.prime.tabSelect(id)
        }}
        onCloseTab={(id) => {
          void window.prime.tabRemove(id).then((res) => {
            mutate((s) => ({ ...s, tabs: res.tabs, activeTabId: res.activeTabId }))
          })
        }}
        onOpenFolder={() => void openFolder()}
      />
      <div className="app-main">
        <TabBar
          inspectorOpen={sidePanelOpen}
          onToggleInspector={() => setSidePanelOpen((open) => !open)}
          sidebarCollapsed={sidebarCollapsed}
          onToggleSidebar={() => setSidebarCollapsed((value) => !value)}
          onNewChat={() => {
            mutate((s) => ({ ...s, view: 'chat' }))
            if (activeAgentId) {
              void window.prime.agentCommand(activeAgentId, { type: 'new_session' } as never)
            }
          }}
          onOpenProject={() => void openFolder()}
          projectName={activeTab?.name}
        />
        <div className="app-body">
          {noTabs ? (
            <WelcomeScreen onOpen={() => void openFolder()} binary={state.binary} onInstall={() => void window.prime.binaryInstall()} />
          ) : (
            <>
              <main className="main-pane">
                {(state.view === 'chat' || state.view === 'autonomy') && activeAgentId && (
                  <ChatView
                    agentId={activeAgentId}
                    info={activeInfo}
                    tab={activeTab}
                    projects={state.tabs}
                    accessMode={accessMode}
                    onAccessModeChange={setAccessMode}
                    rlmMaxDepth={state.settings.rlmMaxDepth ?? 1}
                    onDepthChange={(depth) => {
                      mutate((s) => ({ ...s, settings: { ...s.settings, rlmMaxDepth: depth } }))
                      void window.prime.rlmSet(activeAgentId, depth, true)
                    }}
                    onNavigate={(view) => mutate((s) => ({ ...s, view }))}
                    onOpenGit={() => {
                      setSidePanelTab('git')
                      setSidePanelOpen(true)
                    }}
                    subagents={subagentTree}
                    showSubagentCard={!sidePanelOpen}
                    onOpenSubagents={() => {
                      setSelectedFleetEntry(null)
                      setSidePanelTab('subagents')
                      setSidePanelOpen(true)
                    }}
                    onSelectProject={(projectId) => {
                      mutate((s) => ({ ...s, activeTabId: projectId, view: 'chat' }))
                      void window.prime.tabSelect(projectId)
                    }}
                    onNewProject={() => void openFolder()}
                    showReasoning={state.settings.showReasoning !== false}
                    onToast={(text, kind = 'info') => {
                      const t = { id: `t-${Date.now()}`, kind, text }
                      mutate((s) => ({ ...s, toasts: [...s.toasts.slice(-4), t] }))
                      setTimeout(() => {
                        mutate((s) => ({ ...s, toasts: s.toasts.filter((x) => x.id !== t.id) }))
                      }, 4000)
                    }}
                    onOpenSubagent={(entry) => {
                      const stored = findFleetEntry(activeFleet, entry)
                      const node = findSubagentNode(subagentTree, entry)
                      const merged = stored ? {
                        ...stored,
                        ...entry,
                        parentText: entry.parentText || stored.parentText,
                        childText: entry.childText || stored.childText
                      } : entry
                      setSelectedFleetEntry({
                        ...merged,
                        payload: {
                          ...(merged.payload ?? {}),
                          ...(node ? { activeSessionId: node.activeSessionId, sessionId: node.sessionId, name: node.name, task: node.task } : {})
                        }
                      })
                      setSidePanelTab('subagents')
                      setSidePanelOpen(true)
                    }}
                    onSubagentActivity={(entry) => {
                      const scopedEntry: FleetEntry = {
                        ...entry,
                        ownerAgentId: activeAgentId,
                        ownerSessionId: activeInfo?.sessionId ?? null
                      }
                      mutate((s) => {
                        const scopedFleet = s.fleet.filter((item) => (
                          item.ownerAgentId === activeAgentId && item.ownerSessionId === scopedEntry.ownerSessionId
                        ))
                        const matched = findFleetEntry(scopedFleet, scopedEntry)
                        const index = matched ? s.fleet.indexOf(matched) : -1
                        if (index < 0) return { ...s, fleet: [...s.fleet, scopedEntry] }
                        const fleet = [...s.fleet]
                        fleet[index] = {
                          ...fleet[index],
                          ...scopedEntry,
                          parentText: scopedEntry.parentText || fleet[index].parentText,
                          childText: scopedEntry.childText || fleet[index].childText,
                          payload: { ...(fleet[index].payload ?? {}), ...(scopedEntry.payload ?? {}) }
                        }
                        return { ...s, fleet }
                      })
                      setSelectedFleetEntry((current) => {
                        if (!current || !entriesReferToSameAgent(current, entry)) return current
                        return {
                          ...current,
                          ...entry,
                          parentText: entry.parentText || current.parentText,
                          childText: entry.childText || current.childText,
                          payload: { ...(entry.payload ?? {}), ...(current.payload ?? {}) }
                        }
                      })
                    }}
                  />
                )}
                {state.view === 'fleet' && <FleetView state={state} />}
                {state.view === 'approval' && <ApprovalView activeAgentId={activeAgentId} projectPath={activeTab?.path ?? null} />}
                {state.view === 'dashboard' && <DashboardView />}
                {state.view === 'skills' && <SkillsView activeAgentId={activeAgentId} />}
                {state.view === 'diagnostics' && <DiagnosticsView activeAgentId={activeAgentId} />}
                {state.view === 'settings' && (
                  <SettingsView
                    settings={state.settings}
                    activeAgentId={activeAgentId}
                    onChange={(patch) => {
                      mutate((s) => ({ ...s, settings: { ...s.settings, ...patch } }))
                      void window.prime.settingsSet(patch)
                    }}
                  />
                )}
              </main>
              <SidePanel
                open={sidePanelOpen}
                onToggle={() => setSidePanelOpen((v) => !v)}
                fleet={activeFleet}
                tree={subagentTree}
                agentId={activeAgentId}
                activeTab={sidePanelTab}
                onTabChange={setSidePanelTab}
                selectedEntry={selectedFleetEntry}
                onSelectEntry={setSelectedFleetEntry}
                showReasoning={state.settings.showReasoning !== false}
              />
            </>
          )}
        </div>
      </div>
      <Toasts toasts={state.toasts} />
      <DialogHost dialogs={Object.values(state.dialogs).flat()} onRespond={(id, value, cancelled) => {
        void window.prime.dialogRespond(id, value, cancelled)
        mutate((s) => {
          const dialogs = { ...s.dialogs }
          for (const k of Object.keys(dialogs)) {
            dialogs[k] = dialogs[k].filter((d) => d.id !== id)
          }
          return { ...s, dialogs }
        })
      }} />
    </div>
  )
}

function findSubagentNode(tree: SubagentNode[], entry: FleetEntry): SubagentNode | null {
  const ids = entryIds(entry)
  const exact = findSubagentNodeByIds(tree, ids)
  if (exact || !isLegacyEntry(entry)) return exact
  const named = flattenSubagentNodes(tree).filter((node) => node.name === entry.label)
  return named.length === 1 ? named[0] : null
}

function findSubagentNodeByIds(tree: SubagentNode[], ids: Set<string>): SubagentNode | null {
  for (const node of tree) {
    if (ids.has(node.id) || ids.has(node.sessionId) || (node.activeSessionId ? ids.has(node.activeSessionId) : false)) return node
    const child = findSubagentNodeByIds(node.children, ids)
    if (child) return child
  }
  return null
}

function flattenSubagentNodes(tree: SubagentNode[]): SubagentNode[] {
  return tree.flatMap((node) => [node, ...flattenSubagentNodes(node.children)])
}

function entryIds(entry: FleetEntry): Set<string> {
  return new Set(
    [entry.agentId, entry.payload?.activeSessionId, entry.payload?.sessionId]
      .filter((value): value is string => typeof value === 'string' && value.length > 0)
  )
}

function isLegacyEntry(entry: FleetEntry): boolean {
  return !entry.agentId || entry.agentId === 'subagent' || entry.agentId === entry.label
}

function entriesReferToSameAgent(left: FleetEntry, right: FleetEntry): boolean {
  const rightIds = entryIds(right)
  if ([...entryIds(left)].some((id) => rightIds.has(id))) return true
  return isLegacyEntry(left) && isLegacyEntry(right) && left.label === right.label
}

function findFleetEntry(fleet: FleetEntry[], entry: FleetEntry): FleetEntry | undefined {
  const exact = fleet.find((candidate) => entriesReferToSameAgent(candidate, entry) && (!isLegacyEntry(candidate) || !isLegacyEntry(entry)))
  if (exact || !isLegacyEntry(entry)) return exact
  const named = fleet.filter((candidate) => isLegacyEntry(candidate) && candidate.label === entry.label)
  return named.length === 1 ? named[0] : undefined
}

function summarizeEvent(payload: Record<string, unknown>): string {
  const message = payload.message ?? payload.text ?? payload.summary ?? payload.result ?? payload.status
  if (typeof message === 'string') return message
  const name = payload.name ?? payload.agentName ?? payload.taskName ?? payload.sessionName
  return typeof name === 'string' ? name : String(payload.type ?? 'Subagent activity')
}

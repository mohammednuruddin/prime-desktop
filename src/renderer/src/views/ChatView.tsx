import { useCallback, useEffect, useRef, useState } from 'react'
import type { AgentInfo, GoalState, ProjectTab, SubagentNode, ViewId } from '@shared/types'
import type { GitStatus } from '@shared/types'
import { parseModelList, type ModelOption } from '@shared/models'
import { dispatchSlash, type SlashOverlayId } from '@shared/slash'
import Composer from '../components/Composer'
import CloudMark from '../components/CloudMark'
import MessageItem from '../components/MessageItem'
import SubagentMark from '../components/SubagentMark'
import SlashOverlay from '../components/SlashOverlay'
import HarnessTray from '../components/HarnessTray'
import type { Block, FleetEntry, RenderMessage, ToolExecState } from '../lib/store'
import { mergeMessage as merge, finishToolExecs, patchToolExecs } from '../lib/store'
import type { AccessMode } from '../components/AccessPicker'
import { isInternalStateRestoreMessage } from '@shared/messageVisibility'

function renderMessages(items: unknown[]): RenderMessage[] {
  return (items as Record<string, unknown>[])
    .filter((item) => !isInternalStateRestoreMessage(item))
    .reduce((acc, item) => merge(acc, item), [] as RenderMessage[])
}

interface Props {
  agentId: string
  info: AgentInfo | null
  tab: ProjectTab | null
  projects?: ProjectTab[]
  accessMode?: AccessMode
  onAccessModeChange?: (mode: AccessMode) => void
  onOpenSubagent?: (entry: FleetEntry) => void
  onSubagentActivity?: (entry: FleetEntry) => void
  onNavigate?: (view: ViewId) => void
  onOpenGit?: () => void
  onSelectProject?: (projectId: string) => void
  onNewProject?: () => void
  subagents?: SubagentNode[]
  onOpenSubagents?: () => void
  showSubagentCard?: boolean
  onToast?: (text: string, kind?: 'info' | 'success' | 'warning' | 'error') => void
  rlmMaxDepth?: number
  showReasoning?: boolean
  onDepthChange?: (depth: number) => void
}

export default function ChatView({ agentId, info, tab: _tab, projects = [], accessMode = 'ask', onAccessModeChange, onOpenSubagent, onSubagentActivity, onNavigate, onOpenGit, onSelectProject, onNewProject, subagents = [], onOpenSubagents, showSubagentCard = true, onToast, rlmMaxDepth = 1, showReasoning = true, onDepthChange }: Props): JSX.Element {
  const [messages, setMessages] = useState<RenderMessage[]>([])
  const [toolExecs, setToolExecs] = useState<Record<string, ToolExecState>>({})
  const [commands, setCommands] = useState<{ name: string; description?: string }[]>([])
  const [models, setModels] = useState<ModelOption[]>([])
  const [branch, setBranch] = useState<string | null>(null)
  const [showJumpToLatest, setShowJumpToLatest] = useState(false)
  const [awaitingResponse, setAwaitingResponse] = useState(false)
  const scrollRef = useRef<HTMLDivElement>(null)
  const followLatestRef = useRef(true)

  useEffect(() => {
    followLatestRef.current = true
    setShowJumpToLatest(false)
    setAwaitingResponse(false)
    setMessages([])
    setToolExecs({})
    void window.prime.agentMessages(agentId).then((msgs) => {
      setMessages(renderMessages(msgs))
    })
    void window.prime.agentCommands(agentId).then((cmds) => {
      setCommands(
        (cmds as { name: string; description?: string }[]).map((c) => ({ name: c.name, description: c.description }))
      )
    })
    const loadModels = () => {
      void window.prime.agentCommand(agentId, { type: 'get_available_models' }).then((res) => {
        setModels(parseModelList(res))
      }).catch((err) => {
        console.error('get_available_models failed', err)
      })
    }
    loadModels()
  }, [agentId])

  useEffect(() => {
    const loadBranch = () => {
      void window.prime.gitStatus(agentId)
        .then((status: GitStatus) => setBranch(status.isRepo ? status.branch : null))
        .catch(() => setBranch(null))
    }
    loadBranch()
    const off = window.prime.onEvent((raw) => {
      const event = raw as { agentId?: string; type?: string }
      if (event.agentId === agentId && event.type === 'turn_end') loadBranch()
    })
    const timer = window.setInterval(loadBranch, 5000)
    return () => {
      window.clearInterval(timer)
      off()
    }
  }, [agentId])

  useEffect(() => {
    if (models.length > 0) return
    if (info?.status !== 'idle' && info?.status !== 'working') return
    void window.prime.agentCommand(agentId, { type: 'get_available_models' }).then((res) => {
      setModels(parseModelList(res))
    }).catch(() => {})
  }, [agentId, info?.status, info?.model, models.length])

  useEffect(() => {
    const off = window.prime.onEvent((raw) => {
      const e = raw as { agentId?: string; type?: string; payload?: Record<string, unknown> }
      if (e.agentId !== agentId) return
      const p = e.payload ?? {}
      switch (e.type) {
        case 'message_update': {
          const msg = p.message as Record<string, unknown> | undefined
          if (!msg) return
          if (isInternalStateRestoreMessage(msg)) {
            setMessages((prev) => prev.filter((message) => message.id !== msg.id))
            break
          }
          const ev = p.assistantMessageEvent as Record<string, unknown> | undefined
          if (ev?.type === 'text_delta' || ev?.type === 'thinking_delta' || ev?.type === 'toolcall_delta') {
            setAwaitingResponse(false)
          }
          setMessages((prev) => {
            const next = merge(prev, msg)
            const idx = next.findIndex((m) => m.id === msg.id)
            if (idx >= 0) {
              next[idx] = {
                ...next[idx],
                streaming: ev?.type === 'text_delta' || ev?.type === 'thinking_delta' || ev?.type === 'toolcall_delta'
              }
            }
            return next
          })
          break
        }
        case 'message_start':
        case 'message_end': {
          const msg = p.message as Record<string, unknown> | undefined
          if (e.type === 'message_end' && msg?.role === 'assistant') setAwaitingResponse(false)
          if (msg && isInternalStateRestoreMessage(msg)) {
            setMessages((prev) => prev.filter((message) => message.id !== msg.id))
          } else if (msg) {
            setMessages((prev) => merge(prev, msg))
          }
          if (e.type === 'message_end' && msg?.role === 'toolResult') {
            setToolExecs((prev) => patchToolExecs(prev, 'end', msg))
          }
          break
        }
        case 'custom_message': {
          if (p.display === false) break
          if (isInternalStateRestoreMessage(p)) break
          if (p.customType === 'agent_message') {
            setAwaitingResponse(false)
            setMessages((prev) => merge(prev, { ...p, role: 'assistant' }))
          } else if (
            p.customType === 'session_slash_command' ||
            p.customType === 'session_slash_command_result' ||
            p.customType === 'compaction_outcome'
          ) {
            setMessages((prev) => merge(prev, {
              id: `sys-${String(p.customType)}-${String(p.timestamp ?? Date.now())}`,
              role: 'system',
              content: String(p.content ?? '')
            }))
          }
          break
        }
        case 'session_resumed':
        case 'session_replaced':
        case 'session_resynced': {
          void window.prime.agentMessages(agentId).then((items) => {
            setMessages(renderMessages(items))
          })
          break
        }
        case 'session_started': {
          setMessages([])
          setToolExecs({})
          setAwaitingResponse(false)
          break
        }
        case 'turn_end': {
          setAwaitingResponse(false)
          const msg = p.message as Record<string, unknown> | undefined
          const results = p.toolResults as Record<string, unknown>[] | undefined
          setMessages((prev) => {
            let next = prev
            if (msg && !isInternalStateRestoreMessage(msg)) next = merge(next, msg)
            if (results) {
              for (const r of results) {
                next = merge(next, {
                  id: `tr-${r.toolCallId}`,
                  role: 'toolResult',
                  toolCallId: r.toolCallId,
                  content: r.content,
                  isError: r.isError
                })
              }
            }
            return next.map((m) => ({ ...m, streaming: false }))
          })
          setToolExecs((prev) => finishToolExecs(prev, results))
          break
        }
        case 'agent_end': {
          setAwaitingResponse(false)
          setToolExecs((prev) => finishToolExecs(prev))
          setMessages((prev) => prev.map((m) => (m.streaming ? { ...m, streaming: false } : m)))
          break
        }
        case 'tool_execution_start': {
          setToolExecs((prev) => patchToolExecs(prev, 'start', p))
          break
        }
        case 'tool_execution_update': {
          setToolExecs((prev) => patchToolExecs(prev, 'update', p))
          break
        }
        case 'tool_execution_end': {
          setToolExecs((prev) => patchToolExecs(prev, 'end', p))
          break
        }
      }
    })
    return off
  }, [agentId])

  useEffect(() => {
    const el = scrollRef.current
    if (!el || !followLatestRef.current) return
    const frame = requestAnimationFrame(() => {
      el.scrollTop = el.scrollHeight
    })
    return () => cancelAnimationFrame(frame)
  }, [messages, toolExecs])

  const handleMessagesScroll = useCallback(() => {
    const el = scrollRef.current
    if (!el) return
    const isAtBottom = el.scrollHeight - el.scrollTop - el.clientHeight <= 40
    followLatestRef.current = isAtBottom
    setShowJumpToLatest(!isAtBottom)
  }, [])

  const jumpToLatest = useCallback(() => {
    const el = scrollRef.current
    if (!el) return
    followLatestRef.current = true
    setShowJumpToLatest(false)
    el.scrollTo({ top: el.scrollHeight, behavior: 'smooth' })
  }, [])

  useEffect(() => {
    if (!onSubagentActivity) return
    for (const message of messages) {
      if (!Array.isArray(message.content)) continue
      for (const block of message.content as Block[]) {
        const entry = activityFromBlock(block)
        if (entry) onSubagentActivity(entry)
      }
    }
  }, [messages])

  const busy = info?.isStreaming === true || info?.status === 'working'

  useEffect(() => {
    if (busy) return
    setAwaitingResponse(false)
    setToolExecs((prev) => finishToolExecs(prev))
    setMessages((prev) => {
      if (!prev.some((m) => m.streaming)) return prev
      return prev.map((m) => (m.streaming ? { ...m, streaming: false } : m))
    })
  }, [busy])

  const handleSend = useCallback(
    (text: string, images: { type: 'image'; data: string; mimeType: string }[]) => {
      if (!busy) {
        followLatestRef.current = true
        setShowJumpToLatest(false)
        setAwaitingResponse(true)
      }
      void window.prime.agentCommand(agentId, {
        type: 'prompt',
        message: text,
        images: images.length ? images : undefined,
        streamingBehavior: busy ? 'steer' : undefined
      } as never).catch((err) => {
        setAwaitingResponse(false)
        console.error('send failed', err)
        onToast?.(err instanceof Error ? err.message : String(err), 'error')
      })
    },
    [agentId, busy, onToast]
  )

  const handleAbort = useCallback(
    () => void window.prime.agentCommand(agentId, { type: 'abort' } as never).catch(() => {}),
    [agentId]
  )

  const handleBash = useCallback(
    (cmd: string) => {
      void window.prime.agentCommand(agentId, { type: 'bash', command: cmd } as never).then(() => {
        void window.prime.agentMessages(agentId).then((msgs) => {
          setMessages(renderMessages(msgs))
        })
      }).catch((err) => console.error(err))
    },
    [agentId]
  )

  const pickModel = (name: string) => {
    const slash = name.indexOf('/')
    const provider = slash >= 0 ? name.slice(0, slash) : undefined
    const id = slash >= 0 ? name.slice(slash + 1) : name
    void window.prime.agentCommand(agentId, { type: 'set_model', provider, modelId: id } as never).catch(() => {})
  }

  const sendQuickPrompt = (prompt: string) => {
    handleSend(prompt, [])
  }

  const [effortLevel, setEffortLevel] = useState<string>('High')
  const [overlay, setOverlay] = useState<{ id: SlashOverlayId; args: string } | null>(null)
  const [openPicker, setOpenPicker] = useState<'models' | 'effort' | 'depth' | null>(null)
  const [goal, setGoal] = useState<GoalState | null>(null)

  useEffect(() => {
    setGoal(null)
    void window.prime.agentHarness(agentId, 'goal_state').then((result) => {
      setGoal((result as { goal?: GoalState | null }).goal ?? null)
    }).catch(() => {})
    const off = window.prime.onEvent((raw) => {
      const event = raw as { agentId?: string; type?: string; payload?: Record<string, unknown> }
      if (event.agentId !== agentId || event.type !== 'goal_update') return
      setGoal((event.payload?.goal ?? event.payload) as GoalState | null)
    })
    return off
  }, [agentId])

  const note = useCallback((text: string) => {
    setMessages((prev) => [...prev, { id: `cmd-${Date.now()}`, role: 'system', content: text }])
  }, [])

  const cmd = useCallback((payload: Record<string, unknown>) => {
    return window.prime.agentCommand(agentId, payload as never)
  }, [agentId])

  const reloadMessages = useCallback(() => {
    void window.prime.agentMessages(agentId).then((msgs) => {
      setMessages(renderMessages(msgs))
    })
  }, [agentId])

  const handleSelectEffort = (effort: string) => {
    setEffortLevel(effort)
    void cmd({ type: 'set_thinking_level', level: effort.toLowerCase() }).catch(() => {})
  }

  const handleSlash = useCallback(async (raw: string) => {
    const action = dispatchSlash(raw)
    if (!action) return
    try {
      switch (action.action) {
        case 'pass-through':
        case 'prompt':
          if (action.action === 'prompt' && action.toast) onToast?.(action.toast, 'info')
          handleSend(action.action === 'prompt' ? action.message : action.message, [])
          return
        case 'navigate':
          onNavigate?.(action.view)
          if (action.toast) onToast?.(action.toast, 'info')
          return
        case 'overlay':
          if (action.overlay === 'model') { setOpenPicker('models'); return }
          if (action.overlay === 'effort') {
            if (action.args) { handleSelectEffort(action.args); note(`Effort set to ${action.args}`); return }
            setOpenPicker('effort')
            return
          }
          if (action.overlay === 'depth') { setOpenPicker('depth'); return }
          setOverlay({ id: action.overlay, args: action.args })
          return
        case 'copy': {
          const res = await cmd({ type: 'get_last_assistant_text' }) as { text?: string }
          const text = res.text ?? ''
          if (!text) { note('No assistant message to copy'); return }
          await navigator.clipboard.writeText(text)
          note('Copied last assistant message')
          return
        }
        case 'quit':
          void window.prime.quit()
          return
        case 'new-session':
          await cmd({ type: 'new_session' })
          setMessages([])
          if (action.prompt) handleSend(action.prompt, [])
          else note('Started a new session')
          return
        case 'reload':
          await window.prime.agentHarness(agentId, 'reload')
          void window.prime.agentCommands(agentId).then((cmds) => {
            setCommands((cmds as { name: string; description?: string }[]).map((c) => ({ name: c.name, description: c.description })))
          })
          note(action.toast ?? 'Reloaded')
          return
        case 'fullscreen':
          void window.prime.windowFullscreen(action.mode)
          return
        case 'export': {
          const path = action.path || await window.prime.chooseSave('session.html')
          if (!path) return
          const out = await window.prime.exportHtml(agentId, path)
          note(`Exported to ${out || path}`)
          return
        }
        case 'share': {
          const res = await window.prime.shareSession(agentId) as { previewUrl: string; gistUrl: string }
          await navigator.clipboard.writeText(res.previewUrl)
          note(`Share URL copied: ${res.previewUrl}`)
          return
        }
        case 'compact':
          await cmd({ type: 'compact', customInstructions: action.instructions })
          note('Compacting context…')
          return
        case 'refine':
          await cmd({ type: 'refine', instructions: action.instructions, rollbackId: action.rollbackId, global: action.global })
          note('Refining harness state…')
          return
        case 'clone':
          await cmd({ type: 'clone' })
          reloadMessages()
          note('Cloned this session')
          return
        case 'name':
          await cmd({ type: 'set_session_name', name: action.name })
          note(`Session named “${action.name}”`)
          return
        case 'depth':
          onDepthChange?.(action.value)
          note(`RLM max depth set to ${action.value}${action.global ? ' (global)' : ''}`)
          return
        case 'heartbeat-action': {
          if (action.verb === 'status') {
            const hb = await window.prime.fleetHeartbeat(agentId) as { schedule?: string; prompt?: string; status?: string } | null
            note(hb ? `Heartbeat ${hb.status ?? 'active'}: ${hb.schedule} — ${hb.prompt}` : 'No heartbeat set')
            return
          }
          if (action.verb === 'set') {
            setOverlay({ id: 'heartbeat', args: action.args })
            return
          }
          await window.prime.fleetHeartbeatAction(agentId, action.verb === 'stop' ? 'clear' : action.verb)
          note(`Heartbeat ${action.verb}`)
          return
        }
        case 'logs':
          void window.prime.logsReveal()
          note('Revealed logs folder')
          return
        case 'import': {
          const path = action.path || await window.prime.chooseFile()
          if (!path) return
          await window.prime.agentHarness(agentId, 'import', { path })
          reloadMessages()
          note(`Imported ${path}`)
          return
        }
        case 'btw': {
          window.dispatchEvent(new CustomEvent('prime:open-side-chat', { detail: { question: action.question } }))
          return
        }
        case 'fast': {
          const result = await window.prime.agentHarness(agentId, 'fast') as { enabled?: boolean }
          note(`Fast mode ${result.enabled ? 'on' : 'off'}`)
          return
        }
        case 'update': {
          note('Updating Prime Agent…')
          const result = await window.prime.agentHarness(agentId, 'update') as { output?: string }
          note(result.output || 'Prime Agent is up to date.')
          return
        }
        case 'notice':
          note(action.text)
          return
        case 'traces':
          if (!action.args || ['status', 'on', 'off'].includes(action.args)) {
            const result = await window.prime.agentHarness(agentId, 'traces', { mode: action.args || 'status' }) as { enabled?: boolean }
            note(`Trace sharing ${result.enabled ? 'on' : 'off'}`)
          } else {
            note('Trace preview and upload require Prime Agent trace credentials; use /traces login in the terminal once, then /traces on here.')
          }
          return
        case 'rpc':
          await cmd(action.command)
          if (action.reload) reloadMessages()
          if (action.toast) note(action.toast)
          return
      }
    } catch (err) {
      note(err instanceof Error ? err.message : String(err))
    }
  }, [agentId, cmd, handleSend, note, onDepthChange, onNavigate, onToast, reloadMessages])

  return (
    <div className="codex-chat-layout">
      {/* Main Messages Area */}
      <div className="codex-messages-area" ref={scrollRef} onScroll={handleMessagesScroll}>
        {messages.length === 0 && !awaitingResponse ? (
          <div className="codex-welcome-center">
            {/* Cloud Icon with Code face */}
            <div className="codex-cloud-icon">
              <CloudMark size={56} />
            </div>
            <h1 className="codex-center-title">
              What should we work on?
            </h1>

            {/* 4 Action Suggestion Cards */}
            <div className="codex-cards-grid">
              <button className="codex-action-card" onClick={() => sendQuickPrompt('Explore and understand code')}>
                <div className="card-icon-wrap rose">
                  <svg viewBox="0 0 24 24" width="18" height="18" fill="none" stroke="currentColor" strokeWidth="1.8">
                    <path d="M11 5.882V19.24a1.76 1.76 0 01-3.417.592l-2.147-6.15M18 5.882V19.24a1.76 1.76 0 003.417.592l2.147-6.15" />
                    <path d="M19.4 15a1.65 1.65 0 00.33-1.82l-2.92-5.84A2 2 0 0015 6h-6a2 2 0 00-1.81 1.34L4.27 13.18A1.65 1.65 0 004.6 15" />
                  </svg>
                </div>
                <span className="card-label">Explore and understand code</span>
              </button>

              <button className="codex-action-card" onClick={() => sendQuickPrompt('Build a new feature, app, or tool')}>
                <div className="card-icon-wrap rose">
                  <svg viewBox="0 0 24 24" width="18" height="18" fill="none" stroke="currentColor" strokeWidth="1.8">
                    <path d="M14.7 6.3a1 1 0 000 1.4l1.6 1.6a1 1 0 001.4 0l3.77-3.77a6 6 0 01-7.94 7.94l-6.91 6.91a2.12 2.12 0 01-3-3l6.91-6.91a6 6 0 017.94-7.94l-3.76 3.76z" />
                  </svg>
                </div>
                <span className="card-label">Build a new feature, app, or tool</span>
              </button>

              <button className="codex-action-card" onClick={() => sendQuickPrompt('Review code and suggest changes')}>
                <div className="card-icon-wrap green">
                  <svg viewBox="0 0 24 24" width="18" height="18" fill="none" stroke="currentColor" strokeWidth="1.8">
                    <path d="M21.5 2v6h-6M21.34 15.57a10 10 0 11-.57-8.38l5.67-5.67" />
                  </svg>
                </div>
                <span className="card-label">Review code and suggest changes</span>
              </button>

              <button className="codex-action-card" onClick={() => sendQuickPrompt('Fix issues and failures')}>
                <div className="card-icon-wrap orange">
                  <svg viewBox="0 0 24 24" width="18" height="18" fill="none" stroke="currentColor" strokeWidth="1.8">
                    <rect x="8" y="6" width="8" height="14" rx="4" />
                    <path d="M6 18h12M6 12h12M6 6h12" />
                  </svg>
                </div>
                <span className="card-label">Fix issues and failures</span>
              </button>
            </div>
          </div>
        ) : (
          <div className="codex-feed-wrapper">
            {messages.map((m, i) => (
              <MessageItem key={m.id ?? i} message={m} toolExecs={toolExecs} onOpenSubagent={onOpenSubagent} showReasoning={showReasoning} />
            ))}
            {awaitingResponse && (
              <div className="assistant-pending" role="status" aria-live="polite">
                <span className="assistant-pending-shimmer">Thinking…</span>
              </div>
            )}
          </div>
        )}
      </div>

      {showSubagentCard && subagents.length > 0 && onOpenSubagents && (
        <SubagentActivityCard tree={subagents} onOpen={onOpenSubagents} />
      )}

      {showJumpToLatest && (
        <button
          className="chat-jump-latest"
          type="button"
          onClick={jumpToLatest}
          aria-label="Jump to latest message"
          title="Jump to latest"
        >
          <svg viewBox="0 0 24 24" aria-hidden="true">
            <path d="M6 9l6 6 6-6" />
          </svg>
        </button>
      )}

      {/* Floating Bottom Composer */}
      <ExtensionSurface info={info} placement="aboveEditor" />
      <HarnessTray
        agentId={agentId}
        busy={busy}
        models={models}
        currentModel={info?.model ?? undefined}
        onSelectModel={pickModel}
        commands={commands}
        effortLevel={effortLevel}
        onSelectEffort={handleSelectEffort}
        accessMode={accessMode}
        onAccessModeChange={onAccessModeChange}
        rlmMaxDepth={rlmMaxDepth}
        onDepthChange={onDepthChange}
        showReasoning={showReasoning}
        onSlash={(text) => void handleSlash(text)}
        onBash={handleBash}
        onToast={onToast}
      />
      <Composer
        busy={busy}
        commands={commands}
        onSend={handleSend}
        onSlash={(text) => void handleSlash(text)}
        onAbort={handleAbort}
        onBash={handleBash}
        models={models}
        currentModel={info?.model ?? undefined}
        onSelectModel={pickModel}
        effortLevel={effortLevel}
        onSelectEffort={handleSelectEffort}
        accessMode={accessMode}
        onAccessModeChange={onAccessModeChange}
        rlmMaxDepth={rlmMaxDepth}
        onDepthChange={onDepthChange}
        openPicker={openPicker}
        onPickerConsumed={() => setOpenPicker(null)}
        projectName={_tab?.name}
        branch={branch}
        projects={projects}
        activeProjectId={_tab?.id}
        onSelectProject={onSelectProject}
        onNewProject={onNewProject}
        onBranchClick={onOpenGit}
        showContext={messages.length === 0}
        externalText={info?.extensionUi?.editorText}
        banner={goal?.objective && goal.status !== 'idle' ? (
          <button
            className={`goal-chip ${goal.status}`}
            type="button"
            onClick={() => setOverlay({ id: 'goal', args: '' })}
          >
            <span className="goal-chip-mark" aria-hidden="true" />
            <span className="goal-chip-status">{goal.status === 'budget_limited' ? 'Budget paused' : goal.status}</span>
            <span className="goal-chip-text">{goal.objective}</span>
            {goal.tokenBudget ? (
              <span className="goal-chip-usage">{Math.round((goal.tokensUsed / goal.tokenBudget) * 100)}%</span>
            ) : null}
          </button>
        ) : undefined}
      />
      <ExtensionSurface info={info} placement="belowEditor" />
      {overlay && (
        <SlashOverlay
          overlay={overlay.id}
          args={overlay.args}
          agentId={agentId}
          effortLevel={effortLevel}
          depth={rlmMaxDepth}
          models={models}
          onClose={() => setOverlay(null)}
          onFork={(entryId) => {
            setOverlay(null)
            void cmd({ type: 'fork', entryId }).then(() => {
              reloadMessages()
              note('Forked a new session from that message')
            }).catch((err: Error) => note(err.message))
          }}
          onTree={(entryId) => {
            setOverlay(null)
            void window.prime.agentHarness(agentId, 'navigate_tree', { targetId: entryId }).then(() => {
              reloadMessages()
              note('Moved to the selected point in the session tree')
            }).catch((err: Error) => note(err.message))
          }}
          onName={(name) => {
            setOverlay(null)
            void cmd({ type: 'set_session_name', name }).then(() => note(`Session named “${name}”`))
          }}
          onResume={(path) => {
            setOverlay(null)
            void window.prime.agentResume(agentId, path).then(() => {
              reloadMessages()
              note('Resumed session')
            })
          }}
          onHeartbeatSet={(schedule, prompt) => {
            setOverlay(null)
            void cmd({ type: 'set_heartbeat', schedule, prompt }).then(() => note(`Heartbeat set: ${schedule}`)).catch((err: Error) => note(err.message))
          }}
          onEffort={(level) => handleSelectEffort(level)}
          onDepth={(value) => onDepthChange?.(value)}
          onOpenModelPicker={() => setOpenPicker('models')}
          onScopedModels={(selected) => {
            setOverlay(null)
            void window.prime.agentHarness(agentId, 'scoped_models', {
              models: selected.map((model) => ({ provider: model.provider, modelId: model.id }))
            }).then(() => note(`Model scope saved (${selected.length})`)).catch((err: Error) => note(err.message))
          }}
        />
      )}
    </div>
  )
}

function ExtensionSurface({ info, placement }: { info: AgentInfo | null; placement: 'aboveEditor' | 'belowEditor' }): JSX.Element | null {
  const ui = info?.extensionUi
  if (!ui) return null
  const widgets = Object.entries(ui.widgets).filter(([, widget]) => widget.placement === placement)
  const statuses = placement === 'belowEditor' ? Object.entries(ui.statuses) : []
  if (widgets.length === 0 && statuses.length === 0) return null
  return (
    <div className={`extension-surface ${placement}`}>
      {widgets.map(([key, widget]) => (
        <div className="extension-widget" key={key}>
          {widget.lines.map((line, index) => <div key={index}>{line}</div>)}
        </div>
      ))}
      {statuses.map(([key, text]) => <span className="extension-status" key={key}>{key}: {text}</span>)}
    </div>
  )
}

function SubagentActivityCard({ tree, onOpen }: { tree: SubagentNode[]; onOpen: () => void }): JSX.Element {
  const all = flattenSubagents(tree)
  const working = all.filter((agent) => agent.status === 'working')
  const visible = working.length > 0 ? working : all
  return (
    <button className="agent-activity-card" type="button" onClick={onOpen} aria-label="Open subagent activity">
      <span className="agent-activity-label">Subagents</span>
      <span className="agent-activity-summary">
        <span className="agent-activity-marks" aria-hidden="true">
          {visible.slice(0, 3).map((agent) => <SubagentMark key={agent.id} seed={agent.id} />)}
        </span>
        <span>{working.length > 0 ? `${working.length} working` : `${all.length} ${all.length === 1 ? 'agent' : 'agents'}`}</span>
        <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M9 6l6 6-6 6" /></svg>
      </span>
    </button>
  )
}

function flattenSubagents(tree: SubagentNode[]): SubagentNode[] {
  return tree.flatMap((node) => [node, ...flattenSubagents(node.children)])
}

function activityFromBlock(block: Block): FleetEntry | null {
  if (block.type === 'subagent') {
    return {
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
  }
  if (block.type !== 'toolCall' || block.name?.toLowerCase() !== 'ipython' || !block.arguments || typeof block.arguments !== 'object') return null
  const code = String((block.arguments as Record<string, unknown>).code ?? '')
  if (!/\b(?:await\s+)?rlm\s*\(/.test(code)) return null
  const name = code.match(/\bname\s*=\s*['"]([^'"]+)['"]/)?.[1] ?? 'Subagent'
  const childId = block.result?.match(/rlm_child_id=['"]([^'"]+)['"]/)?.[1] ?? block.id ?? name
  const task = code.match(/\brlm\s*\(\s*(['"])([\s\S]*?)\1/)?.[2] ?? ''
  return {
    id: childId,
    at: Date.now(),
    agentId: childId,
    label: name,
    text: task,
    parentText: task,
    childText: '',
    status: block.status === 'error' ? 'error' : 'running',
    payload: { name, message: task, status: block.status ?? 'running' }
  }
}

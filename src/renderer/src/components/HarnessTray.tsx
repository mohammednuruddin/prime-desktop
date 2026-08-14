import { useCallback, useEffect, useState } from 'react'
import { createPortal } from 'react-dom'
import type { ActionQueue, SideQuestionTurn } from '@shared/types'
import type { ModelOption } from '@shared/models'
import MessageItem from './MessageItem'
import Composer from './Composer'
import type { AccessMode } from './AccessPicker'
import type { RenderMessage } from '../lib/store'

interface Props {
  agentId: string
  busy: boolean
  models?: ModelOption[]
  currentModel?: string
  onSelectModel?: (model: string) => void
  commands?: { name: string; description?: string }[]
  effortLevel?: string
  onSelectEffort?: (effort: string) => void
  accessMode?: AccessMode
  onAccessModeChange?: (mode: AccessMode) => void
  rlmMaxDepth?: number
  onDepthChange?: (depth: number) => void
  onSlash?: (text: string) => void
  onBash?: (command: string) => void
  showReasoning?: boolean
  onToast?: (text: string, kind?: 'info' | 'success' | 'warning' | 'error') => void
}

interface SideRun {
  id: string
  question: string
  answer: string
  status: 'running' | 'complete' | 'cancelled' | 'error'
  errorMessage?: string
}

const EMPTY_QUEUE: ActionQueue = { steering: [], followUp: [] }

export default function HarnessTray({
  agentId,
  busy,
  models = [],
  currentModel,
  onSelectModel,
  commands = [],
  effortLevel,
  onSelectEffort,
  accessMode,
  onAccessModeChange,
  rlmMaxDepth,
  onDepthChange,
  onSlash,
  onBash,
  showReasoning = true,
  onToast
}: Props): JSX.Element {
  const [queue, setQueue] = useState<ActionQueue>(EMPTY_QUEUE)
  const [open, setOpen] = useState(false)
  const [lane, setLane] = useState<'steering' | 'followUp'>('steering')
  const [queuedText, setQueuedText] = useState('')
  const [sideHost, setSideHost] = useState<HTMLElement | null>(null)
  const [turns, setTurns] = useState<SideQuestionTurn[]>([])
  const [sideRun, setSideRun] = useState<SideRun | null>(null)

  const loadQueue = useCallback(() => {
    void window.prime.agentHarness(agentId, 'queue')
      .then((result) => setQueue(result as ActionQueue))
      .catch(() => {})
  }, [agentId])

  useEffect(() => {
    const attach = () => setSideHost(document.getElementById('side-thread-panel-slot'))
    attach()
    const observer = new MutationObserver(attach)
    observer.observe(document.body, { childList: true, subtree: true })
    return () => observer.disconnect()
  }, [])

  useEffect(() => {
    setQueue(EMPTY_QUEUE)
    setTurns([])
    setSideRun(null)
    loadQueue()
    const timer = window.setInterval(loadQueue, busy ? 1200 : 4000)
    const off = window.prime.onEvent((raw) => {
      const message = raw as { agentId?: string; type?: string; payload?: Record<string, unknown> }
      if (message.agentId !== agentId) return
      if (message.type === 'session_action_update') {
        const actions = message.payload?.actions as { steering?: string[]; followUps?: string[] } | undefined
        setQueue((current) => ({
          ...current,
          steering: actions?.steering ?? [],
          followUp: actions?.followUps ?? []
        }))
      }
      if (message.type === 'side_question_event') {
        const event = (message.payload?.event ?? message.payload) as Record<string, unknown>
        setSideRun({
          id: String(event.id ?? ''),
          question: String(event.question ?? ''),
          answer: String(event.answer ?? ''),
          status: String(event.status ?? 'running') as SideRun['status'],
          errorMessage: typeof event.errorMessage === 'string' ? event.errorMessage : undefined
        })
      }
    })
    return () => {
      window.clearInterval(timer)
      off()
    }
  }, [agentId, busy, loadQueue])

  useEffect(() => {
    if (sideRun?.status !== 'complete') return
    setTurns((current) => {
      const next = { question: sideRun.question, answer: sideRun.answer }
      const prior = current[current.length - 1]
      return prior?.question === next.question && prior.answer === next.answer ? current : [...current, next]
    })
  }, [sideRun])

  const mutate = async (
    targetLane: 'steering' | 'followUp',
    index: number,
    expectedText: string,
    mutation: Record<string, unknown>
  ) => {
    try {
      const result = await window.prime.agentHarness(agentId, 'queue_mutate', {
        lane: targetLane,
        index,
        expectedText,
        mutation
      }) as ActionQueue & { status?: string }
      setQueue(result)
      if (result.status && result.status !== 'applied') {
        onToast?.(result.status === 'unsupported' ? 'Queue editing requires Prime Agent 0.7.2.' : 'Queue changed elsewhere; refreshed.', 'warning')
      }
    } catch (error) {
      onToast?.(error instanceof Error ? error.message : String(error), 'error')
      loadQueue()
    }
  }

  const addQueued = () => {
    const text = queuedText.trim()
    if (!text) return
    const command = lane === 'steering' ? { type: 'steer', message: text } : { type: 'follow_up', message: text }
    void window.prime.agentCommand(agentId, command as never)
      .then(() => {
        setQueuedText('')
        loadQueue()
      })
      .catch((error: Error) => onToast?.(error.message, 'error'))
  }

  const startSide = (questionText: string) => {
    const question = questionText.trim()
    if (!question || sideRun?.status === 'running') return
    const id = `desktop-side-${Date.now()}`
    setSideRun({ id, question, answer: '', status: 'running' })
    void window.prime.agentHarness(agentId, 'side_question_start', { id, question, previousTurns: turns })
      .catch((error: Error) => setSideRun({ id, question, answer: '', status: 'error', errorMessage: error.message }))
  }

  useEffect(() => {
    const handleOpen = (event: Event) => {
      const question = (event as CustomEvent<{ question?: string }>).detail?.question
      if (question) startSide(question)
    }
    window.addEventListener('prime:open-side-chat', handleOpen)
    return () => window.removeEventListener('prime:open-side-chat', handleOpen)
  })

  const count = queue.steering.length + queue.followUp.length
  const sideMessages: RenderMessage[] = turns.flatMap((turn, index) => [
    { id: `side-user-${index}`, role: 'user', content: turn.question },
    { id: `side-assistant-${index}`, role: 'assistant', content: turn.answer }
  ])
  if (sideRun && sideRun.status !== 'complete') {
    sideMessages.push({ id: `side-user-${sideRun.id}`, role: 'user', content: sideRun.question })
    if (sideRun.answer) {
      sideMessages.push({
        id: `side-assistant-${sideRun.id}`,
        role: 'assistant',
        content: sideRun.answer,
        streaming: sideRun.status === 'running'
      })
    }
  }
  return (
    <>
      {count > 0 && (
        <div className="harness-controls" aria-label="Queued session messages">
          <button className={`harness-control ${open ? 'active' : ''}`} type="button" onClick={() => setOpen((value) => !value)}>
            Queued messages · {count}
          </button>
        </div>
      )}

      {open && count > 0 && (
        <section className="harness-popover queue-popover">
          <header>
            <strong>Admission queue</strong>
            <span>{busy ? 'Agent is working' : 'Agent is idle'}</span>
          </header>
          <div className="queue-compose">
            <select value={lane} onChange={(event) => setLane(event.target.value as typeof lane)}>
              <option value="steering">Steer next</option>
              <option value="followUp">Follow up</option>
            </select>
            <input
              value={queuedText}
              placeholder={lane === 'steering' ? 'Interrupt at next turn boundary' : 'Run when idle'}
              onChange={(event) => setQueuedText(event.target.value)}
              onKeyDown={(event) => { if (event.key === 'Enter') addQueued() }}
            />
            <button type="button" onClick={addQueued}>Add</button>
          </div>
          <QueueLane label="Steering" lane="steering" items={queue.steering} editable={queue.mutationSupported === true} onMutate={mutate} />
          <QueueLane label="Follow-up" lane="followUp" items={queue.followUp} editable={queue.mutationSupported === true} onMutate={mutate} />
          {count > 0 && (
            <footer>
              <button type="button" onClick={() => void window.prime.agentHarness(agentId, 'queue_clear').then((value) => setQueue(value as ActionQueue))}>Clear queued</button>
              <button className="danger" type="button" onClick={() => void window.prime.agentHarness(agentId, 'queue_abort_clear').then((value) => setQueue(value as ActionQueue))}>Abort & clear</button>
            </footer>
          )}
        </section>
      )}

      {sideHost && createPortal(
        <section className="side-thread-chat">
          <div className="side-thread-intro">Independent from the main transcript</div>
          <div className="side-thread-turns sp-chat-feed">
            {sideMessages.map((message) => <MessageItem key={message.id} message={message} toolExecs={{}} showReasoning={showReasoning} />)}
            {sideRun?.status === 'running' && !sideRun.answer && (
              <div className="assistant-pending" role="status">
                <span className="assistant-pending-shimmer">Thinking…</span>
              </div>
            )}
            {sideRun && (sideRun.status === 'error' || sideRun.status === 'cancelled') && (
              <div className="sp-chat-error">{sideRun.errorMessage || sideRun.status}</div>
            )}
            {!sideRun && turns.length === 0 && <div className="sp-empty">Ask a quick question without changing the main conversation.</div>}
          </div>
          <Composer
            busy={sideRun?.status === 'running'}
            commands={commands}
            onSend={(text, images) => {
              if (images.length > 0) {
                onToast?.('Side questions do not support image attachments yet.', 'warning')
              }
              if (text.trim()) startSide(text)
            }}
            onSlash={onSlash ?? startSide}
            onAbort={() => {
              if (sideRun?.status === 'running') {
                void window.prime.agentHarness(agentId, 'side_question_abort', { id: sideRun.id })
              }
            }}
            onBash={onBash ?? ((command) => startSide(`!${command}`))}
            models={models}
            currentModel={currentModel}
            onSelectModel={onSelectModel}
            effortLevel={effortLevel}
            onSelectEffort={onSelectEffort}
            accessMode={accessMode}
            onAccessModeChange={onAccessModeChange}
            rlmMaxDepth={rlmMaxDepth}
            onDepthChange={onDepthChange}
            showContext={false}
          />
        </section>,
        sideHost
      )}
    </>
  )
}

function QueueLane({
  label,
  lane,
  items,
  editable,
  onMutate
}: {
  label: string
  lane: 'steering' | 'followUp'
  items: string[]
  editable: boolean
  onMutate: (lane: 'steering' | 'followUp', index: number, expectedText: string, mutation: Record<string, unknown>) => void
}): JSX.Element {
  return (
    <div className="queue-lane">
      <div className="queue-lane-title">{label}<span>{items.length}</span></div>
      {items.length === 0 && <div className="queue-empty">Empty</div>}
      {items.map((text, index) => (
        <div className="queue-item" key={`${index}-${text}`}>
          <input
            defaultValue={text}
            readOnly={!editable}
            aria-label={`${label} queued message ${index + 1}`}
            onBlur={(event) => {
              const next = event.target.value.trim()
              if (editable && next && next !== text) onMutate(lane, index, text, { type: 'replace', text: next, lane })
            }}
          />
          {editable && (
            <div className="queue-item-actions">
              <button type="button" disabled={index === 0} aria-label="Move up" onClick={() => onMutate(lane, index, text, { type: 'move', direction: -1 })}>↑</button>
              <button type="button" disabled={index === items.length - 1} aria-label="Move down" onClick={() => onMutate(lane, index, text, { type: 'move', direction: 1 })}>↓</button>
              <button type="button" aria-label={`Move to ${lane === 'steering' ? 'follow-up' : 'steering'}`} onClick={() => onMutate(lane, index, text, { type: 'replace', text, lane: lane === 'steering' ? 'followUp' : 'steering' })}>⇄</button>
              <button type="button" aria-label="Delete queued message" onClick={() => onMutate(lane, index, text, { type: 'delete' })}>×</button>
            </div>
          )}
        </div>
      ))}
    </div>
  )
}

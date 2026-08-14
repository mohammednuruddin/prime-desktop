import { useEffect, useState, type ReactNode } from 'react'
import type { AuthProvider, GoalState, SessionSummary } from '@shared/types'
import type { ModelOption } from '@shared/models'
import type { SlashOverlayId } from '@shared/slash'
import { HOTKEYS } from '@shared/slash'
import DepthSlider from './DepthSlider'

export interface ForkMessage {
  entryId: string
  text: string
}

interface Props {
  overlay: SlashOverlayId
  args: string
  agentId: string
  effortLevel: string
  depth: number
  models: ModelOption[]
  onClose: () => void
  onFork: (entryId: string) => void
  onTree: (entryId: string) => void
  onName: (name: string) => void
  onResume: (path: string) => void
  onHeartbeatSet: (schedule: string, prompt: string) => void
  onEffort: (level: string) => void
  onDepth: (value: number) => void
  onOpenModelPicker: () => void
  onScopedModels: (models: ModelOption[]) => void
}

const EFFORTS = ['off', 'minimal', 'low', 'medium', 'high', 'xhigh']

export default function SlashOverlay(props: Props): JSX.Element {
  return (
    <div className="slash-backdrop" onMouseDown={props.onClose}>
      <div className="slash-sheet" onMouseDown={(e) => e.stopPropagation()}>
        <OverlayBody {...props} />
      </div>
    </div>
  )
}

function OverlayBody(props: Props): JSX.Element {
  switch (props.overlay) {
    case 'fork':
      return <ForkPane agentId={props.agentId} onFork={props.onFork} onClose={props.onClose} title="Fork from a message" />
    case 'tree':
      return <TreePane agentId={props.agentId} onTree={props.onTree} onClose={props.onClose} />
    case 'login':
      return <LoginPane onClose={props.onClose} />
    case 'logout':
      return <LogoutPane onClose={props.onClose} />
    case 'name':
      return <NamePane initial={props.args} onName={props.onName} onClose={props.onClose} />
    case 'resume':
      return <ResumePane agentId={props.agentId} onResume={props.onResume} onClose={props.onClose} />
    case 'heartbeat':
      return <HeartbeatPane args={props.args} onSet={props.onHeartbeatSet} onClose={props.onClose} />
    case 'session':
      return <SessionPane agentId={props.agentId} onClose={props.onClose} />
    case 'usage':
      return <UsagePane agentId={props.agentId} onClose={props.onClose} />
    case 'hotkeys':
      return <HotkeysPane onClose={props.onClose} />
    case 'changelog':
      return <ChangelogPane onClose={props.onClose} />
    case 'system-prompt':
      return <SystemPromptPane agentId={props.agentId} onClose={props.onClose} />
    case 'model':
      return (
        <SimplePane title="Model" onClose={props.onClose}>
          <p className="slash-copy">Use the model pill in the composer to search and switch models.</p>
          <button className="btn primary" onClick={() => { props.onOpenModelPicker(); props.onClose() }}>Open model picker</button>
        </SimplePane>
      )
    case 'effort':
      return (
        <SimplePane title="Effort" onClose={props.onClose}>
          <div className="slash-list">
            {EFFORTS.map((level) => (
              <button
                key={level}
                className={`slash-row ${level === props.effortLevel.toLowerCase() ? 'selected' : ''}`}
                onClick={() => { props.onEffort(level); props.onClose() }}
              >
                {level}
              </button>
            ))}
          </div>
        </SimplePane>
      )
    case 'depth':
      return (
        <SimplePane title="Recursive depth" onClose={props.onClose}>
          <p className="slash-copy">How many nested subagents this chat may spawn. 0 keeps work in the parent only.</p>
          <DepthSlider value={props.depth} onChange={props.onDepth} />
        </SimplePane>
      )
    case 'scoped-models':
      return (
        <ScopedModelsPane
          models={props.models}
          onSave={props.onScopedModels}
          onClose={props.onClose}
        />
      )
    case 'goal':
      return <GoalPane agentId={props.agentId} args={props.args} onClose={props.onClose} />
  }
}

function SimplePane({ title, onClose, children }: { title: string; onClose: () => void; children: ReactNode }): JSX.Element {
  return (
    <>
      <header className="slash-head">
        <h3>{title}</h3>
        <button className="slash-x" onClick={onClose} aria-label="Close">×</button>
      </header>
      <div className="slash-body">{children}</div>
    </>
  )
}

function ForkPane({ agentId, onFork, onClose, title }: { agentId: string; onFork: (id: string) => void; onClose: () => void; title: string }): JSX.Element {
  const [items, setItems] = useState<ForkMessage[] | null>(null)
  const [error, setError] = useState<string | null>(null)
  useEffect(() => {
    void window.prime.agentCommand(agentId, { type: 'get_fork_messages' }).then((res) => {
      const data = res as { messages?: ForkMessage[] }
      setItems(data.messages ?? [])
    }).catch((err: Error) => setError(err.message))
  }, [agentId])
  return (
    <SimplePane title={title} onClose={onClose}>
      {error && <p className="slash-copy">{error}</p>}
      {items === null && !error && <p className="slash-copy">Loading messages…</p>}
      {items && items.length === 0 && <p className="slash-copy">No user messages to fork from yet.</p>}
      <div className="slash-list">
        {items?.map((item) => (
          <button key={item.entryId} className="slash-row" onClick={() => onFork(item.entryId)}>
            <span className="slash-row-text">{item.text.slice(0, 180) || '(empty)'}</span>
          </button>
        ))}
      </div>
    </SimplePane>
  )
}

interface TreeItem {
  entryId: string
  parentId: string | null
  depth: number
  label: string
  text: string
  role: string
}

function TreePane({ agentId, onTree, onClose }: { agentId: string; onTree: (id: string) => void; onClose: () => void }): JSX.Element {
  const [items, setItems] = useState<TreeItem[] | null>(null)
  const [leafId, setLeafId] = useState<string | null>(null)
  const [error, setError] = useState<string | null>(null)
  useEffect(() => {
    void window.prime.agentHarness(agentId, 'get_tree').then((result) => {
      const data = result as { nodes?: TreeItem[]; leafId?: string | null }
      setItems((data.nodes ?? []).filter((item) => item.role === 'user'))
      setLeafId(data.leafId ?? null)
    }).catch((err: Error) => setError(err.message))
  }, [agentId])
  return (
    <SimplePane title="Session tree" onClose={onClose}>
      <p className="slash-copy">Choose a prior turn to continue from that point in this session.</p>
      {error && <p className="slash-copy">{error}</p>}
      {items === null && !error && <p className="slash-copy">Loading branches…</p>}
      <div className="slash-list tree-list">
        {items?.map((item) => (
          <button
            key={item.entryId}
            className={`slash-row tree-row ${item.entryId === leafId ? 'selected' : ''}`}
            style={{ paddingLeft: `${12 + Math.min(item.depth, 8) * 12}px` }}
            onClick={() => onTree(item.entryId)}
          >
            <span className="tree-node-dot" />
            <span className="slash-row-text">{item.label || item.text.slice(0, 180) || '(empty turn)'}</span>
            {item.entryId === leafId && <span className="slash-meta">current</span>}
          </button>
        ))}
      </div>
    </SimplePane>
  )
}

function LoginPane({ onClose }: { onClose: () => void }): JSX.Element {
  const [providers, setProviders] = useState<AuthProvider[]>([])
  const [picked, setPicked] = useState<string | null>(null)
  const [key, setKey] = useState('')
  const [status, setStatus] = useState<string | null>(null)
  useEffect(() => {
    void window.prime.authList().then((list) => setProviders(list as AuthProvider[]))
  }, [])
  return (
    <SimplePane title="Login" onClose={onClose}>
      <p className="slash-copy">Store an API key in ~/.prime/agent/auth.json, shared with the CLI. Subscription OAuth still uses the Prime Agent terminal.</p>
      <div className="slash-list">
        {providers.map((p) => (
          <button key={p.id} className={`slash-row ${picked === p.id ? 'selected' : ''}`} onClick={() => setPicked(p.id)}>
            <span>{p.name}</span>
            <span className="slash-meta">{p.configured ? 'saved' : p.id}</span>
          </button>
        ))}
      </div>
      {picked && (
        <form
          className="slash-form"
          onSubmit={(e) => {
            e.preventDefault()
            if (!key.trim()) return
            void window.prime.authSet(picked, key.trim()).then(() => {
              setStatus(`Saved ${picked}`)
              setKey('')
              void window.prime.authList().then((list) => setProviders(list as AuthProvider[]))
            })
          }}
        >
          <input className="field" type="password" autoFocus placeholder="API key" value={key} onChange={(e) => setKey(e.target.value)} />
          <button className="btn primary" type="submit">Save key</button>
        </form>
      )}
      <button className="btn ghost" onClick={() => void window.prime.authOpenTui()}>Open Prime Agent for OAuth</button>
      {status && <p className="slash-copy">{status}</p>}
    </SimplePane>
  )
}

function LogoutPane({ onClose }: { onClose: () => void }): JSX.Element {
  const [providers, setProviders] = useState<AuthProvider[]>([])
  useEffect(() => {
    void window.prime.authList().then((list) => setProviders((list as AuthProvider[]).filter((p) => p.configured)))
  }, [])
  return (
    <SimplePane title="Logout" onClose={onClose}>
      {providers.length === 0 && <p className="slash-copy">No stored credentials to remove.</p>}
      <div className="slash-list">
        {providers.map((p) => (
          <button
            key={p.id}
            className="slash-row"
            onClick={() => {
              void window.prime.authRemove(p.id).then(() => {
                setProviders((cur) => cur.filter((x) => x.id !== p.id))
              })
            }}
          >
            Remove {p.name}
          </button>
        ))}
      </div>
    </SimplePane>
  )
}

function NamePane({ initial, onName, onClose }: { initial: string; onName: (name: string) => void; onClose: () => void }): JSX.Element {
  const [name, setName] = useState(initial)
  return (
    <SimplePane title="Session name" onClose={onClose}>
      <form
        className="slash-form"
        onSubmit={(e) => {
          e.preventDefault()
          if (!name.trim()) return
          onName(name.trim())
        }}
      >
        <input className="field" autoFocus placeholder="Name this chat" value={name} onChange={(e) => setName(e.target.value)} />
        <button className="btn primary" type="submit">Save</button>
      </form>
    </SimplePane>
  )
}

function ResumePane({ agentId, onResume, onClose }: { agentId: string; onResume: (path: string) => void; onClose: () => void }): JSX.Element {
  const [sessions, setSessions] = useState<SessionSummary[]>([])
  useEffect(() => {
    void window.prime.agentSessions(agentId).then(setSessions)
  }, [agentId])
  return (
    <SimplePane title="Resume session" onClose={onClose}>
      {sessions.length === 0 && <p className="slash-copy">No saved sessions for this project.</p>}
      <div className="slash-list">
        {sessions.map((s) => (
          <button key={s.sessionFile} className="slash-row" onClick={() => onResume(s.sessionFile)}>
            <span className="slash-row-text">{s.name ?? s.sessionId.slice(0, 8)}</span>
            <span className="slash-meta">{s.messageCount} msgs</span>
          </button>
        ))}
      </div>
    </SimplePane>
  )
}

function HeartbeatPane({ args, onSet, onClose }: { args: string; onSet: (schedule: string, prompt: string) => void; onClose: () => void }): JSX.Element {
  const [schedule, setSchedule] = useState('every 30m')
  const [prompt, setPrompt] = useState(args)
  return (
    <SimplePane title="Heartbeat" onClose={onClose}>
      <p className="slash-copy">A heartbeat steers the agent on a schedule. Example: every 30m</p>
      <form
        className="slash-form col"
        onSubmit={(e) => {
          e.preventDefault()
          if (!prompt.trim()) return
          onSet(schedule.trim() || 'every 30m', prompt.trim())
        }}
      >
        <input className="field" value={schedule} onChange={(e) => setSchedule(e.target.value)} placeholder="every 30m" />
        <textarea className="field" rows={3} value={prompt} onChange={(e) => setPrompt(e.target.value)} placeholder="Check whether the tests still pass" />
        <button className="btn primary" type="submit">Set heartbeat</button>
      </form>
    </SimplePane>
  )
}

function SessionPane({ agentId, onClose }: { agentId: string; onClose: () => void }): JSX.Element {
  const [text, setText] = useState('Loading…')
  useEffect(() => {
    void window.prime.agentCommand(agentId, { type: 'get_session_stats' }).then((res) => {
      const s = res as Record<string, unknown>
      const lines = [
        s.sessionName ? `Name: ${s.sessionName}` : null,
        `File: ${s.sessionFile ?? 'in-memory'}`,
        `ID: ${s.sessionId ?? '—'}`,
        `User: ${s.userMessages ?? '—'}`,
        `Assistant: ${s.assistantMessages ?? '—'}`,
        `Tool calls: ${s.toolCalls ?? '—'}`,
        `Total: ${s.totalMessages ?? '—'}`
      ].filter(Boolean)
      setText(lines.join('\n'))
    }).catch((err: Error) => setText(err.message))
  }, [agentId])
  return (
    <SimplePane title="Session" onClose={onClose}>
      <pre className="slash-pre">{text}</pre>
    </SimplePane>
  )
}

function UsagePane({ agentId, onClose }: { agentId: string; onClose: () => void }): JSX.Element {
  const [text, setText] = useState('Loading…')
  useEffect(() => {
    void window.prime.agentStats(agentId).then((res) => {
      const s = (res ?? {}) as Record<string, unknown>
      const tokens = (s.tokens as Record<string, number> | undefined) ?? {}
      const ctx = (s.contextUsage as Record<string, number | null> | undefined) ?? {}
      setText([
        `Cost: $${Number(s.cost ?? 0).toFixed(4)}`,
        `Input: ${tokens.input ?? 0}`,
        `Output: ${tokens.output ?? 0}`,
        `Cache read: ${tokens.cacheRead ?? 0}`,
        `Context: ${ctx.percent != null ? `${ctx.percent}%` : '—'} (${ctx.tokens ?? '—'} / ${ctx.contextWindow ?? '—'})`
      ].join('\n'))
    }).catch((err: Error) => setText(err.message))
  }, [agentId])
  return (
    <SimplePane title="Usage" onClose={onClose}>
      <pre className="slash-pre">{text}</pre>
    </SimplePane>
  )
}

function HotkeysPane({ onClose }: { onClose: () => void }): JSX.Element {
  return (
    <SimplePane title="Hotkeys" onClose={onClose}>
      <div className="slash-list">
        {HOTKEYS.map((row) => (
          <div key={row.keys} className="slash-row static">
            <kbd>{row.keys}</kbd>
            <span className="slash-meta">{row.action}</span>
          </div>
        ))}
      </div>
    </SimplePane>
  )
}

function ChangelogPane({ onClose }: { onClose: () => void }): JSX.Element {
  const [text, setText] = useState('Loading…')
  useEffect(() => {
    void window.prime.changelog().then(setText)
  }, [])
  return (
    <SimplePane title="Changelog" onClose={onClose}>
      <pre className="slash-pre">{text}</pre>
    </SimplePane>
  )
}

function SystemPromptPane({ agentId, onClose }: { agentId: string; onClose: () => void }): JSX.Element {
  const [text, setText] = useState('Loading…')
  useEffect(() => {
    void window.prime.agentHarness(agentId, 'system_prompt').then((result) => {
      setText(String((result as { text?: string }).text ?? ''))
    }).catch((err: Error) => setText(err.message))
  }, [agentId])
  return (
    <SimplePane title="System prompt" onClose={onClose}>
      <pre className="slash-pre">{text}</pre>
    </SimplePane>
  )
}

function GoalPane({ agentId, args, onClose }: { agentId: string; args: string; onClose: () => void }): JSX.Element {
  const parsed = parseGoalDraft(args)
  const [goal, setGoal] = useState<GoalState | null>(null)
  const [objective, setObjective] = useState(parsed.objective)
  const [budget, setBudget] = useState(parsed.budget)
  const [editing, setEditing] = useState(Boolean(parsed.objective))
  const [loaded, setLoaded] = useState(false)
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState('')

  const refresh = () => {
    void window.prime.agentHarness(agentId, 'goal_state').then((result) => {
      const next = (result as { goal?: GoalState | null }).goal ?? null
      setGoal(next)
      if (next?.objective && next.status !== 'idle' && !parsed.objective) {
        setObjective(next.objective)
        setBudget(next.tokenBudget ? String(next.tokenBudget) : '')
        setEditing(false)
      }
      setLoaded(true)
    }).catch((err: Error) => {
      setError(err.message)
      setLoaded(true)
    })
  }

  useEffect(() => {
    refresh()
    const off = window.prime.onEvent((raw) => {
      const event = raw as { agentId?: string; type?: string; payload?: Record<string, unknown> }
      if (event.agentId === agentId && event.type === 'goal_update') {
        setGoal((event.payload?.goal ?? event.payload) as GoalState | null)
      }
    })
    return off
  }, [agentId])

  const live = goal?.objective && goal.status !== 'idle' ? goal : null
  const composing = editing || !live

  const run = async (command: string) => {
    setBusy(true)
    setError('')
    try {
      await window.prime.agentHarness(agentId, 'goal_command', { command })
      window.setTimeout(refresh, 200)
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err))
    } finally {
      setBusy(false)
    }
  }

  const start = () => {
    const text = objective.trim()
    if (!text) return
    const tokens = Number(budget)
    const prefix = tokens > 0 ? `--budget ${tokens} ` : ''
    void run(`/goal ${prefix}${text}`).then(() => setEditing(false))
  }

  return (
    <div className="goal-sheet">
      <header className="slash-head">
        <div className="goal-kicker">
          <span className="goal-mark" aria-hidden="true" />
          <div>
            <h3>Thread goal</h3>
            <p>The daemon keeps this objective until you pause or clear it.</p>
          </div>
        </div>
        <button className="slash-x" onClick={onClose} aria-label="Close">×</button>
      </header>
      <div className="slash-body">
        {live && !composing && loaded && (
          <div className={`goal-brief ${live.status}`}>
            <div className="goal-brief-meta">
              <span className={`goal-status ${live.status}`}>{goalStatusLabel(live.status)}</span>
              <span>{formatGoalUsage(live)}</span>
            </div>
            <p>{live.objective}</p>
            <div className="goal-meter" aria-hidden="true">
              <i style={{ width: `${goalProgress(live)}%` }} />
            </div>
            {live.lastReason && <div className="slash-copy">{live.lastReason}</div>}
            {live.lastError && <div className="theme-import-error">{live.lastError}</div>}
            <div className="goal-actions">
              {live.status === 'active' && <button className="btn small" disabled={busy} onClick={() => void run('/goal pause')}>Pause</button>}
              {['paused', 'budget_limited'].includes(live.status) && <button className="btn small" disabled={busy} onClick={() => void run('/goal resume')}>Resume</button>}
              <button className="btn ghost small" disabled={busy} onClick={() => setEditing(true)}>Rewrite</button>
              <button className="btn ghost small" disabled={busy} onClick={() => void run('/goal clear').then(() => { setObjective(''); setBudget(''); setEditing(true) })}>Clear</button>
            </div>
          </div>
        )}

        {composing && (loaded || Boolean(parsed.objective)) && (
          <form
            className="goal-compose"
            onSubmit={(event) => {
              event.preventDefault()
              start()
            }}
          >
            <label className="goal-field">
              <span>Keep working until</span>
              <textarea
                className="field"
                autoFocus
                rows={4}
                value={objective}
                placeholder="Ship the inspector redesign, then stop."
                onChange={(event) => setObjective(event.target.value)}
              />
            </label>
            <div className="goal-budget">
              <span>Token budget</span>
              <div className="goal-budget-chips">
                {['', '20000', '50000', '100000'].map((value) => (
                  <button
                    key={value || 'none'}
                    type="button"
                    className={budget === value ? 'selected' : ''}
                    onClick={() => setBudget(value)}
                  >
                    {value ? `${Number(value) / 1000}k` : 'None'}
                  </button>
                ))}
                <input
                  className="field"
                  type="number"
                  min="1"
                  placeholder="Custom"
                  value={['', '20000', '50000', '100000'].includes(budget) ? '' : budget}
                  onChange={(event) => setBudget(event.target.value)}
                />
              </div>
            </div>
            <div className="goal-actions">
              <button className="btn primary" type="submit" disabled={busy || !objective.trim()}>
                {live ? 'Update goal' : 'Set goal'}
              </button>
              {live && <button className="btn ghost" type="button" onClick={() => setEditing(false)}>Cancel</button>}
            </div>
          </form>
        )}
        {error && <p className="theme-import-error">{error}</p>}
        {!loaded && <p className="slash-copy">Loading this thread’s goal…</p>}
      </div>
    </div>
  )
}

function parseGoalDraft(args: string): { objective: string; budget: string } {
  const match = /^(?:--(?:token-)?budget(?:=|\s+))(\d+)\s*([\s\S]*)/.exec(args.trim())
  if (match) return { budget: match[1], objective: match[2].trim() }
  return { objective: args.trim(), budget: '' }
}

function goalStatusLabel(status: GoalState['status']): string {
  if (status === 'budget_limited') return 'Budget paused'
  return status.replace('_', ' ')
}

function formatGoalUsage(goal: GoalState): string {
  const used = goal.tokensUsed.toLocaleString()
  return goal.tokenBudget ? `${used} / ${goal.tokenBudget.toLocaleString()} tokens` : `${used} tokens`
}

function goalProgress(goal: GoalState): number {
  if (!goal.tokenBudget || goal.tokenBudget <= 0) return goal.active ? 12 : 0
  return Math.max(4, Math.min(100, Math.round((goal.tokensUsed / goal.tokenBudget) * 100)))
}

function ScopedModelsPane({ models, onSave, onClose }: { models: ModelOption[]; onSave: (models: ModelOption[]) => void; onClose: () => void }): JSX.Element {
  const [selected, setSelected] = useState<Set<string>>(() => new Set(models.map((model) => model.key)))
  return (
    <SimplePane title="Scoped models" onClose={onClose}>
      <p className="slash-copy">Choose which models are included when cycling models.</p>
      <div className="slash-list">
        {models.map((model) => (
          <label key={model.key} className="slash-row scoped-model-row">
            <input
              type="checkbox"
              checked={selected.has(model.key)}
              onChange={(event) => {
                setSelected((current) => {
                  const next = new Set(current)
                  if (event.target.checked) next.add(model.key)
                  else next.delete(model.key)
                  return next
                })
              }}
            />
            <span className="slash-row-text">{model.name || model.id}</span>
            <span className="slash-meta">{model.provider}</span>
          </label>
        ))}
      </div>
      <button className="btn primary" onClick={() => onSave(models.filter((model) => selected.has(model.key)))}>
        Save scope
      </button>
    </SimplePane>
  )
}

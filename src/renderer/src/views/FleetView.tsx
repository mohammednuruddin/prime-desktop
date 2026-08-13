import { useEffect, useState } from 'react'
import type { AppState, FleetEntry } from '../lib/store'
import type { ScheduleJob } from '@shared/types'

interface Props {
  state: AppState
}

export default function FleetView({ state }: Props): JSX.Element {
  const [schedules, setSchedules] = useState<Record<string, ScheduleJob[]>>({})
  const [fleet, setFleet] = useState<FleetEntry[]>([])
  const [addFor, setAddFor] = useState<string | null>(null)
  const [cron, setCron] = useState('')
  const [prompt, setPrompt] = useState('')
  const [target, setTarget] = useState<string>('')
  const [msg, setMsg] = useState('')
  const [mode, setMode] = useState('auto')
  const [hearts, setHearts] = useState<Record<string, string>>({})

  useEffect(() => {
    void window.prime.fleetSchedules().then((res) => {
      setSchedules(res as Record<string, ScheduleJob[]>)
    })
    const off = window.prime.onEvent((raw) => {
      const e = raw as { agentId?: string; type?: string; payload?: Record<string, unknown> }
      if (e.type === 'fleet_event') {
        const entry: FleetEntry = {
          id: `${Date.now()}-${Math.random().toString(36).slice(2, 6)}`,
          at: Date.now(),
          agentId: e.agentId ?? '?',
          label: String((e.payload?.type as string) ?? 'event'),
          text: String(e.payload?.message ?? e.payload?.text ?? e.payload?.summary ?? e.payload?.status ?? e.payload?.type ?? ''),
          payload: e.payload
        }
        setFleet((prev) => [...prev.slice(-199), entry])
      }
    })
    return off
  }, [])

  const agents = Object.values(state.agents)

  return (
    <div className="view scheduled-page">
      <header className="view-header">
        <h2>Automations</h2>
        <p className="view-sub">Recurring work, heartbeats, and messages between running agents.</p>
      </header>

      <div className="fleet-grid">
        {agents.map((a) => (
          <div key={a.id} className={`fleet-card ${a.isStreaming || a.status === 'working' ? 'working' : ''}`}>
            <div className="fleet-card-head">
              <span className={`tab-dot ${a.isStreaming || a.status === 'working' ? 'working' : a.status === 'error' ? 'error' : 'idle'}`} />
              <span className="fleet-name">{a.name}</span>
              <span className="fleet-status">{a.isStreaming ? 'working' : a.status}</span>
            </div>
            <div className="fleet-meta">
              {a.model ?? 'no model'} · {a.thinkingLevel ?? '—'}
            </div>
            <div className="fleet-meta">
              session {a.sessionId ? a.sessionId.slice(0, 8) : '—'} · {a.messageCount} msgs
            </div>
            <div className="fleet-row">
              <span className="stat">${a.cost.toFixed(4)}</span>
              {a.contextPercent !== null && (
                <div className="ctx-bar small">
                  <div className={`ctx-fill ${a.contextPercent > 80 ? 'hot' : ''}`} style={{ width: `${Math.min(100, a.contextPercent)}%` }} />
                </div>
              )}
            </div>
            <div className="fleet-actions">
              {a.sessionId && (
                <button
                  className="btn ghost small"
                  onClick={() => void window.prime.fleetObserve(a.id, a.sessionId!)}
                >
                  Observe
                </button>
              )}
              <button className="btn ghost small" onClick={() => setAddFor(addFor === a.id ? null : a.id)}>
                Schedule
              </button>
              <button
                className="btn ghost small"
                onClick={async () => {
                  const h = await window.prime.fleetHeartbeat(a.id)
                  const text = h ? `Active: ${h.prompt.slice(0, 60)}` : 'No heartbeat'
                  setHearts((p) => ({ ...p, [a.id]: text }))
                }}
              >
                Heartbeat
              </button>
            </div>
            {hearts[a.id] && <div className="fleet-heartbeat">{hearts[a.id]}</div>}
            {addFor === a.id && (
              <div className="schedule-form">
                <input
                  className="field"
                  placeholder="cron: 0 9 * * 1-5 or: in 30m"
                  value={cron}
                  onChange={(e) => setCron(e.target.value)}
                />
                <input
                  className="field"
                  placeholder="prompt"
                  value={prompt}
                  onChange={(e) => setPrompt(e.target.value)}
                  onKeyDown={(e) => {
                    if (e.key === 'Enter' && cron.trim() && prompt.trim()) {
                      void window.prime.fleetScheduleAdd(a.id, cron.trim(), prompt.trim()).then(() => {
                        void window.prime.fleetSchedules().then(setSchedules)
                      })
                      setCron('')
                      setPrompt('')
                      setAddFor(null)
                    }
                  }}
                />
                <div className="row-gap">
                  <button
                    className="btn small"
                    onClick={() => {
                      void window.prime.fleetScheduleAdd(a.id, cron.trim(), prompt.trim()).then(() => {
                        void window.prime.fleetSchedules().then(setSchedules)
                      })
                      setCron('')
                      setPrompt('')
                      setAddFor(null)
                    }}
                  >
                    Add
                  </button>
                </div>
              </div>
            )}
          </div>
        ))}
        {agents.length === 0 && <div className="cmd-empty">No agents running. Open a project folder first.</div>}
      </div>

      <section className="panel">
        <div className="panel-head">Send message between agents</div>
        <div className="row-gap">
          <select className="field" value={target} onChange={(e) => setTarget(e.target.value)}>
            <option value="">target session…</option>
            {agents
              .filter((a) => a.sessionId)
              .map((a) => (
                <option key={a.id} value={a.sessionId!}>
                  {a.name} ({a.sessionId!.slice(0, 8)})
                </option>
              ))}
          </select>
          <select className="field" value={mode} onChange={(e) => setMode(e.target.value)}>
            <option value="auto">auto</option>
            <option value="steer">steer</option>
            <option value="follow_up">follow up</option>
          </select>
          <input
            className="field grow"
            placeholder="message"
            value={msg}
            onChange={(e) => setMsg(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === 'Enter' && target && msg.trim() && agents[0]) {
                void window.prime.fleetSend(agents[0].id, target, msg.trim(), mode)
                setMsg('')
              }
            }}
          />
          <button
            className="btn primary small"
            onClick={() => {
              if (target && msg.trim() && agents[0]) {
                void window.prime.fleetSend(agents[0].id, target, msg.trim(), mode)
                setMsg('')
              }
            }}
          >
            Send
          </button>
        </div>
      </section>

      <section className="panel">
        <div className="panel-head">Schedules</div>
        {Object.entries(schedules).map(([agentId, jobs]) => (
          <div key={agentId} className="schedule-group">
            <div className="schedule-group-name">{state.agents[agentId]?.name ?? agentId}</div>
            {jobs.length === 0 && <div className="cmd-empty">No scheduled jobs</div>}
            {jobs.map((j) => (
              <div key={j.id} className="schedule-row">
                <code>{j.cron}</code>
                <span className="schedule-prompt">{j.prompt}</span>
                {j.active && <span className="badge-ok">active</span>}
                <button
                  className="btn ghost small"
                  onClick={() => {
                    void window.prime.fleetScheduleCancel(agentId, j.id).then(() => {
                      void window.prime.fleetSchedules().then(setSchedules)
                    })
                  }}
                >
                  cancel
                </button>
              </div>
            ))}
          </div>
        ))}
      </section>

      <section className="panel">
        <div className="panel-head">Observed event stream</div>
        <div className="fleet-log">
          {fleet.length === 0 && <div className="cmd-empty">Observe an agent to see its live event stream here.</div>}
          {fleet.slice(-50).map((e) => (
            <div key={e.id} className="fleet-log-line">
              <span className="fleet-log-time">{new Date(e.at).toLocaleTimeString()}</span>
              <span className="fleet-log-agent">{state.agents[e.agentId]?.name ?? e.agentId}</span>
              <span className="fleet-log-label">{e.label}</span>
              <span className="fleet-log-text">{e.text}</span>
            </div>
          ))}
        </div>
      </section>
    </div>
  )
}

import { useEffect, useState } from 'react'
import type { AutonomousConfig } from '@shared/types'

export default function AutonomyView(): JSX.Element {
  const [config, setConfig] = useState<AutonomousConfig | null>(null)
  const [gateInput, setGateInput] = useState('')

  useEffect(() => {
    void window.prime.autonomyGet().then((r) => {
      const res = r as { config: AutonomousConfig; progress: unknown }
      setConfig(res.config)
    })
  }, [])

  if (!config) {
    return (
      <div className="view">
        <header className="view-header">
          <h2>Autonomous Ops</h2>
        </header>
      </div>
    )
  }

  const update = (patch: Partial<AutonomousConfig>) => {
    const next = { ...config, ...patch }
    setConfig(next)
    void window.prime.autonomySet(patch)
  }

  return (
    <div className="view autonomy-page">
      <header className="view-header">
        <h2>Autonomous Ops</h2>
        <p className="view-sub">
          Bounded autonomous continuation policy. The app injects follow-ups until quality gates pass or budgets run
          out. Start a session with <code>/autonomous on</code> to use it.
        </p>
      </header>

      <section className="panel">
        <div className="panel-head">Gates</div>
        <div className="row-gap">
          <input
            className="field grow"
            placeholder="shell command that must pass, e.g. npm run check"
            value={gateInput}
            onChange={(e) => setGateInput(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === 'Enter' && gateInput.trim()) {
                update({ gates: [...config.gates, gateInput.trim()] })
                setGateInput('')
              }
            }}
          />
          <button
            className="btn primary small"
            onClick={() => {
              if (gateInput.trim()) {
                update({ gates: [...config.gates, gateInput.trim()] })
                setGateInput('')
              }
            }}
          >
            Add gate
          </button>
        </div>
        {config.gates.length === 0 && <div className="cmd-empty">No gates configured — the run ends when a budget is reached.</div>}
        {config.gates.map((g, i) => (
          <div key={i} className="perm-row">
            <code>{g}</code>
            <button className="btn ghost small" onClick={() => update({ gates: config.gates.filter((_, j) => j !== i) })}>
              remove
            </button>
          </div>
        ))}
        <div className="row-gap">
          <label className="field-label">
            gate retries
            <input
              type="number"
              className="field narrow"
              value={config.gateRetries}
              onChange={(e) => update({ gateRetries: Math.max(1, Number(e.target.value)) })}
            />
          </label>
        </div>
      </section>

      <section className="panel">
        <div className="panel-head">Budgets</div>
        <div className="budget-grid">
          <label className="field-label">
            max continuations
            <input
              type="number"
              className="field"
              value={config.maxContinuations}
              onChange={(e) => update({ maxContinuations: Math.max(1, Number(e.target.value)) })}
            />
          </label>
          <label className="field-label">
            max turns
            <input
              type="number"
              className="field"
              value={config.maxTurns}
              onChange={(e) => update({ maxTurns: Math.max(1, Number(e.target.value)) })}
            />
          </label>
          <label className="field-label">
            max tokens
            <input
              type="number"
              className="field"
              value={config.maxTokens}
              onChange={(e) => update({ maxTokens: Math.max(1000, Number(e.target.value)) })}
            />
          </label>
          <label className="field-label">
            max minutes
            <input
              type="number"
              className="field"
              value={Math.round(config.maxSeconds / 60)}
              onChange={(e) => update({ maxSeconds: Math.max(1, Number(e.target.value)) * 60 })}
            />
          </label>
        </div>
        <div className="row-gap pad-top">
          <button className="btn primary" onClick={() => update({ enabled: !config.enabled })}>
            {config.enabled ? 'Autonomy enabled — disable' : 'Enable autonomy'}
          </button>
          <span className="hint-text">
            Enable via UI stores the policy; sessions use it on their next autonomous run.
          </span>
        </div>
      </section>
    </div>
  )
}

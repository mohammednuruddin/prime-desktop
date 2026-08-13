import { useEffect, useState } from 'react'
import type { SpendPoint } from '@shared/types'

export default function DashboardView(): JSX.Element {
  const [spend, setSpend] = useState<{ points: SpendPoint[]; totals: { cost: number; tokensIn: number; tokensOut: number } } | null>(null)
  const [models, setModels] = useState<string[]>([])

  useEffect(() => {
    void window.prime.dashboardSpend().then(setSpend)
    void window.prime.dashboardModels().then(setModels)
  }, [])

  const max = spend ? Math.max(...spend.points.map((p) => p.tokensIn + p.tokensOut), 1) : 1

  return (
    <div className="view dashboard-page">
      <header className="view-header">
        <h2>Usage & Spend</h2>
        <p className="view-sub">Token and cost totals across live agents and recent sessions.</p>
      </header>

      <div className="stat-cards">
        <div className="stat-card">
          <div className="stat-card-value">${spend ? spend.totals.cost.toFixed(4) : '—'}</div>
          <div className="stat-card-label">estimated cost</div>
        </div>
        <div className="stat-card">
          <div className="stat-card-value">{spend ? formatTokens(spend.totals.tokensIn) : '—'}</div>
          <div className="stat-card-label">input tokens</div>
        </div>
        <div className="stat-card">
          <div className="stat-card-value">{spend ? formatTokens(spend.totals.tokensOut) : '—'}</div>
          <div className="stat-card-label">output tokens</div>
        </div>
        <div className="stat-card">
          <div className="stat-card-value">{models.length}</div>
          <div className="stat-card-label">models in use</div>
        </div>
      </div>

      <section className="panel">
        <div className="panel-head">Tokens per day</div>
        {spend && spend.points.length === 0 && <div className="cmd-empty">No usage recorded yet.</div>}
        {spend && spend.points.length > 0 && (
          <div className="chart">
            {spend.points.map((p) => {
              const v = p.tokensIn + p.tokensOut
              const h = Math.max(4, Math.round((v / max) * 120))
              return (
                <div key={p.date} className="chart-col">
                  <div className="chart-bar-wrap">
                    <div className="chart-bar" style={{ height: h }} title={`${p.date}: ${formatTokens(v)}`} />
                  </div>
                  <div className="chart-label">{p.date.slice(5)}</div>
                </div>
              )
            })}
          </div>
        )}
      </section>

      {models.length > 0 && (
        <section className="panel">
          <div className="panel-head">Models</div>
          <div className="tag-row">
            {models.map((m) => (
              <span key={m} className="tag">
                {m}
              </span>
            ))}
          </div>
        </section>
      )}
    </div>
  )
}

function formatTokens(n: number): string {
  if (n >= 1000000) return `${(n / 1000000).toFixed(2)}M`
  if (n >= 1000) return `${(n / 1000).toFixed(1)}k`
  return String(n)
}

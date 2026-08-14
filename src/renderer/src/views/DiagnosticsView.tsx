import { useEffect, useState } from 'react'
import type { TraceInfo } from '@shared/types'

export default function DiagnosticsView({ activeAgentId }: { activeAgentId: string | null }): JSX.Element {
  const [output, setOutput] = useState('')
  const [running, setRunning] = useState('')
  const [tracesEnabled, setTracesEnabled] = useState(false)
  const [traces, setTraces] = useState<TraceInfo[]>([])
  const [selectedTrace, setSelectedTrace] = useState<TraceInfo | null>(null)
  const [tracePreview, setTracePreview] = useState('')

  const agentId = activeAgentId ?? ''

  const run = async (action: string, input: Record<string, unknown> = {}) => {
    setRunning(action)
    try {
      const result = await window.prime.agentHarness(agentId, action, input) as { output?: string; data?: unknown; ok?: boolean }
      setOutput(result.output || JSON.stringify(result.data ?? result, null, 2))
    } catch (error) {
      setOutput(error instanceof Error ? error.message : String(error))
    } finally {
      setRunning('')
    }
  }

  const loadTraces = () => {
    void window.prime.agentHarness(agentId, 'trace_list').then((result) => {
      setTraces((result as { files?: TraceInfo[] }).files ?? [])
    })
  }

  useEffect(() => {
    void window.prime.agentHarness(agentId, 'traces', { mode: 'status' })
      .then((result) => setTracesEnabled(Boolean((result as { enabled?: boolean }).enabled)))
    loadTraces()
  }, [agentId])

  return (
    <div className="view diagnostics-page">
      <header className="view-header">
        <h2>Daemon & traces</h2>
        <p className="view-sub">Direct controls for Prime Agent’s resident daemon and local trace sessions.</p>
      </header>

      <section className="panel">
        <div className="panel-head">Daemon operations</div>
        <div className="diagnostic-actions">
          <button className="btn" disabled={Boolean(running)} onClick={() => void run('daemon_status')}>Status</button>
          <button className="btn" disabled={Boolean(running)} onClick={() => void run('daemon_doctor')}>Doctor</button>
          <button
            className="btn"
            disabled={Boolean(running)}
            onClick={() => { if (window.confirm('Clean stale sockets and idle orphaned services?')) void run('daemon_recover') }}
          >
            Recover
          </button>
          <button className="btn" disabled={Boolean(running)} onClick={() => void run('update')}>Update Prime Agent</button>
          <button
            className="btn danger"
            disabled={Boolean(running)}
            onClick={() => { if (window.confirm('Stop every Prime Agent and the resident daemon?')) void run('daemon_shutdown') }}
          >
            Shutdown all
          </button>
        </div>
        {running && <div className="hint-text pad-top">Running {running.replaceAll('_', ' ')}…</div>}
        {output && <pre className="diagnostic-output">{output}</pre>}
      </section>

      <section className="panel">
        <div className="panel-head">Trace sharing</div>
        <label className="setting-row">
          <div>
            <div className="setting-title">Automatic Prime Agent traces</div>
            <div className="setting-desc">Daemon-owned opt-in. Credentials and upload behavior remain in Prime Agent.</div>
          </div>
          <input
            className="setting-toggle"
            type="checkbox"
            checked={tracesEnabled}
            onChange={(event) => {
              const enabled = event.target.checked
              setTracesEnabled(enabled)
              void window.prime.agentHarness(agentId, 'traces', { mode: enabled ? 'on' : 'off' })
            }}
          />
        </label>
      </section>

      <section className="panel trace-explorer">
        <div className="panel-head">Local trace explorer <button className="btn ghost small" onClick={loadTraces}>Refresh</button></div>
        <div className="trace-grid">
          <div className="trace-list">
            {traces.map((trace) => (
              <button
                className={selectedTrace?.path === trace.path ? 'active' : ''}
                key={trace.path}
                onClick={() => {
                  setSelectedTrace(trace)
                  setTracePreview('Loading…')
                  void window.prime.agentHarness(agentId, 'trace_preview', { path: trace.path }).then((result) => {
                    const value = result as { preview?: string; truncated?: boolean }
                    setTracePreview(`${value.preview ?? ''}${value.truncated ? '\n\n…preview truncated' : ''}`)
                  })
                }}
              >
                <strong>{trace.name}</strong>
                <span>{new Date(trace.modifiedAt).toLocaleString()} · {formatBytes(trace.size)}</span>
              </button>
            ))}
            {traces.length === 0 && <div className="empty-state">No local session traces.</div>}
          </div>
          <pre className="trace-preview">{tracePreview || 'Select a trace to inspect its JSONL.'}</pre>
        </div>
      </section>
    </div>
  )
}

function formatBytes(value: number): string {
  if (value < 1024) return `${value} B`
  if (value < 1024 * 1024) return `${(value / 1024).toFixed(1)} KB`
  return `${(value / 1024 / 1024).toFixed(1)} MB`
}

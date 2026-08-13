import { useEffect, useRef, useState } from 'react'
import { Terminal } from '@xterm/xterm'
import { FitAddon } from '@xterm/addon-fit'
import '@xterm/xterm/css/xterm.css'

interface Snapshot {
  running: boolean
  pid: number
  buffer: string
  cwd: string
  offset: number
}

interface DataEvent {
  agentId: string
  data: string
  startOffset: number
  endOffset: number
}

export default function TerminalPanel({ agentId }: { agentId: string | null }): JSX.Element {
  const hostRef = useRef<HTMLDivElement>(null)
  const terminalRef = useRef<Terminal | null>(null)
  const fitRef = useRef<FitAddon | null>(null)
  const consumedOffset = useRef(0)
  const ready = useRef(false)
  const queued = useRef<DataEvent[]>([])
  const [status, setStatus] = useState<'starting' | 'ready' | 'exited' | 'error'>('starting')
  const [error, setError] = useState('')

  useEffect(() => {
    if (!agentId || !hostRef.current) return
    const styles = getComputedStyle(document.documentElement)
    const terminal = new Terminal({
      allowProposedApi: false,
      convertEol: true,
      cursorBlink: true,
      cursorStyle: 'bar',
      fontFamily: "'SFMono-Regular', 'SF Mono', Menlo, Monaco, Consolas, monospace",
      fontSize: 12,
      lineHeight: 1.35,
      scrollback: 10_000,
      theme: {
        background: styles.getPropertyValue('--bg').trim() || '#fff',
        foreground: styles.getPropertyValue('--ink').trim() || '#2e2b3c',
        cursor: styles.getPropertyValue('--text-muted').trim() || '#777386',
        selectionBackground: 'rgba(112, 98, 170, 0.2)',
        black: '#2e2b3c',
        red: '#c85b63',
        green: '#598b62',
        yellow: '#a87b35',
        blue: '#527cb8',
        magenta: '#8c68ad',
        cyan: '#478d91',
        white: '#e8e6ec',
        brightBlack: '#777386',
        brightRed: '#dc7279',
        brightGreen: '#72a47b',
        brightYellow: '#c6984e',
        brightBlue: '#6c95d1',
        brightMagenta: '#a681c4',
        brightCyan: '#62a8ab',
        brightWhite: '#ffffff'
      }
    })
    const fit = new FitAddon()
    terminal.loadAddon(fit)
    terminal.open(hostRef.current)
    terminalRef.current = terminal
    fitRef.current = fit
    const refreshTheme = () => {
      const current = getComputedStyle(document.documentElement)
      terminal.options.theme = {
        ...terminal.options.theme,
        background: current.getPropertyValue('--bg').trim(),
        foreground: current.getPropertyValue('--ink').trim(),
        cursor: current.getPropertyValue('--text-muted').trim(),
        green: current.getPropertyValue('--diff-added').trim(),
        red: current.getPropertyValue('--diff-removed').trim(),
        magenta: current.getPropertyValue('--skill').trim()
      }
    }
    window.addEventListener('prime-theme-change', refreshTheme)

    const applyData = (event: DataEvent) => {
      if (event.endOffset <= consumedOffset.current) return
      const overlap = Math.max(0, consumedOffset.current - event.startOffset)
      terminal.write(event.data.slice(overlap))
      consumedOffset.current = event.endOffset
    }
    const offData = window.prime.onTerminalData((event) => {
      if (event.agentId !== agentId) return
      if (!ready.current) queued.current.push(event)
      else applyData(event)
    })
    const offExit = window.prime.onTerminalExit((event) => {
      if (event.agentId === agentId) setStatus('exited')
    })
    const input = terminal.onData((data) => {
      void window.prime.terminalWrite(agentId, data)
    })
    const resize = new ResizeObserver(() => {
      try {
        fit.fit()
        void window.prime.terminalResize(agentId, terminal.cols, terminal.rows)
      } catch {
        // The panel can briefly have zero dimensions while switching tabs.
      }
    })
    resize.observe(hostRef.current)

    requestAnimationFrame(() => {
      fit.fit()
      void window.prime.terminalStart(agentId, terminal.cols, terminal.rows)
        .then((snapshot: Snapshot) => {
          terminal.reset()
          if (snapshot.buffer) terminal.write(snapshot.buffer)
          consumedOffset.current = snapshot.offset
          ready.current = true
          for (const event of queued.current) applyData(event)
          queued.current = []
          setStatus('ready')
          terminal.focus()
        })
        .catch((reason: unknown) => {
          setError(reason instanceof Error ? reason.message : String(reason))
          setStatus('error')
        })
    })

    return () => {
      ready.current = false
      queued.current = []
      resize.disconnect()
      input.dispose()
      offData()
      offExit()
      window.removeEventListener('prime-theme-change', refreshTheme)
      terminal.dispose()
      terminalRef.current = null
      fitRef.current = null
    }
  }, [agentId])

  const clear = () => {
    if (!agentId) return
    terminalRef.current?.clear()
    void window.prime.terminalClear(agentId)
  }

  const restart = () => {
    if (!agentId || !terminalRef.current) return
    const terminal = terminalRef.current
    ready.current = false
    queued.current = []
    consumedOffset.current = 0
    terminal.reset()
    setStatus('starting')
    void window.prime.terminalRestart(agentId, terminal.cols, terminal.rows)
      .then((snapshot: Snapshot) => {
        if (snapshot.buffer) terminal.write(snapshot.buffer)
        consumedOffset.current = snapshot.offset
        ready.current = true
        for (const event of queued.current) {
          if (event.endOffset <= consumedOffset.current) continue
          const overlap = Math.max(0, consumedOffset.current - event.startOffset)
          terminal.write(event.data.slice(overlap))
          consumedOffset.current = event.endOffset
        }
        queued.current = []
        setStatus('ready')
        terminal.focus()
      })
      .catch((reason: unknown) => {
        setError(reason instanceof Error ? reason.message : String(reason))
        setStatus('error')
      })
  }

  if (!agentId) return <div className="sp-empty"><span>No active project</span></div>

  return (
    <div className="terminal-panel">
      <div className="terminal-toolbar">
        <span className={`terminal-status ${status}`}>{status === 'ready' ? 'Shell' : status}</span>
        <div className="terminal-toolbar-actions">
          <button onClick={clear}>Clear</button>
          <button onClick={restart}>Restart</button>
        </div>
      </div>
      <div className="terminal-host" ref={hostRef} />
      {status === 'error' && <div className="terminal-error">{error || 'Terminal could not start'}</div>}
    </div>
  )
}

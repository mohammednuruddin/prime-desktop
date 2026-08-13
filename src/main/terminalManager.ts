import { EventEmitter } from 'events'
import { spawn, type IPty } from 'node-pty'

const MAX_BUFFER = 200_000

interface TerminalSession {
  process: IPty
  cwd: string
  buffer: string
  offset: number
}

export interface TerminalSnapshot {
  running: boolean
  pid: number
  buffer: string
  cwd: string
  offset: number
}

export class TerminalManager extends EventEmitter {
  private sessions = new Map<string, TerminalSession>()

  start(agentId: string, cwd: string, cols = 100, rows = 30): TerminalSnapshot {
    const existing = this.sessions.get(agentId)
    if (existing) return this.snapshot(existing)

    const shell = process.env.SHELL || '/bin/zsh'
    const pty = spawn(shell, ['-l'], {
      name: 'xterm-256color',
      cols: clamp(cols, 20, 500),
      rows: clamp(rows, 5, 200),
      cwd,
      env: {
        ...process.env,
        TERM: 'xterm-256color',
        COLORTERM: 'truecolor'
      } as Record<string, string>
    })
    const session: TerminalSession = { process: pty, cwd, buffer: '', offset: 0 }
    this.sessions.set(agentId, session)

    pty.onData((data) => {
      if (this.sessions.get(agentId) !== session) return
      const startOffset = session.offset
      session.offset += data.length
      session.buffer = (session.buffer + data).slice(-MAX_BUFFER)
      this.emit('data', { agentId, data, startOffset, endOffset: session.offset })
    })
    pty.onExit(({ exitCode }) => {
      if (this.sessions.get(agentId) !== session) return
      this.sessions.delete(agentId)
      this.emit('exit', { agentId, exitCode })
    })
    return this.snapshot(session)
  }

  write(agentId: string, data: string): void {
    this.sessions.get(agentId)?.process.write(data)
  }

  resize(agentId: string, cols: number, rows: number): void {
    const session = this.sessions.get(agentId)
    if (!session) return
    session.process.resize(clamp(cols, 20, 500), clamp(rows, 5, 200))
  }

  restart(agentId: string, cwd: string, cols = 100, rows = 30): TerminalSnapshot {
    this.close(agentId)
    return this.start(agentId, cwd, cols, rows)
  }

  clear(agentId: string): void {
    const session = this.sessions.get(agentId)
    if (session) session.buffer = ''
  }

  close(agentId: string): void {
    const session = this.sessions.get(agentId)
    if (!session) return
    this.sessions.delete(agentId)
    session.process.kill()
  }

  shutdownAll(): void {
    for (const agentId of [...this.sessions.keys()]) this.close(agentId)
  }

  private snapshot(session: TerminalSession): TerminalSnapshot {
    return {
      running: true,
      pid: session.process.pid,
      buffer: session.buffer,
      cwd: session.cwd,
      offset: session.offset
    }
  }
}

function clamp(value: number, min: number, max: number): number {
  return Math.max(min, Math.min(max, Math.floor(value)))
}

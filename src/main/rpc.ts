import { spawn, type ChildProcess } from 'child_process'
import { existsSync, readFileSync, realpathSync } from 'fs'
import { homedir } from 'os'
import { join } from 'path'
import { StringDecoder } from 'string_decoder'
import { EventEmitter } from 'events'

export interface RpcClientOptions {
  binary: string
  cwd: string
  args?: string[]
  env?: Record<string, string>
}

export interface RpcEvent {
  type: string
  [key: string]: unknown
}

export class RpcError extends Error {
  command: string
  constructor(command: string, message: string) {
    super(message)
    this.command = command
  }
}

export class RpcClient extends EventEmitter {
  private proc: ChildProcess | null = null
  private decoder = new StringDecoder('utf8')
  private buffer = ''
  private pending = new Map<string, { resolve: (v: unknown) => void; reject: (e: Error) => void; cmd: string }>()
  private idCounter = 0
  private readonly binary: string
  private readonly cwd: string
  private readonly args: string[]
  private readonly env: Record<string, string> | undefined
  private exited = false
  private dieTimer: ReturnType<typeof setTimeout> | null = null
  private exitHandlers = new Set<() => void>()
  private stderrBuf = ''

  constructor(opts: RpcClientOptions) {
    super()
    this.binary = opts.binary
    this.cwd = opts.cwd
    this.args = opts.args ?? []
    this.env = opts.env
  }

  get running(): boolean {
    return this.proc !== null && !this.exited
  }

  onExit(fn: () => void): void {
    this.exitHandlers.add(fn)
  }

  async start(): Promise<void> {
    if (this.proc) return
    this.exited = false
    this.buffer = ''
    this.stderrBuf = ''
    const launch = launchSpec(this.binary)
    const child = spawn(launch.cmd, [...launch.args, ...this.args], {
      cwd: this.cwd,
      env: childEnv(this.env),
      stdio: ['pipe', 'pipe', 'pipe']
    })
    this.proc = child

    child.stdout!.on('data', (chunk: Buffer) => {
      this.handleChunk(chunk)
    })
    child.stderr!.on('data', (chunk: Buffer) => {
      const text = chunk.toString()
      this.stderrBuf = (this.stderrBuf + text).slice(-4000)
      this.emit('stderr', text)
    })
    child.on('error', (err) => {
      this.exited = true
      this.rejectAll(new RpcError('process', `Failed to spawn prime-agent: ${err.message}`))
      this.emit('process_error', err.message)
      this.fireExits()
    })
    child.on('exit', (code, signal) => {
      this.exited = true
      this.proc = null
      const detail = this.stderrBuf.trim().slice(-400)
      this.rejectAll(new RpcError('process', `prime-agent exited (code=${code}, signal=${signal})${detail ? `: ${detail}` : ''}`))
      this.emit('exit', { code, signal })
      this.fireExits()
    })

    await new Promise((resolve) => setTimeout(resolve, 150))
    if (this.exited || child.exitCode !== null) {
      throw new RpcError('process', `prime-agent exited immediately (code=${child.exitCode}). ${this.stderrBuf.trim().slice(-400)}`)
    }
  }

  private fireExits(): void {
    for (const fn of this.exitHandlers) fn()
  }

  private handleChunk(chunk: Buffer): void {
    this.buffer += this.decoder.write(chunk)
    while (true) {
      const nl = this.buffer.indexOf('\n')
      if (nl === -1) break
      let line = this.buffer.slice(0, nl)
      this.buffer = this.buffer.slice(nl + 1)
      if (line.endsWith('\r')) line = line.slice(0, -1)
      if (!line.trim()) continue
      this.dispatchLine(line)
    }
  }

  private dispatchLine(line: string): void {
    let obj: Record<string, unknown>
    try {
      obj = JSON.parse(line)
    } catch {
      this.emit('parse_error', line.slice(0, 200))
      return
    }
    const type = obj.type
    if (type === 'response') {
      const id = obj.id as string | undefined
      const cmd = obj.command as string
      if (id && this.pending.has(id)) {
        const p = this.pending.get(id)!
        this.pending.delete(id)
        if (obj.success) p.resolve(obj.data ?? obj)
        else p.reject(new RpcError(cmd, (obj.error as string) ?? 'Unknown RPC error'))
      }
      return
    }
    this.emit('event', obj as RpcEvent)
  }

  send<T = unknown>(cmd: Record<string, unknown>, timeoutMs = 120000): Promise<T> {
    return new Promise<T>((resolve, reject) => {
      if (!this.proc || this.exited) {
        reject(new RpcError(String(cmd.type), 'Agent is not running'))
        return
      }
      const id = `req-${++this.idCounter}`
      const payload = { ...cmd, id }
      const timer = setTimeout(() => {
        this.pending.delete(id)
        reject(new RpcError(String(cmd.type), `RPC command timed out after ${timeoutMs}ms`))
      }, timeoutMs)
      this.pending.set(id, {
        resolve: (v) => {
          clearTimeout(timer)
          resolve(v as T)
        },
        reject: (e) => {
          clearTimeout(timer)
          reject(e)
        },
        cmd: String(cmd.type)
      })
      this.proc.stdin!.write(JSON.stringify(payload) + '\n')
    })
  }

  fire(cmd: Record<string, unknown>): void {
    if (!this.proc || this.exited) return
    this.proc.stdin!.write(JSON.stringify(cmd) + '\n')
  }

  private rejectAll(err: Error): void {
    for (const [, p] of this.pending) p.reject(err)
    this.pending.clear()
  }

  stop(): void {
    if (this.dieTimer) clearTimeout(this.dieTimer)
    if (!this.proc || this.exited) return
    try {
      this.proc.stdin!.end()
    } catch {
      /* ignore */
    }
    this.dieTimer = setTimeout(() => {
      try {
        this.proc!.kill('SIGKILL')
      } catch {
        /* ignore */
      }
    }, 3000)
  }

  kill(): void {
    if (this.dieTimer) clearTimeout(this.dieTimer)
    this.rejectAll(new RpcError('process', 'Agent killed'))
    if (this.proc && !this.exited) {
      try {
        this.proc.kill('SIGKILL')
      } catch {
        /* ignore */
      }
    }
    this.proc = null
    this.exited = true
  }
}

function extraPath(): string {
  return ['/opt/homebrew/bin', '/usr/local/bin', join(homedir(), '.local/bin'), join(homedir(), '.hermes/node/bin')].join(':')
}

function childEnv(overrides?: Record<string, string>): NodeJS.ProcessEnv {
  const env: NodeJS.ProcessEnv = { ...process.env, ...overrides, PI_SKIP_VERSION_CHECK: '1' }
  delete env.ELECTRON_RUN_AS_NODE
  delete env.ELECTRON_NO_ASAR
  env.PATH = `${extraPath()}:${env.PATH ?? '/usr/bin:/bin'}`
  return env
}

function findNode(): string {
  for (const candidate of [
    '/opt/homebrew/bin/node',
    '/usr/local/bin/node',
    join(homedir(), '.local/bin/node'),
    join(homedir(), '.hermes/node/bin/node')
  ]) {
    if (existsSync(candidate)) return candidate
  }
  return 'node'
}

function launchSpec(binary: string): { cmd: string; args: string[] } {
  let target = binary
  try {
    target = realpathSync(binary)
  } catch {
    /* keep original path */
  }
  let shebang = ''
  try {
    shebang = readFileSync(target, 'utf8').slice(0, 120)
  } catch {
    /* native binary */
  }
  if (shebang.startsWith('#!') && /node/.test(shebang)) {
    return { cmd: findNode(), args: [target, '--mode', 'rpc'] }
  }
  return { cmd: target, args: ['--mode', 'rpc'] }
}

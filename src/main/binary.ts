import { execFile } from 'child_process'
import { mkdir, writeFile, chmod, unlink } from 'fs/promises'
import { existsSync, readFileSync } from 'fs'
import { homedir } from 'os'
import { join } from 'path'
import { promisify } from 'util'
import https from 'https'
import { EventEmitter } from 'events'
import { createHash } from 'crypto'

const execFileAsync = promisify(execFile)

const INSTALL_URL = 'https://app.primeintellect.ai/prime-agent/install.sh'
const BIN_DIR = join(homedir(), 'Library', 'Application Support', 'PrimeDesktop', 'bin')
const MAX_INSTALLER_BYTES = 2 * 1024 * 1024

export class BinaryManager extends EventEmitter {
  private state: {
    status: 'checking' | 'found' | 'installing' | 'installed' | 'error'
    path: string | null
    version: string | null
    error: string | null
    progress?: number
  } = { status: 'checking', path: null, version: null, error: null }

  get stateSnapshot() {
    return { ...this.state }
  }

  async check(): Promise<void> {
    this.state.status = 'checking'
    this.emit('change', this.stateSnapshot)
    try {
      const local = await this.findLocalInstall()
      if (local) {
        this.state.path = local.path
        this.state.version = local.version
        this.state.status = 'found'
      } else {
        this.state.status = 'error'
        this.state.error = 'prime-agent not found on PATH and not installed locally.'
      }
    } catch (err) {
      this.state.status = 'error'
      this.state.error = err instanceof Error ? err.message : String(err)
    }
    this.emit('change', this.stateSnapshot)
  }

  private async findLocalInstall(): Promise<{ path: string; version: string | null } | null> {
    const candidates = [
      this.state.path,
      process.env.PRIME_AGENT_PATH,
      join(BIN_DIR, 'prime-agent'),
      join(homedir(), '.local', 'bin', 'prime-agent'),
      '/opt/homebrew/bin/prime-agent',
      '/usr/local/bin/prime-agent'
    ].filter(Boolean) as string[]

    for (const c of candidates) {
      if (existsSync(c)) {
        const version = await this.getVersion(c)
        if (version) return { path: c, version }
      }
    }

    try {
      const { stdout } = await execFileAsync('which', ['prime-agent'])
      const p = stdout.trim()
      if (p) {
        const version = await this.getVersion(p)
        return { path: p, version }
      }
    } catch {
      /* not on PATH */
    }
    return null
  }

  private async getVersion(bin: string): Promise<string | null> {
    try {
      const { stdout, stderr } = await execFileAsync(bin, ['--version'], { timeout: 10000 })
      const text = (stdout + stderr).trim().split('\n')[0]
      return text || null
    } catch {
      return null
    }
  }

  async install(): Promise<void> {
    this.state.status = 'installing'
    this.state.progress = 0
    this.emit('change', this.stateSnapshot)
    try {
      await mkdir(BIN_DIR, { recursive: true })
      const script = await this.download(INSTALL_URL, (pct) => {
        this.state.progress = pct * 0.5
        this.emit('change', this.stateSnapshot)
      })
      if (!script.startsWith('#!') || !/\b(sh|bash)\b/.test(script.slice(0, 120))) {
        throw new Error('Installer response is not a shell script')
      }
      const expectedHash = process.env.PRIME_AGENT_INSTALL_SHA256?.trim().toLowerCase()
      if (expectedHash && createHash('sha256').update(script).digest('hex') !== expectedHash) {
        throw new Error('Installer checksum does not match PRIME_AGENT_INSTALL_SHA256')
      }
      const scriptPath = join(BIN_DIR, 'install.sh')
      await writeFile(scriptPath, script)
      await chmod(scriptPath, 0o755)
      this.state.progress = 0.6
      this.emit('change', this.stateSnapshot)
      await execFileAsync('sh', [scriptPath], { env: { ...process.env, PATH: `${BIN_DIR}:${process.env.PATH ?? ''}` } })
      await unlink(scriptPath).catch(() => {})
      this.state.progress = 1
      const local = await this.findLocalInstall()
      if (local) {
        this.state.path = local.path
        this.state.version = local.version
        this.state.status = 'installed'
        this.state.error = null
      } else {
        this.state.status = 'error'
        this.state.error = 'Installer ran but prime-agent could not be located.'
      }
    } catch (err) {
      this.state.status = 'error'
      this.state.error = err instanceof Error ? err.message : String(err)
    }
    this.emit('change', this.stateSnapshot)
  }

  private download(url: string, onProgress: (pct: number) => void, redirects = 0): Promise<string> {
    return new Promise((resolve, reject) => {
      if (redirects > 3) {
        reject(new Error('Installer redirected too many times'))
        return
      }
      const req = https.get(url, { headers: { 'User-Agent': 'prime-desktop' } }, (res) => {
        if (res.statusCode && res.statusCode >= 300 && res.statusCode < 400 && res.headers.location) {
          let next: URL
          try {
            next = new URL(res.headers.location, url)
          } catch {
            reject(new Error('Installer redirect URL is invalid'))
            res.resume()
            return
          }
          if (next.protocol !== 'https:' || next.hostname !== 'app.primeintellect.ai') {
            reject(new Error('Installer redirect leaves the trusted Prime Intellect host'))
            res.resume()
            return
          }
          this.download(next.toString(), onProgress, redirects + 1).then(resolve, reject)
          res.resume()
          return
        }
        if (res.statusCode !== 200) {
          reject(new Error(`Download failed with status ${res.statusCode}`))
          res.resume()
          return
        }
        const chunks: Buffer[] = []
        const total = Number(res.headers['content-length'] ?? 0)
        if (total > MAX_INSTALLER_BYTES) {
          reject(new Error('Installer is larger than the allowed limit'))
          res.resume()
          return
        }
        let received = 0
        res.on('data', (c: Buffer) => {
          if (received + c.length > MAX_INSTALLER_BYTES) {
            req.destroy(new Error('Installer is larger than the allowed limit'))
            return
          }
          chunks.push(c)
          received += c.length
          if (total > 0) onProgress(received / total)
        })
        res.on('end', () => resolve(Buffer.concat(chunks).toString('utf8')))
        res.on('error', reject)
      })
      req.on('error', reject)
    })
  }

  getBinary(): string {
    if (this.state.path && existsSync(this.state.path)) return this.state.path
    const p = join(BIN_DIR, 'prime-agent')
    if (existsSync(p)) return p
    return 'prime-agent'
  }
}

export function readAuthFile(): Record<string, string> {
  try {
    const p = join(homedir(), '.prime', 'agent', 'auth.json')
    if (existsSync(p)) {
      return JSON.parse(readFileSyncSafe(p)) as Record<string, string>
    }
  } catch {
    /* ignore */
  }
  return {}
}

function readFileSyncSafe(p: string): string {
  return readFileSync(p, 'utf8')
}

import type { BrowserWindow, IpcMainInvokeEvent } from 'electron'
import { existsSync, statSync } from 'fs'
import { isAbsolute, normalize, resolve, sep } from 'path'
import type { AgentCommand } from '@shared/types'

export function assertTrustedRenderer(event: IpcMainInvokeEvent, getWindow: () => BrowserWindow | null): void {
  const window = getWindow()
  if (!window || window.isDestroyed() || event.sender !== window.webContents) {
    throw new Error('Untrusted IPC sender')
  }
}

export function requireString(value: unknown, name: string, maxLength = 200_000): string {
  if (typeof value !== 'string' || value.length > maxLength) {
    throw new Error(`${name} must be a string of at most ${maxLength} characters`)
  }
  return value
}

export function requireNonEmptyString(value: unknown, name: string, maxLength = 200_000): string {
  const text = requireString(value, name, maxLength).trim()
  if (!text) throw new Error(`${name} is required`)
  return text
}

export function requireFiniteNumber(value: unknown, name: string, min?: number, max?: number): number {
  if (typeof value !== 'number' || !Number.isFinite(value)) throw new Error(`${name} must be a finite number`)
  if (min !== undefined && value < min) throw new Error(`${name} must be at least ${min}`)
  if (max !== undefined && value > max) throw new Error(`${name} must be at most ${max}`)
  return value
}

export function requireExistingDirectory(value: unknown, name: string): string {
  const path = requireNonEmptyString(value, name, 4_096)
  if (!isAbsolute(path)) throw new Error(`${name} must be an absolute path`)
  if (!existsSync(path) || !statSync(path).isDirectory()) throw new Error(`${name} is not an existing directory`)
  return resolve(path)
}

export function requireExistingFile(value: unknown, name: string): string {
  const path = requireNonEmptyString(value, name, 4_096)
  if (!isAbsolute(path)) throw new Error(`${name} must be an absolute path`)
  if (!existsSync(path) || !statSync(path).isFile()) throw new Error(`${name} is not an existing file`)
  return resolve(path)
}

export function requireSafeExternalUrl(value: unknown): string {
  const raw = requireNonEmptyString(value, 'url', 8_192)
  let url: URL
  try {
    url = new URL(raw)
  } catch {
    throw new Error('Invalid external URL')
  }
  if (url.protocol !== 'https:' && url.protocol !== 'http:') {
    throw new Error('Only HTTP(S) external URLs are allowed')
  }
  return url.toString()
}

export function isWithin(root: string, candidate: string): boolean {
  const rootPath = normalize(resolve(root))
  const candidatePath = normalize(resolve(candidate))
  return candidatePath === rootPath || candidatePath.startsWith(`${rootPath}${sep}`)
}

export function validateAgentCommand(value: unknown): AgentCommand {
  if (!value || typeof value !== 'object' || Array.isArray(value)) throw new Error('Invalid agent command')
  const command = value as Record<string, unknown>
  const type = command.type
  const allowed = new Set([
    'prompt', 'steer', 'follow_up', 'abort', 'compact', 'new_session', 'switch_session',
    'fork', 'clone', 'set_model', 'set_thinking_level', 'set_auto_compaction', 'set_auto_retry',
    'set_session_name', 'get_available_models', 'get_commands', 'bash', 'refine', 'export_html'
  ])
  if (typeof type !== 'string' || !allowed.has(type)) throw new Error('Unsupported agent command')
  if (['prompt', 'steer', 'follow_up', 'bash'].includes(type)) {
    requireNonEmptyString(command.message ?? command.command, 'command text', 200_000)
  }
  return command as AgentCommand
}

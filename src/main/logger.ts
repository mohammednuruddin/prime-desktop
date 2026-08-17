import { appendFile, mkdir } from 'fs/promises'
import { homedir } from 'os'
import { join } from 'path'

const LOG_DIR = join(homedir(), 'Library', 'Logs', 'PrimeDesktop')
const LOG_FILE = join(LOG_DIR, 'main.log')

export async function log(level: 'info' | 'warn' | 'error', message: string, detail?: unknown): Promise<void> {
  const suffix = detail === undefined ? '' : ` ${serialize(detail)}`
  const line = `${new Date().toISOString()} [${level}] ${message}${suffix}\n`
  if (level === 'error') console.error(message, detail)
  else if (level === 'warn') console.warn(message, detail)
  else console.info(message, detail)
  try {
    await mkdir(LOG_DIR, { recursive: true })
    await appendFile(LOG_FILE, line, { mode: 0o600 })
  } catch {
    // Logging must never break the application.
  }
}

export function logError(message: string, error: unknown): void {
  void log('error', message, error)
}

function serialize(value: unknown): string {
  if (value instanceof Error) return value.stack ?? value.message
  try {
    return JSON.stringify(value)
  } catch {
    return String(value)
  }
}

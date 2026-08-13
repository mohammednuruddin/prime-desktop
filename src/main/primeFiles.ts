import { existsSync, readFileSync } from 'fs'
import { readFile, writeFile, mkdir } from 'fs/promises'
import { homedir } from 'os'
import { join } from 'path'
import { spawn } from 'child_process'
import type { AuthProvider } from '@shared/types'

export const PRIME_AGENT_DIR = join(homedir(), '.prime', 'agent')
export const AUTH_PATH = join(PRIME_AGENT_DIR, 'auth.json')
export const PRIME_SETTINGS_PATH = join(PRIME_AGENT_DIR, 'settings.json')
export const LOGS_DIR = join(PRIME_AGENT_DIR, 'logs')

const PROVIDERS: { id: string; name: string }[] = [
  { id: 'openai', name: 'OpenAI' },
  { id: 'openai-codex', name: 'OpenAI Codex' },
  { id: 'anthropic', name: 'Anthropic' },
  { id: 'google', name: 'Google Gemini' },
  { id: 'openrouter', name: 'OpenRouter' },
  { id: 'prime-inference', name: 'Prime Inference' },
  { id: 'xai', name: 'xAI' },
  { id: 'groq', name: 'Groq' },
  { id: 'mistral', name: 'Mistral' },
  { id: 'deepseek', name: 'DeepSeek' },
  { id: 'fireworks', name: 'Fireworks' },
  { id: 'cerebras', name: 'Cerebras' },
  { id: 'moonshotai', name: 'Moonshot AI' },
  { id: 'minimax', name: 'MiniMax' },
  { id: 'zai', name: 'ZAI' }
]

type AuthFile = Record<string, { type?: string; key?: string }>

async function readAuth(): Promise<AuthFile> {
  try {
    return JSON.parse(await readFile(AUTH_PATH, 'utf8')) as AuthFile
  } catch {
    return {}
  }
}

export async function listAuthProviders(): Promise<AuthProvider[]> {
  const data = await readAuth()
  const ids = new Set([...PROVIDERS.map((p) => p.id), ...Object.keys(data)])
  return [...ids].map((id) => ({
    id,
    name: PROVIDERS.find((p) => p.id === id)?.name ?? id,
    configured: Boolean(data[id])
  }))
}

export async function setAuthKey(provider: string, key: string): Promise<void> {
  await mkdir(PRIME_AGENT_DIR, { recursive: true })
  const data = await readAuth()
  data[provider] = { type: 'api_key', key: key.trim() }
  await writeFile(AUTH_PATH, JSON.stringify(data, null, 2))
}

export async function removeAuth(provider: string): Promise<void> {
  const data = await readAuth()
  delete data[provider]
  await mkdir(PRIME_AGENT_DIR, { recursive: true })
  await writeFile(AUTH_PATH, JSON.stringify(data, null, 2))
}

export function openPrimeAgentLogin(): void {
  spawn('open', ['-a', 'Terminal', join(homedir(), '')], { detached: true, stdio: 'ignore' }).unref()
  spawn('osascript', [
    '-e',
    'tell application "Terminal" to do script "prime-agent"'
  ], { detached: true, stdio: 'ignore' }).unref()
}

export async function readPrimeSettings(): Promise<Record<string, unknown>> {
  try {
    return JSON.parse(await readFile(PRIME_SETTINGS_PATH, 'utf8')) as Record<string, unknown>
  } catch {
    return {}
  }
}

export async function writePrimeRlmMaxDepth(maxDepth: number): Promise<void> {
  await mkdir(PRIME_AGENT_DIR, { recursive: true })
  const data = await readPrimeSettings()
  data.rlmMaxDepth = maxDepth
  await writeFile(PRIME_SETTINGS_PATH, JSON.stringify(data, null, 2))
}

export async function setAgentTracesEnabled(enabled: boolean): Promise<void> {
  await mkdir(PRIME_AGENT_DIR, { recursive: true })
  const data = await readPrimeSettings()
  const current = data.agentTraces && typeof data.agentTraces === 'object'
    ? data.agentTraces as Record<string, unknown>
    : {}
  data.agentTraces = { ...current, enabled }
  await writeFile(PRIME_SETTINGS_PATH, JSON.stringify(data, null, 2))
}

export async function getAgentTracesEnabled(): Promise<boolean> {
  const data = await readPrimeSettings()
  const traces = data.agentTraces as Record<string, unknown> | undefined
  return traces?.enabled === true
}

export async function readPrimeRlmMaxDepth(fallback = 1): Promise<number> {
  const data = await readPrimeSettings()
  const value = data.rlmMaxDepth
  return typeof value === 'number' && Number.isInteger(value) && value >= 0 ? value : fallback
}

export function changelogText(): string {
  const candidates = [
    '/opt/homebrew/lib/node_modules/prime-agent/CHANGELOG.md',
    join(homedir(), '.local/lib/node_modules/prime-agent/CHANGELOG.md')
  ]
  for (const path of candidates) {
    if (existsSync(path)) {
      try {
        return readFileSync(path, 'utf8').slice(0, 12000)
      } catch {
        /* try next */
      }
    }
  }
  return 'Changelog not found. Installed Prime Agent package did not include CHANGELOG.md.'
}

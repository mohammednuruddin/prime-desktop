import { readFile, writeFile, mkdir, rename } from 'fs/promises'
import { homedir } from 'os'
import { join } from 'path'
import { existsSync } from 'fs'
import type { AppSettings, ProjectTab } from '@shared/types'
import { CODEX_DARK_THEME, PRIME_LIGHT_THEME } from '@shared/themes'

export interface StoredState {
  settings: AppSettings
  tabs: ProjectTab[]
  activeTabId: string | null
  model: string | null
  pinnedSessionFiles: string[]
}

const STATE_FILE = join(homedir(), 'Library', 'Application Support', 'PrimeDesktop', 'state.json')

const DEFAULTS: AppSettings = {
  notifications: true,
  checkpoints: true,
  dockBadge: false,
  thinkingLevel: 'medium',
  showReasoning: true,
  autoCompaction: true,
  autoRetry: true,
  model: null,
  rlmMaxDepth: 1,
  transport: 'auto',
  autonomous: {
    enabled: false,
    gates: [],
    gateRetries: 2,
    maxContinuations: 8,
    maxTurns: 30,
    maxTokens: 100000,
    maxSeconds: 3600
  },
  themeMode: 'system',
  codeThemeId: 'codex',
  lightTheme: PRIME_LIGHT_THEME,
  darkTheme: CODEX_DARK_THEME
}

let cache: StoredState | null = null
let saveTail: Promise<void> = Promise.resolve()

async function load(): Promise<StoredState> {
  if (cache) return cache
  try {
    if (existsSync(STATE_FILE)) {
      const raw = JSON.parse(await readFile(STATE_FILE, 'utf8')) as Partial<StoredState>
      cache = {
        settings: { ...DEFAULTS, ...(raw.settings ?? {}) },
        tabs: raw.tabs ?? [],
        activeTabId: raw.activeTabId ?? null,
        model: raw.model ?? null,
        pinnedSessionFiles: raw.pinnedSessionFiles ?? []
      }
    } else {
      cache = { settings: { ...DEFAULTS }, tabs: [], activeTabId: null, model: null, pinnedSessionFiles: [] }
    }
  } catch {
    cache = { settings: { ...DEFAULTS }, tabs: [], activeTabId: null, model: null, pinnedSessionFiles: [] }
  }
  return cache
}

async function save(): Promise<void> {
  if (!cache) return
  const contents = JSON.stringify(cache, null, 2)
  const tempFile = `${STATE_FILE}.${process.pid}.tmp`
  const write = saveTail.catch(() => {}).then(async () => {
    await mkdir(join(STATE_FILE, '..'), { recursive: true })
    await writeFile(tempFile, contents)
    await rename(tempFile, STATE_FILE)
  })
  saveTail = write.catch(() => {})
  await write
}

export async function getState(): Promise<StoredState> {
  return load()
}

export async function setSettings(patch: Partial<AppSettings>): Promise<AppSettings> {
  const s = await load()
  s.settings = { ...s.settings, ...patch }
  await save()
  return s.settings
}

export async function setTabs(tabs: ProjectTab[], activeTabId: string | null): Promise<void> {
  const s = await load()
  s.tabs = tabs
  s.activeTabId = activeTabId
  await save()
}

export async function setModel(model: string | null): Promise<void> {
  const s = await load()
  s.model = model
  await save()
}

export async function setSessionPinned(sessionFile: string, pinned: boolean): Promise<string[]> {
  const s = await load()
  const next = new Set(s.pinnedSessionFiles)
  if (pinned) next.add(sessionFile)
  else next.delete(sessionFile)
  s.pinnedSessionFiles = [...next]
  await save()
  return s.pinnedSessionFiles
}

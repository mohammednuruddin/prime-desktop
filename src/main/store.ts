import { readFile, writeFile, mkdir, rename } from 'fs/promises'
import { homedir } from 'os'
import { join } from 'path'
import { existsSync } from 'fs'
import type { AppSettings, ProjectTab } from '@shared/types'
import { CODEX_DARK_THEME, PRIME_LIGHT_THEME } from '@shared/themes'

export interface StoredState {
  version: number
  settings: AppSettings
  tabs: ProjectTab[]
  activeTabId: string | null
  model: string | null
  pinnedSessionFiles: string[]
}

const STATE_FILE = join(homedir(), 'Library', 'Application Support', 'PrimeDesktop', 'state.json')
const STATE_VERSION = 2

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
      cache = migrateState(raw)
    } else {
      cache = { version: STATE_VERSION, settings: cloneDefaults(), tabs: [], activeTabId: null, model: null, pinnedSessionFiles: [] }
    }
  } catch {
    cache = { version: STATE_VERSION, settings: cloneDefaults(), tabs: [], activeTabId: null, model: null, pinnedSessionFiles: [] }
  }
  return cache
}

function cloneDefaults(): AppSettings {
  return structuredClone(DEFAULTS)
}

function migrateState(raw: Partial<StoredState>): StoredState {
  const rawSettings = raw.settings && typeof raw.settings === 'object' ? raw.settings as Partial<AppSettings> : {}
  const autonomous = rawSettings.autonomous && typeof rawSettings.autonomous === 'object' && !Array.isArray(rawSettings.autonomous)
    ? rawSettings.autonomous as Partial<AppSettings['autonomous']>
    : {}
  const settings: AppSettings = {
    ...cloneDefaults(),
    ...rawSettings,
    autonomous: { ...DEFAULTS.autonomous, ...autonomous },
    lightTheme: mergeTheme(DEFAULTS.lightTheme, rawSettings.lightTheme),
    darkTheme: mergeTheme(DEFAULTS.darkTheme, rawSettings.darkTheme),
    rlmMaxDepth: integerOr(DEFAULTS.rlmMaxDepth, rawSettings.rlmMaxDepth, 0, 10),
    themeMode: rawSettings.themeMode === 'light' || rawSettings.themeMode === 'dark' ? rawSettings.themeMode : 'system',
    transport: rawSettings.transport === 'sse' || rawSettings.transport === 'websocket' || rawSettings.transport === 'websocket-cached'
      ? rawSettings.transport
      : 'auto'
  }
  const tabs = Array.isArray(raw.tabs)
    ? raw.tabs.filter((tab): tab is ProjectTab =>
      Boolean(tab && typeof tab.id === 'string' && typeof tab.path === 'string' && typeof tab.name === 'string'))
    : []
  const activeTabId = tabs.some((tab) => tab.id === raw.activeTabId) ? raw.activeTabId ?? null : tabs[0]?.id ?? null
  return {
    version: STATE_VERSION,
    settings,
    tabs,
    activeTabId,
    model: typeof raw.model === 'string' ? raw.model : null,
    pinnedSessionFiles: Array.isArray(raw.pinnedSessionFiles)
      ? raw.pinnedSessionFiles.filter((path): path is string => typeof path === 'string')
      : []
  }
}

function integerOr(fallback: number, value: unknown, min: number, max: number): number {
  return typeof value === 'number' && Number.isInteger(value) && value >= min && value <= max ? value : fallback
}

function mergeTheme(base: AppSettings['lightTheme'], patch: unknown): AppSettings['lightTheme'] {
  if (!patch || typeof patch !== 'object' || Array.isArray(patch)) return structuredClone(base)
  const raw = patch as Partial<AppSettings['lightTheme']>
  const semantic = raw.semanticColors && typeof raw.semanticColors === 'object' && !Array.isArray(raw.semanticColors) ? raw.semanticColors : {}
  const fonts = raw.fonts && typeof raw.fonts === 'object' && !Array.isArray(raw.fonts) ? raw.fonts : {}
  return {
    ...base,
    ...raw,
    fonts: { ...base.fonts, ...fonts },
    semanticColors: { ...base.semanticColors, ...semantic }
  }
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

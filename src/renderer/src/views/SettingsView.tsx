import { useEffect, useRef, useState } from 'react'
import type { AppSettings, AuthProvider, ModelCatalog, ThemeConfig, ThemeMode } from '@shared/types'
import { CODEX_DARK_THEME, PRIME_LIGHT_THEME, resolveThemeMode } from '@shared/themes'
import DepthSlider from '../components/DepthSlider'

const THINKING_LEVELS = ['off', 'minimal', 'low', 'medium', 'high', 'xhigh']

interface Props {
  settings: AppSettings
  activeAgentId: string | null
  onChange: (patch: Partial<AppSettings>) => void
}

interface RefinementResult {
  id: string
  summary: string
  scope?: 'local' | 'global'
  appliedEdits?: { applied: boolean }[]
}

export default function SettingsView({ settings, activeAgentId, onChange }: Props): JSX.Element {
  const [systemDark, setSystemDark] = useState(() => window.matchMedia('(prefers-color-scheme: dark)').matches)
  const [copyLabel, setCopyLabel] = useState('Copy theme')
  const [importError, setImportError] = useState('')
  const [providers, setProviders] = useState<AuthProvider[]>([])
  const [providerKeys, setProviderKeys] = useState<Record<string, string>>({})
  const [catalog, setCatalog] = useState<ModelCatalog | null>(null)
  const [refineInstructions, setRefineInstructions] = useState('')
  const [refinements, setRefinements] = useState<RefinementResult[]>([])
  const [refining, setRefining] = useState(false)
  const [settingsMessage, setSettingsMessage] = useState('')
  const importRef = useRef<HTMLInputElement>(null)

  useEffect(() => {
    const media = window.matchMedia('(prefers-color-scheme: dark)')
    const update = () => setSystemDark(media.matches)
    media.addEventListener('change', update)
    return () => media.removeEventListener('change', update)
  }, [])

  useEffect(() => {
    void window.prime.authList().then(setProviders)
    if (!activeAgentId) {
      setCatalog(null)
      setRefinements([])
      return
    }
    void window.prime.agentHarness(activeAgentId, 'model_catalog').then((value) => {
      const raw = value as { models?: Record<string, unknown>[]; configuredProviders?: string[] }
      setCatalog({
        models: (raw.models ?? []).map((model) => ({
          id: String(model.id ?? ''),
          name: typeof model.name === 'string' ? model.name : undefined,
          provider: String(model.provider ?? ''),
          reasoning: Boolean(model.reasoning)
        })),
        configuredProviders: raw.configuredProviders ?? [],
        transport: settings.transport
      })
    }).catch(() => setCatalog(null))
    loadRefinements()
  }, [activeAgentId])

  const set = (patch: Partial<AppSettings>) => onChange(patch)
  const editingVariant = resolveThemeMode(settings.themeMode, systemDark)
  const themeKey = editingVariant === 'dark' ? 'darkTheme' : 'lightTheme'
  const theme = settings[themeKey]

  const updateTheme = (patch: Partial<ThemeConfig>) => {
    set({ [themeKey]: { ...theme, ...patch } })
  }

  const copyTheme = async () => {
    const payload = `codex-theme-v1:${JSON.stringify({
      codeThemeId: settings.codeThemeId,
      theme,
      variant: editingVariant
    })}`
    await navigator.clipboard.writeText(payload)
    setCopyLabel('Copied')
    window.setTimeout(() => setCopyLabel('Copy theme'), 1600)
  }

  const importTheme = async (file: File) => {
    try {
      const raw = (await file.text()).trim().replace(/^codex-theme-v1:/, '')
      const parsed = JSON.parse(raw) as { codeThemeId?: string; theme?: ThemeConfig; variant?: 'light' | 'dark' }
      if (!parsed.theme || !parsed.variant || !isThemeConfig(parsed.theme)) throw new Error('Invalid Codex theme')
      set({
        themeMode: parsed.variant,
        codeThemeId: parsed.codeThemeId ?? 'codex',
        [parsed.variant === 'dark' ? 'darkTheme' : 'lightTheme']: parsed.theme
      })
      setImportError('')
    } catch {
      setImportError('This file is not a valid codex-theme-v1 theme.')
    }
  }

  function loadRefinements(): void {
    if (!activeAgentId) return
    void window.prime.agentHarness(activeAgentId, 'refinement_history').then((value) => {
      setRefinements((value as { history?: RefinementResult[] }).history ?? [])
    }).catch(() => setRefinements([]))
  }

  async function runRefine(global: boolean, rollbackId?: string): Promise<void> {
    if (!activeAgentId) return
    setRefining(true)
    setSettingsMessage(rollbackId ? 'Rolling back refinement…' : 'Running refinement pass…')
    try {
      const result = await window.prime.agentHarness(activeAgentId, 'refine', {
        instructions: refineInstructions.trim() || undefined,
        rollbackId,
        global
      }) as RefinementResult
      setSettingsMessage(result.summary || (rollbackId ? 'Rollback complete.' : 'Refinement complete.'))
      setRefineInstructions('')
      loadRefinements()
    } catch (error) {
      setSettingsMessage(error instanceof Error ? error.message : String(error))
    } finally {
      setRefining(false)
    }
  }

  return (
    <div className="view settings-page">
      <header className="view-header">
        <h2>Settings</h2>
        <p className="view-sub">Preferences shared with Prime Agent on this Mac.</p>
      </header>

      <section className="theme-settings">
        <div className="theme-section-title">Appearance</div>
        <div className="theme-mode-grid" role="radiogroup" aria-label="Theme">
          {(['system', 'light', 'dark'] as ThemeMode[]).map((mode) => (
            <button
              key={mode}
              className={`theme-mode-option ${settings.themeMode === mode ? 'selected' : ''}`}
              type="button"
              role="radio"
              aria-checked={settings.themeMode === mode}
              onClick={() => set({ themeMode: mode })}
            >
              <ThemePreview mode={mode} />
              <span>{mode[0].toUpperCase() + mode.slice(1)}</span>
            </button>
          ))}
        </div>

        <div className="theme-code-preview" aria-hidden="true">
          <div className="theme-code-gutter">1<br /><b>2</b><br /><b>3</b><br /><b>4</b><br />5</div>
          <pre><span>const</span> themePreview = {'{'}{'\n'}  surface: <em>"sidebar"</em>,{'\n'}  accent: <em>"{theme.accent}"</em>,{'\n'}  contrast: <strong>{theme.contrast}</strong>,{'\n'}{'}'};</pre>
          <div className="theme-code-result">
            <div />
            <div style={{ background: `${theme.semanticColors.diffAdded}22`, borderLeftColor: theme.semanticColors.diffAdded }} />
            <div style={{ background: `${theme.semanticColors.diffRemoved}22`, borderLeftColor: theme.semanticColors.diffRemoved }} />
          </div>
        </div>

        <div className="theme-editor">
          <div className="theme-editor-head">
            <span>{editingVariant === 'dark' ? 'Dark' : 'Light'} theme</span>
            <div className="theme-editor-actions">
              <input
                ref={importRef}
                hidden
                type="file"
                accept=".json,.txt"
                onChange={(event) => {
                  const file = event.target.files?.[0]
                  if (file) void importTheme(file)
                  event.target.value = ''
                }}
              />
              <button type="button" onClick={() => importRef.current?.click()}>Import</button>
              <button type="button" onClick={() => void copyTheme()}>{copyLabel}</button>
              <select
                className="theme-preset"
                aria-label="Theme preset"
                value={editingVariant === 'dark' && theme.accent.toLowerCase() === '#339cff' ? 'codex-dark' : editingVariant === 'light' && theme.accent.toLowerCase() === '#c97b76' ? 'prime-light' : 'custom'}
                onChange={(event) => {
                  if (event.target.value === 'codex-dark') set({ themeMode: 'dark', darkTheme: CODEX_DARK_THEME, codeThemeId: 'codex' })
                  if (event.target.value === 'prime-light') set({ themeMode: 'light', lightTheme: PRIME_LIGHT_THEME, codeThemeId: 'codex' })
                }}
              >
                <option value="custom">Custom</option>
                <option value="codex-dark">Codex Dark</option>
                <option value="prime-light">Prime Light</option>
              </select>
            </div>
          </div>
          {importError && <div className="theme-import-error">{importError}</div>}
          <ThemeColorRow label="Accent" value={theme.accent} onChange={(accent) => updateTheme({ accent })} />
          <ThemeColorRow label="Background" value={theme.surface} onChange={(surface) => updateTheme({ surface })} />
          <ThemeColorRow label="Foreground" value={theme.ink} onChange={(ink) => updateTheme({ ink })} />
          <ThemeColorRow label="Diff added" value={theme.semanticColors.diffAdded} onChange={(diffAdded) => updateTheme({ semanticColors: { ...theme.semanticColors, diffAdded } })} />
          <ThemeColorRow label="Diff removed" value={theme.semanticColors.diffRemoved} onChange={(diffRemoved) => updateTheme({ semanticColors: { ...theme.semanticColors, diffRemoved } })} />
          <ThemeColorRow label="Skill" value={theme.semanticColors.skill} onChange={(skill) => updateTheme({ semanticColors: { ...theme.semanticColors, skill } })} />
          <label className="theme-control-row">
            <span>UI font</span>
            <input
              className="theme-text-input"
              value={theme.fonts.ui ?? ''}
              placeholder="System default"
              onChange={(event) => updateTheme({ fonts: { ...theme.fonts, ui: event.target.value || null } })}
            />
          </label>
          <label className="theme-control-row">
            <span>Code font</span>
            <input
              className="theme-text-input"
              value={theme.fonts.code ?? ''}
              placeholder="SF Mono"
              onChange={(event) => updateTheme({ fonts: { ...theme.fonts, code: event.target.value || null } })}
            />
          </label>
          <label className="theme-control-row">
            <span>Translucent sidebar</span>
            <input className="setting-toggle" type="checkbox" checked={!theme.opaqueWindows} onChange={(event) => updateTheme({ opaqueWindows: !event.target.checked })} />
          </label>
          <label className="theme-control-row theme-contrast-row">
            <span>Contrast</span>
            <input type="range" min="0" max="100" value={theme.contrast} onChange={(event) => updateTheme({ contrast: Number(event.target.value) })} />
            <output>{theme.contrast}</output>
          </label>
        </div>
      </section>

      <section className="panel">
        <div className="panel-head">Behavior</div>
        <label className="setting-row">
          <div>
            <div className="setting-title">Native notifications</div>
            <div className="setting-desc">Notify when a background agent finishes or needs input.</div>
          </div>
          <input className="setting-toggle" type="checkbox" checked={settings.notifications} onChange={(e) => set({ notifications: e.target.checked })} />
        </label>
        <label className="setting-row">
          <div>
            <div className="setting-title">Auto-checkpoint before prompts</div>
            <div className="setting-desc">Create a git commit checkpoint before each prompt (git repos only).</div>
          </div>
          <input className="setting-toggle" type="checkbox" checked={settings.checkpoints} onChange={(e) => set({ checkpoints: e.target.checked })} />
        </label>
        <label className="setting-row">
          <div>
            <div className="setting-title">Auto-compaction</div>
            <div className="setting-desc">Summarize older messages when context gets full.</div>
          </div>
          <input className="setting-toggle" type="checkbox" checked={settings.autoCompaction} onChange={(e) => set({ autoCompaction: e.target.checked })} />
        </label>
        <label className="setting-row">
          <div>
            <div className="setting-title">Auto-retry</div>
            <div className="setting-desc">Retry on transient provider errors (overloaded, rate limit, 5xx).</div>
          </div>
          <input className="setting-toggle" type="checkbox" checked={settings.autoRetry} onChange={(e) => set({ autoRetry: e.target.checked })} />
        </label>
        <label className="setting-row">
          <div>
            <div className="setting-title">Autonomous continuation</div>
            <div className="setting-desc">Let Prime Agent keep going until gates pass or a budget runs out. Toggle in chat with /autonomous on.</div>
          </div>
          <input
            className="setting-toggle"
            type="checkbox"
            checked={settings.autonomous.enabled}
            onChange={(e) => {
              const autonomous = { ...settings.autonomous, enabled: e.target.checked }
              set({ autonomous })
              void window.prime.autonomySet({ enabled: e.target.checked })
            }}
          />
        </label>
      </section>

      <section className="panel">
        <div className="panel-head">Model & reasoning</div>
        <label className="setting-row">
          <div>
            <div className="setting-title">Thinking level</div>
            <div className="setting-desc">Reasoning effort for supported models. xhigh is codex-max only.</div>
          </div>
          <select
            className="field"
            value={settings.thinkingLevel}
            onChange={(e) => set({ thinkingLevel: e.target.value })}
          >
            {THINKING_LEVELS.map((l) => (
              <option key={l} value={l}>
                {l}
              </option>
            ))}
          </select>
        </label>
        <div className="setting-stack">
          <div>
            <div className="setting-title">Recursive depth</div>
            <div className="setting-desc">How many nested subagents a chat may spawn. 0 keeps work in the parent.</div>
          </div>
          <DepthSlider
            value={settings.rlmMaxDepth ?? 1}
            onChange={(rlmMaxDepth) => set({ rlmMaxDepth })}
          />
        </div>
        <label className="setting-row">
          <div>
            <div className="setting-title">Provider transport</div>
            <div className="setting-desc">Applied directly to the active daemon session. Auto lets Prime Agent choose.</div>
          </div>
          <select
            className="field"
            value={settings.transport}
            onChange={(event) => {
              const transport = event.target.value as AppSettings['transport']
              set({ transport })
              if (activeAgentId) void window.prime.agentHarness(activeAgentId, 'set_transport', { transport })
            }}
          >
            <option value="auto">auto</option>
            <option value="sse">SSE</option>
            <option value="websocket">WebSocket</option>
            <option value="websocket-cached">WebSocket cached</option>
          </select>
        </label>
        {catalog && (
          <div className="setting-desc pad-top">
            {catalog.models.length} models · {catalog.configuredProviders.length} configured providers
          </div>
        )}
      </section>

      <section className="panel">
        <div className="panel-head">Harness refinement</div>
        <p className="setting-desc">Refine prompt notes, memory, skills, and subagent definitions through Prime Agent.</p>
        <textarea
          className="field refinement-instructions"
          placeholder="Optional instructions for the refinement pass"
          value={refineInstructions}
          onChange={(event) => setRefineInstructions(event.target.value)}
        />
        <div className="row-gap pad-top">
          <button className="btn primary small" disabled={!activeAgentId || refining} onClick={() => void runRefine(false)}>
            {refining ? 'Refining…' : 'Refine session'}
          </button>
          <button className="btn small" disabled={!activeAgentId || refining} onClick={() => void runRefine(true)}>Refine globally</button>
        </div>
        {settingsMessage && <div className="hint-text pad-top">{settingsMessage}</div>}
        <div className="refinement-history">
          {refinements.map((item) => (
            <div className="refinement-row" key={item.id}>
              <div>
                <strong>{item.summary || item.id}</strong>
                <span>{item.scope ?? 'local'} · {item.appliedEdits?.filter((edit) => edit.applied).length ?? 0} edits</span>
              </div>
              <button
                className="btn ghost small"
                disabled={refining}
                onClick={() => {
                  if (window.confirm(`Roll back refinement “${item.summary || item.id}”?`)) void runRefine(item.scope === 'global', item.id)
                }}
              >
                Roll back
              </button>
            </div>
          ))}
          {refinements.length === 0 && <div className="empty-state">No refinements recorded in this session.</div>}
        </div>
      </section>

      <section className="panel">
        <div className="panel-head">Credentials</div>
        <p className="setting-desc">Keys are written to Prime Agent’s credential file with owner-only permissions.</p>
        <div className="provider-list">
          {providers.map((provider) => (
            <div className="provider-row" key={provider.id}>
              <div>
                <strong>{provider.name}</strong>
                <span>{provider.configured ? 'configured' : 'not configured'}</span>
              </div>
              <input
                className="field"
                type="password"
                autoComplete="off"
                placeholder={provider.configured ? 'Replace API key' : 'API key'}
                value={providerKeys[provider.id] ?? ''}
                onChange={(event) => setProviderKeys((value) => ({ ...value, [provider.id]: event.target.value }))}
              />
              <button
                className="btn small"
                disabled={!providerKeys[provider.id]?.trim()}
                onClick={() => {
                  void window.prime.authSet(provider.id, providerKeys[provider.id]).then(() => {
                    setProviderKeys((value) => ({ ...value, [provider.id]: '' }))
                    void window.prime.authList().then(setProviders)
                  })
                }}
              >
                Save
              </button>
              {provider.configured && (
                <button className="btn ghost small" onClick={() => void window.prime.authRemove(provider.id).then(() => window.prime.authList().then(setProviders))}>Remove</button>
              )}
            </div>
          ))}
        </div>
        <button className="btn small pad-top" onClick={() => void window.prime.authOpenTui()}>Open Prime Agent login</button>
      </section>

      <section className="panel">
        <div className="panel-head">About</div>
        <div className="setting-desc">
          Prime Desktop 0.1.0 — open-source Electron shell for prime-agent (MIT). Built on the{' '}
          resident daemon connection. Workers run with your user permissions — they are not a security sandbox.
        </div>
      </section>
    </div>
  )
}

function ThemeColorRow({ label, value, onChange }: { label: string; value: string; onChange: (value: string) => void }) {
  const [draft, setDraft] = useState(value.toUpperCase())
  useEffect(() => setDraft(value.toUpperCase()), [value])

  return (
    <label className="theme-control-row">
      <span>{label}</span>
      <span className="theme-color-control">
        <input type="color" value={value} onChange={(event) => onChange(event.target.value)} />
        <input
          value={draft}
          maxLength={7}
          onBlur={() => setDraft(value.toUpperCase())}
          onChange={(event) => {
            const next = event.target.value
            setDraft(next)
            if (/^#[0-9a-f]{6}$/i.test(next)) onChange(next)
          }}
        />
      </span>
    </label>
  )
}

function ThemePreview({ mode }: { mode: ThemeMode }) {
  return (
    <span className={`theme-preview ${mode}`}>
      <i className="theme-preview-top" />
      <i className="theme-preview-window">
        <b />
        <b />
        <b />
      </i>
    </span>
  )
}

function isThemeConfig(value: ThemeConfig): boolean {
  return Boolean(
    value &&
    /^#[0-9a-f]{6}$/i.test(value.accent) &&
    /^#[0-9a-f]{6}$/i.test(value.ink) &&
    /^#[0-9a-f]{6}$/i.test(value.surface) &&
    value.semanticColors &&
    /^#[0-9a-f]{6}$/i.test(value.semanticColors.diffAdded) &&
    /^#[0-9a-f]{6}$/i.test(value.semanticColors.diffRemoved) &&
    /^#[0-9a-f]{6}$/i.test(value.semanticColors.skill) &&
    value.fonts &&
    typeof value.contrast === 'number'
  )
}

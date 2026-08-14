import { useEffect, useMemo, useRef, useState, type ReactNode } from 'react'
import AccessPicker, { type AccessMode } from './AccessPicker'
import DepthSlider from './DepthSlider'
import type { ModelOption } from '@shared/models'
import type { ProjectTab } from '@shared/types'
import { BUILTIN_SLASH_COMMANDS, getSlashCommand, isBuiltinSlash, parseSlash, slashOpensOnPick } from '@shared/slash'

function mergeCommands(
  api: { name: string; description?: string }[]
): { name: string; description: string; takesArgument?: boolean }[] {
  const map = new Map<string, { name: string; description: string; takesArgument?: boolean }>()
  for (const c of BUILTIN_SLASH_COMMANDS) {
    map.set(c.name, { name: c.name, description: c.description, takesArgument: c.takesArgument })
    for (const alias of c.aliases ?? []) {
      map.set(alias, { name: alias, description: `Alias for /${c.name}`, takesArgument: c.takesArgument })
    }
  }
  for (const c of api) {
    if (!map.has(c.name)) map.set(c.name, { name: c.name, description: c.description ?? '' })
  }
  return Array.from(map.values())
}

interface Props {
  busy: boolean
  commands: { name: string; description?: string }[]
  onSend: (text: string, images: { type: 'image'; data: string; mimeType: string }[]) => void
  onSlash: (text: string) => void
  onAbort: () => void
  onBash: (cmd: string) => void
  models?: ModelOption[]
  currentModel?: string
  onSelectModel?: (model: string) => void
  effortLevel?: string
  onSelectEffort?: (effort: string) => void
  accessMode?: AccessMode
  onAccessModeChange?: (mode: AccessMode) => void
  rlmMaxDepth?: number
  onDepthChange?: (depth: number) => void
  openPicker?: 'models' | 'effort' | 'depth' | null
  onPickerConsumed?: () => void
  projectName?: string
  branch?: string | null
  projects?: ProjectTab[]
  activeProjectId?: string | null
  onSelectProject?: (projectId: string) => void
  onNewProject?: () => void
  onBranchClick?: () => void
  externalText?: string
  banner?: ReactNode
}

const EFFORT_LEVELS = ['Light', 'Medium', 'High', 'Extra High', 'Max']

/** Extract a short display name from a full model path */
function shortModelName(raw: string): string {
  if (!raw) return 'Model'
  const id = raw.includes('/') ? raw.split('/').pop()! : raw
  // openai-codex style: gpt-5.6-sol → 5.6 Sol
  const m56 = id.match(/gpt-(\d+\.\d+)-(\w+)/i)
  if (m56) return `${m56[1]} ${m56[2].charAt(0).toUpperCase() + m56[2].slice(1)}`
  // gpt-4o → GPT-4o
  if (/gpt-4o/i.test(id)) return 'GPT-4o'
  if (/gpt-4/i.test(id)) return 'GPT-4'
  if (/o3-mini/i.test(id)) return 'o3-mini'
  if (/o3/i.test(id)) return 'o3'
  // claude-opus-5 → Opus 5
  const claudeM = id.match(/claude-([a-z]+)-(\d)/i)
  if (claudeM) return `${claudeM[1].charAt(0).toUpperCase() + claudeM[1].slice(1)} ${claudeM[2]}`
  // fallback — trim
  return id.length > 12 ? id.slice(0, 12) : id
}

export default function Composer({
  busy,
  commands,
  onSend,
  onSlash,
  onAbort,
  onBash,
  models = [],
  currentModel,
  onSelectModel,
  effortLevel = 'Light',
  onSelectEffort,
  accessMode = 'ask',
  onAccessModeChange,
  rlmMaxDepth = 1,
  onDepthChange,
  openPicker = null,
  onPickerConsumed,
  projectName,
  branch,
  projects = [],
  activeProjectId,
  onSelectProject,
  onNewProject,
  onBranchClick,
  externalText,
  banner
}: Props): JSX.Element {
  const [text, setText] = useState('')
  const [images, setImages] = useState<{ type: 'image'; data: string; mimeType: string }[]>([])
  const [showCmds, setShowCmds] = useState(false)
  const [cmdFilter, setCmdFilter] = useState('')
  const [cmdIndex, setCmdIndex] = useState(0)
  const [showModelPicker, setShowModelPicker] = useState(false)
  const [showProjectPicker, setShowProjectPicker] = useState(false)
  const [projectQuery, setProjectQuery] = useState('')
  const [chosenModel, setChosenModel] = useState<string | null>(null)
  const [pickerView, setPickerView] = useState<'main' | 'models' | 'effort' | 'depth'>('main')
  const [modelQuery, setModelQuery] = useState('')

  const taRef = useRef<HTMLTextAreaElement>(null)
  const pickerRef = useRef<HTMLDivElement>(null)
  const projectPickerRef = useRef<HTMLDivElement>(null)
  const fileRef = useRef<HTMLInputElement>(null)

  useEffect(() => {
    if (externalText === undefined) return
    setText(externalText)
    requestAnimationFrame(() => taRef.current?.focus())
  }, [externalText])

  useEffect(() => {
    function handleClickOutside(e: MouseEvent) {
      if (pickerRef.current && !pickerRef.current.contains(e.target as Node)) {
        setShowModelPicker(false)
        setPickerView('main')
        setModelQuery('')
      }
      if (projectPickerRef.current && !projectPickerRef.current.contains(e.target as Node)) {
        setShowProjectPicker(false)
        setProjectQuery('')
      }
    }
    if (showModelPicker || showProjectPicker) document.addEventListener('mousedown', handleClickOutside)
    return () => document.removeEventListener('mousedown', handleClickOutside)
  }, [showModelPicker, showProjectPicker])

  useEffect(() => {
    if (!openPicker) return
    setShowModelPicker(true)
    setPickerView(openPicker)
    onPickerConsumed?.()
  }, [openPicker, onPickerConsumed])

  useEffect(() => {
    const el = taRef.current
    if (el) {
      el.style.height = 'auto'
      el.style.height = `${Math.max(44, Math.min(el.scrollHeight, 200))}px`
    }
  }, [text])

  const filtered = useMemo(() => {
    const merged = mergeCommands(commands)
    const q = cmdFilter.split(/\s+/)[0]?.toLowerCase() ?? ''
    const list = q
      ? merged.filter((c) => c.name.toLowerCase().includes(q) || c.description.toLowerCase().includes(q))
      : merged
    return list.slice(0, 40)
  }, [commands, cmdFilter])

  useEffect(() => {
    setCmdIndex(0)
  }, [cmdFilter])

  function runSlash(raw: string) {
    onSlash(raw)
    setText('')
    setImages([])
    setShowCmds(false)
    setCmdFilter('')
  }

  function submit() {
    const t = text.trim()
    if (!t && images.length === 0) return
    if (t.startsWith('!')) { onBash(t.slice(1)); setText(''); return }
    const parsed = parseSlash(t)
    if (parsed && (isBuiltinSlash(parsed.name) || parsed.name.includes(':'))) {
      runSlash(t)
      return
    }
    onSend(t, images)
    setText('')
    setImages([])
    setShowCmds(false)
  }

  function onKeyDown(e: React.KeyboardEvent) {
    if (showCmds && filtered.length > 0 && (e.key === 'ArrowDown' || e.key === 'ArrowUp')) {
      e.preventDefault()
      setCmdIndex((i) => {
        if (e.key === 'ArrowDown') return (i + 1) % filtered.length
        return (i - 1 + filtered.length) % filtered.length
      })
      return
    }
    if (e.key === 'Tab' && showCmds && filtered.length > 0) {
      e.preventDefault()
      insertCommand(filtered[cmdIndex]?.name ?? filtered[0].name)
      return
    }
    if (e.key === 'Enter' && !e.shiftKey) {
      e.preventDefault()
      if (showCmds && filtered.length > 0) {
        const picked = filtered[cmdIndex] ?? filtered[0]
        const rest = text.replace(/^\/\S*\s*/, '').trim()
        runSlash(`/${picked.name}${rest ? ` ${rest}` : ''}`)
        return
      }
      submit()
    } else if (e.key === 'Escape') {
      setShowCmds(false)
      setShowModelPicker(false)
      setPickerView('main')
    }
  }

  function insertCommand(name: string) {
    const def = getSlashCommand(name)
    const suffix = def?.takesArgument ? ' ' : ''
    setText(`/${name}${suffix}`)
    setShowCmds(false)
    setCmdFilter('')
    requestAnimationFrame(() => {
      const el = taRef.current
      if (!el) return
      el.focus()
      const pos = el.value.length
      el.setSelectionRange(pos, pos)
    })
  }

  function handleFileAdd(files: FileList | null) {
    if (!files) return
    for (const file of Array.from(files)) {
      if (!file.type.startsWith('image/')) continue
      const reader = new FileReader()
      reader.onload = (ev) => {
        const data = (ev.target?.result as string)?.split(',')[1]
        if (data) setImages((p) => [...p, { type: 'image', data, mimeType: file.type }])
      }
      reader.readAsDataURL(file)
    }
  }

  const activeModelRaw = chosenModel ?? currentModel ?? ''
  const modelShort = shortModelName(activeModelRaw)
  const pillLabel = `${modelShort} ${effortLevel}`

  const modelList = models.length > 0 ? models : activeModelRaw ? [{ key: activeModelRaw, provider: '', id: activeModelRaw, name: shortModelName(activeModelRaw) }] : []
  const filteredModels = modelList.filter((model) => {
    const q = modelQuery.trim().toLowerCase()
    if (!q) return true
    return `${model.name} ${model.key} ${model.id}`.toLowerCase().includes(q)
  })
  const filteredProjects = projects.filter((project) => {
    const query = projectQuery.trim().toLowerCase()
    return !query || `${project.name} ${project.path}`.toLowerCase().includes(query)
  })

  return (
    <div className="composer-wrap">
      {projectName && (
        <div className="composer-context">
          <div className="composer-project-control" ref={projectPickerRef}>
            <button
              className={`composer-context-item ${showProjectPicker ? 'active' : ''}`}
              type="button"
              onClick={() => setShowProjectPicker((open) => !open)}
              title="Switch project"
            >
              <FolderContextIcon />
              <span>{projectName}</span>
            </button>
            {showProjectPicker && (
              <div className="composer-project-menu">
                <label className="project-menu-search">
                  <SearchContextIcon />
                  <input
                    autoFocus
                    value={projectQuery}
                    onChange={(event) => setProjectQuery(event.target.value)}
                    placeholder="Search projects"
                    aria-label="Search projects"
                  />
                </label>
                <div className="project-menu-list">
                  {filteredProjects.map((project) => (
                    <button
                      className={`project-menu-row ${project.id === activeProjectId ? 'selected' : ''}`}
                      key={project.id}
                      type="button"
                      onClick={() => {
                        onSelectProject?.(project.id)
                        setShowProjectPicker(false)
                        setProjectQuery('')
                      }}
                    >
                      <FolderContextIcon />
                      <span>{project.name}</span>
                      {project.id === activeProjectId && <CheckIcon />}
                    </button>
                  ))}
                  {filteredProjects.length === 0 && <div className="project-menu-empty">No projects match</div>}
                </div>
                <div className="project-menu-divider" />
                <button
                  className="project-menu-action"
                  type="button"
                  onClick={() => {
                    setShowProjectPicker(false)
                    setProjectQuery('')
                    onNewProject?.()
                  }}
                >
                  <PlusContextIcon />
                  <span>New project</span>
                </button>
              </div>
            )}
          </div>
          {branch && (
            <button className="composer-context-item" type="button" onClick={onBranchClick} title="Open Git inspector">
              <BranchContextIcon />
              <span>{branch}</span>
            </button>
          )}
        </div>
      )}
      {banner}
      <div className={`composer-card ${busy ? 'busy' : ''}`}>
        {/* Command palette */}
        {showCmds && (
          <div className="cmd-pop">
            {filtered.length === 0 && <div className="cmd-empty">No commands match</div>}
            {filtered.map((c, i) => (
              <button
                key={c.name}
                type="button"
                className={`cmd-item ${i === cmdIndex ? 'active' : ''}`}
                onMouseEnter={() => setCmdIndex(i)}
                onMouseDown={(e) => e.preventDefault()}
                onClick={() => {
                  if (slashOpensOnPick(c.name)) runSlash(`/${c.name}`)
                  else insertCommand(c.name)
                }}
              >
                <span className="cmd-item-icon"><CommandGlyph name={c.name} /></span>
                <span className="cmd-name">{displayCommandName(c.name)}</span>
                {c.description && <span className="cmd-desc">{c.description}</span>}
              </button>
            ))}
          </div>
        )}

        {/* Image previews */}
        {images.length > 0 && (
          <div className="img-row">
            {images.map((img, i) => (
              <span key={i} className="img-chip">
                <img src={`data:${img.mimeType};base64,${img.data}`} alt={`Attached image ${i + 1}`} />
                <button
                  className="img-chip-del"
                  onClick={() => setImages((p) => p.filter((_, j) => j !== i))}
                  aria-label="Remove image"
                >
                  ×
                </button>
              </span>
            ))}
          </div>
        )}

        {/* Textarea */}
        <textarea
          ref={taRef}
          className="composer-textarea"
          rows={1}
          value={text}
          placeholder={busy ? 'Working… send to steer' : 'Do anything'}
          onChange={(e) => {
            const v = e.target.value
            setText(v)
            setShowCmds(v.startsWith('/'))
            setCmdFilter(v.startsWith('/') ? v.slice(1) : '')
          }}
          onKeyDown={onKeyDown}
          onPaste={(e) => {
            const items = e.clipboardData?.items
            if (!items) return
            for (const item of Array.from(items)) {
              if (item.type.startsWith('image/')) {
                const file = item.getAsFile()
                if (!file) continue
                const reader = new FileReader()
                reader.onload = (ev) => {
                  const data = (ev.target?.result as string)?.split(',')[1]
                  if (data) setImages((p) => [...p, { type: 'image', data, mimeType: item.type }])
                }
                reader.readAsDataURL(file)
                e.preventDefault()
              }
            }
          }}
        />

        {/* Toolbar: + | access | spacer | model-pill | mic | send */}
        <div className="composer-toolbar">
          {/* + button */}
          <input
            ref={fileRef}
            type="file"
            accept="image/*"
            style={{ display: 'none' }}
            onChange={(e) => handleFileAdd(e.target.files)}
          />
          <button
            className="toolbar-icon-btn add-btn"
            title="Attach image"
            onClick={() => fileRef.current?.click()}
          >
            <svg viewBox="0 0 24 24" width="15" height="15" fill="none" stroke="currentColor" strokeWidth="2">
              <path d="M12 5v14M5 12h14" />
            </svg>
          </button>

          {/* Access mode badge */}
          <AccessPicker mode={accessMode} onChange={onAccessModeChange ?? (() => {})} />

          {/* Spacer */}
          <div className="toolbar-spacer" />

          {/* Model + Effort pill */}
          <div className="model-select-wrapper" ref={pickerRef}>
            <button
              className="model-pill"
              onClick={() => { setShowModelPicker((v) => !v); setPickerView('main') }}
              title="Model & effort"
            >
              <span className="model-pill-text">{pillLabel}</span>
              <svg viewBox="0 0 24 24" width="10" height="10" fill="none" stroke="currentColor" strokeWidth="2.5">
                <path d="M6 9l6 6 6-6" />
              </svg>
            </button>

            {showModelPicker && (
              <div className="model-popover">
                {pickerView === 'main' && (
                  <div className="popover-main">
                    <button
                      className="popover-row"
                      onClick={() => setPickerView('models')}
                    >
                      <span className="popover-row-label">Model</span>
                      <div className="popover-row-val">
                        <span>{modelShort}</span>
                        <ChevronRightIcon />
                      </div>
                    </button>
                    <button
                      className="popover-row"
                      onClick={() => setPickerView('effort')}
                    >
                      <span className="popover-row-label">Effort</span>
                      <div className="popover-row-val">
                        <span>{effortLevel}</span>
                        <ChevronRightIcon />
                      </div>
                    </button>
                    <button
                      className="popover-row"
                      onClick={() => setPickerView('depth')}
                    >
                      <span className="popover-row-label">Depth</span>
                      <div className="popover-row-val">
                        <span>{rlmMaxDepth}</span>
                        <ChevronRightIcon />
                      </div>
                    </button>
                    <button className="popover-row" disabled>
                      <span className="popover-row-label">Speed</span>
                      <div className="popover-row-val">
                        <span className="muted">Standard</span>
                        <ChevronRightIcon />
                      </div>
                    </button>
                  </div>
                )}

                {pickerView === 'models' && (
                  <div className="popover-submenu">
                    <button className="popover-back" onClick={() => setPickerView('main')}>
                      <ChevronLeftIcon />
                      <span>Model</span>
                    </button>
                    <div className="model-search-wrap">
                      <svg viewBox="0 0 24 24" width="13" height="13" fill="none" stroke="currentColor" strokeWidth="1.8"><circle cx="11" cy="11" r="8" /><path d="M21 21l-4.35-4.35" /></svg>
                      <input autoFocus value={modelQuery} onChange={(e) => setModelQuery(e.target.value)} placeholder="Search models" aria-label="Search models" />
                    </div>
                    <div className="model-options-scroll">
                      {filteredModels.length === 0 && <div className="model-empty">{modelList.length ? 'No models match' : 'No models available'}</div>}
                      {filteredModels.map((m) => (
                        <button
                          key={m.key}
                          className={`popover-option ${m.key === activeModelRaw || m.id === activeModelRaw ? 'selected' : ''}`}
                          onClick={() => {
                            setChosenModel(m.key)
                            onSelectModel?.(m.key)
                            setShowModelPicker(false)
                            setPickerView('main')
                            setModelQuery('')
                          }}
                        >
                        <span className="popover-model-label">
                          <span>{m.name || shortModelName(m.key)}</span>
                          {(m.provider || m.key.includes('/')) && (
                            <span className="popover-model-provider">{m.provider || m.key.split('/')[0]}</span>
                          )}
                        </span>
                          {(m.key === activeModelRaw || m.id === activeModelRaw) && <CheckIcon />}
                        </button>
                      ))}
                    </div>
                  </div>
                )}

                {pickerView === 'effort' && (
                  <div className="popover-submenu">
                    <button className="popover-back" onClick={() => setPickerView('main')}>
                      <ChevronLeftIcon />
                      <span>Effort</span>
                    </button>
                    {EFFORT_LEVELS.map((lvl) => (
                      <button
                        key={lvl}
                        className={`popover-option ${lvl === effortLevel ? 'selected' : ''}`}
                        onClick={() => {
                          onSelectEffort?.(lvl)
                          setShowModelPicker(false)
                          setPickerView('main')
                        }}
                      >
                        <span>{lvl}</span>
                        {lvl === effortLevel && <CheckIcon />}
                      </button>
                    ))}
                    <div className="popover-note">Consumes usage limits faster at higher levels</div>
                  </div>
                )}

                {pickerView === 'depth' && (
                  <div className="popover-submenu depth-pane">
                    <button className="popover-back" onClick={() => setPickerView('main')}>
                      <ChevronLeftIcon />
                      <span>Depth</span>
                    </button>
                    <div className="depth-pane-body">
                      <DepthSlider value={rlmMaxDepth} onChange={(n) => onDepthChange?.(n)} />
                    </div>
                  </div>
                )}
              </div>
            )}
          </div>

          {/* Mic button */}
          <button className="toolbar-icon-btn mic-btn" title="Voice input" disabled>
            <MicIcon />
          </button>

          {/* Send / Abort */}
          {busy ? (
            <button className="send-btn abort" onClick={onAbort} title="Stop">
              <span className="stop-square" />
            </button>
          ) : (
            <button
              className={`send-btn ${text.trim() || images.length ? 'active' : ''}`}
              onClick={submit}
              disabled={!text.trim() && images.length === 0}
              title="Send"
            >
              <svg viewBox="0 0 24 24" width="14" height="14" fill="currentColor">
                <path d="M12 19V5M5 12l7-7 7 7" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round" strokeLinejoin="round" fill="none" />
              </svg>
            </button>
          )}
        </div>
      </div>
    </div>
  )
}

/* ─── Icons ──────────────────────────────────────────────────── */

function FolderContextIcon() {
  return (
    <svg viewBox="0 0 24 24" aria-hidden="true">
      <path d="M3 7.5A2.5 2.5 0 015.5 5H10l2 2h6.5A2.5 2.5 0 0121 9.5v7A2.5 2.5 0 0118.5 19h-13A2.5 2.5 0 013 16.5v-9z" />
    </svg>
  )
}

function SearchContextIcon() {
  return (
    <svg viewBox="0 0 24 24" aria-hidden="true">
      <circle cx="11" cy="11" r="7" />
      <path d="M20 20l-4-4" />
    </svg>
  )
}

function PlusContextIcon() {
  return (
    <svg viewBox="0 0 24 24" aria-hidden="true">
      <path d="M12 4v16M4 12h16" />
    </svg>
  )
}

function BranchContextIcon() {
  return (
    <svg viewBox="0 0 24 24" aria-hidden="true">
      <circle cx="6" cy="5" r="2" />
      <circle cx="18" cy="7" r="2" />
      <circle cx="6" cy="19" r="2" />
      <path d="M6 7v10M8 12h3a7 7 0 007-3" />
    </svg>
  )
}

function ChevronRightIcon() {
  return (
    <svg viewBox="0 0 24 24" width="12" height="12" fill="none" stroke="currentColor" strokeWidth="2.2">
      <path d="M9 18l6-6-6-6" />
    </svg>
  )
}

function ChevronLeftIcon() {
  return (
    <svg viewBox="0 0 24 24" width="12" height="12" fill="none" stroke="currentColor" strokeWidth="2.2">
      <path d="M15 18l-6-6 6-6" />
    </svg>
  )
}

function CheckIcon() {
  return (
    <svg viewBox="0 0 24 24" width="14" height="14" fill="none" stroke="currentColor" strokeWidth="2.5">
      <polyline points="20 6 9 17 4 12" />
    </svg>
  )
}

function MicIcon() {
  return (
    <svg viewBox="0 0 24 24" width="15" height="15" fill="none" stroke="currentColor" strokeWidth="1.8">
      <rect x="9" y="2" width="6" height="11" rx="3" />
      <path d="M5 10a7 7 0 0014 0M12 19v3M8 22h8" />
    </svg>
  )
}

function displayCommandName(name: string): string {
  return name.replace(/-/g, ' ').replace(/\b\w/g, (ch) => ch.toUpperCase())
}

function CommandGlyph({ name }: { name: string }): JSX.Element {
  const n = name.toLowerCase()
  let inner: JSX.Element
  if (n === 'compact') inner = <path d="M4 7h16M8 12h8M10 17h4" />
  else if (n === 'new' || n === 'clear') inner = <path d="M12 5v14M5 12h14" />
  else if (n === 'model') inner = <><circle cx="12" cy="12" r="3" /><path d="M12 3v2M12 19v2M3 12h2M19 12h2M5.6 5.6l1.4 1.4M17 17l1.4 1.4M5.6 18.4L7 17M17 7l1.4-1.4" /></>
  else if (n === 'effort' || n === 'thinking') inner = <path d="M12 3a6 6 0 00-4 10c.6.6 1 1.4 1 2.2V17h6v-1.8c0-.8.4-1.6 1-2.2A6 6 0 0012 3zM10 21h4" />
  else if (n === 'goal') inner = <><circle cx="12" cy="12" r="8" /><circle cx="12" cy="12" r="3" /></>
  else if (n === 'login' || n === 'logout') inner = <path d="M10 17l5-5-5-5M15 12H3M15 3h4a2 2 0 012 2v14a2 2 0 01-2 2h-4" />
  else if (n === 'fork' || n === 'tree' || n === 'clone') inner = <path d="M6 3v12M6 15a3 3 0 100 6 3 3 0 000-6zM18 3a3 3 0 100 6 3 3 0 000-6zM18 21a3 3 0 100-6 3 3 0 000 6zM6 9h6a6 6 0 016 6" />
  else if (n === 'settings') inner = <><circle cx="12" cy="12" r="3" /><path d="M19.4 15a1.7 1.7 0 00.34 1.88l.06.06-2.83 2.83-.06-.06A1.7 1.7 0 0015 19.4a1.7 1.7 0 00-1 .6V21h-4v-1a1.7 1.7 0 00-1-.6 1.7 1.7 0 00-1.88.34l-.06.06-2.83-2.83.06-.06A1.7 1.7 0 004.6 15a1.7 1.7 0 00-.6-1H3v-4h1a1.7 1.7 0 00.6-1 1.7 1.7 0 00-.34-1.88l-.06-.06 2.83-2.83.06.06A1.7 1.7 0 009 4.6a1.7 1.7 0 001-.6V3h4v1a1.7 1.7 0 001 .6 1.7 1.7 0 001.88-.34l.06-.06 2.83 2.83-.06.06A1.7 1.7 0 0019.4 9a1.7 1.7 0 00.6 1H21v4h-1a1.7 1.7 0 00-.6 1z" /></>
  else if (n === 'share' || n === 'export') inner = <path d="M4 12v7a1 1 0 001 1h14a1 1 0 001-1v-7M16 6l-4-4-4 4M12 2v14" />
  else if (n === 'mcp') inner = <path d="M8 10V7a4 4 0 018 0v3M6 10h12v10H6z" />
  else if (n === 'heartbeat' || n === 'heartbeats') inner = <path d="M3 12h4l2-5 4 10 2-5h6" />
  else if (n === 'rlm-max-depth') inner = <><circle cx="12" cy="12" r="3" /><circle cx="12" cy="12" r="7" /></>
  else if (n === 'resume') inner = <path d="M8 5v14l11-7z" />
  else if (n === 'copy') inner = <><rect x="9" y="9" width="11" height="11" rx="2" /><path d="M5 15V5a2 2 0 012-2h10" /></>
  else if (n === 'quit') inner = <path d="M9 21H5a2 2 0 01-2-2V5a2 2 0 012-2h4M16 17l5-5-5-5M21 12H9" />
  else inner = <circle cx="12" cy="12" r="3.5" />
  return (
    <svg viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" strokeWidth="1.7" strokeLinecap="round" strokeLinejoin="round">
      {inner}
    </svg>
  )
}

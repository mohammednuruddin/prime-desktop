import { useEffect, useRef, useState } from 'react'

export type AccessMode = 'ask' | 'approve-me' | 'full' | 'custom'

interface Props {
  mode: AccessMode
  onChange: (mode: AccessMode) => void
}

const MODES: { id: AccessMode; label: string; desc: string; danger?: boolean }[] = [
  { id: 'ask', label: 'Ask for approval', desc: 'Always ask to edit external files and use the internet' },
  { id: 'approve-me', label: 'Approve for me', desc: 'Only ask for actions detected as potentially unsafe' },
  { id: 'full', label: 'Full access', desc: 'Unrestricted access to the internet and any file on your computer', danger: true },
  { id: 'custom', label: 'Custom (config.toml)', desc: 'Uses permissions defined in config.toml' }
]

export default function AccessPicker({ mode, onChange }: Props): JSX.Element {
  const [open, setOpen] = useState(false)
  const ref = useRef<HTMLDivElement>(null)
  const current = MODES.find((item) => item.id === mode) ?? MODES[0]

  useEffect(() => {
    const outside = (event: MouseEvent) => {
      if (ref.current && !ref.current.contains(event.target as Node)) setOpen(false)
    }
    if (open) document.addEventListener('mousedown', outside)
    return () => document.removeEventListener('mousedown', outside)
  }, [open])

  return (
    <div className="access-picker" ref={ref}>
      <button className={`access-badge ${mode}`} onClick={() => setOpen((value) => !value)} title="Action approval mode">
        <ModeIcon mode={mode} />
        <span>{current.label}</span>
        <ChevronIcon />
      </button>

      {open && (
        <div className="access-menu">
          <div className="access-menu-title">How should agent actions be approved?</div>
          {MODES.map((item) => (
            <button
              key={item.id}
              className={`access-option ${item.id === mode ? 'selected' : ''} ${item.danger ? 'danger' : ''}`}
              onClick={() => { onChange(item.id); setOpen(false) }}
            >
              <span className="access-opt-icon"><ModeIcon mode={item.id} /></span>
              <span className="access-opt-body">
                <span className="access-opt-label">{item.label}</span>
                <span className="access-opt-desc">{item.desc}</span>
              </span>
              {item.id === mode && <CheckIcon />}
            </button>
          ))}
        </div>
      )}
    </div>
  )
}

function ModeIcon({ mode }: { mode: AccessMode }): JSX.Element {
  if (mode === 'ask') return <svg viewBox="0 0 24 24"><path d="M8 11V5.5a1.5 1.5 0 013 0V10m0-4.5a1.5 1.5 0 013 0V10m0-3.5a1.5 1.5 0 013 0V12m0-3.5a1.5 1.5 0 013 0v5.25C20 18.2 17.2 21 13.5 21h-1.2a6.4 6.4 0 01-5.1-2.55L4.4 14.7a1.6 1.6 0 012.45-2.05L8 14" /></svg>
  if (mode === 'approve-me') return <svg viewBox="0 0 24 24"><path d="M12 3l7 3v5c0 4.6-2.8 8.2-7 10-4.2-1.8-7-5.4-7-10V6l7-3z" /><path d="M9.5 12l1.7 1.7 3.6-4" /></svg>
  if (mode === 'full') return <svg viewBox="0 0 24 24"><path d="M12 3l7 3v5c0 4.6-2.8 8.2-7 10-4.2-1.8-7-5.4-7-10V6l7-3z" /><path d="M12 8v5m0 3h.01" /></svg>
  return <svg viewBox="0 0 24 24"><circle cx="12" cy="12" r="3" /><path d="M19.4 15a1.7 1.7 0 00.34 1.88l.06.06-2.83 2.83-.06-.06A1.7 1.7 0 0015 19.4a1.7 1.7 0 00-1 .6V20h-4v-.09a1.7 1.7 0 00-1-.51 1.7 1.7 0 00-1.88.34l-.06.06-2.83-2.83.06-.06A1.7 1.7 0 004.6 15a1.7 1.7 0 00-.6-1H4v-4h.09a1.7 1.7 0 00.51-1 1.7 1.7 0 00-.34-1.88l-.06-.06 2.83-2.83.06.06A1.7 1.7 0 009 4.6a1.7 1.7 0 001-.6V4h4v.09a1.7 1.7 0 001 .51 1.7 1.7 0 001.88-.34l.06-.06 2.83 2.83-.06.06A1.7 1.7 0 0019.4 9a1.7 1.7 0 00.6 1H20v4h-.09a1.7 1.7 0 00-.51 1z" /></svg>
}

function ChevronIcon(): JSX.Element {
  return <svg className="access-chevron" viewBox="0 0 24 24"><path d="M7 9l5 5 5-5" /></svg>
}

function CheckIcon(): JSX.Element {
  return <svg className="access-check" viewBox="0 0 24 24"><path d="M20 6L9 17l-5-5" /></svg>
}

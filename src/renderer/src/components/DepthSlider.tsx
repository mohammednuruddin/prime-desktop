import { useEffect, useRef, useState } from 'react'

interface Props {
  value: number
  onChange: (value: number) => void
  disabled?: boolean
}

const LABELS = ['Off', 'One hop', 'Shallow', 'Deep'] as const

function depthLabel(value: number): string {
  if (value === 0) return 'No recursion'
  if (value === 1) return 'One hop'
  if (value <= 3) return 'Shallow'
  if (value <= 6) return 'Layered'
  return 'Deep recursion'
}

export default function DepthSlider({ value, onChange, disabled }: Props): JSX.Element {
  const [live, setLive] = useState(value)
  const [bump, setBump] = useState(0)
  const prev = useRef(value)

  useEffect(() => {
    setLive(value)
  }, [value])

  useEffect(() => {
    if (prev.current !== live) {
      prev.current = live
      setBump((n) => n + 1)
    }
  }, [live])

  const pct = (live / 10) * 100

  return (
    <div className={`depth-slider ${disabled ? 'disabled' : ''}`}>
      <div className="depth-slider-top">
        <div className="depth-rings" aria-hidden="true">
          {Array.from({ length: 5 }, (_, i) => {
            const lit = live >= (i + 1) * 2 || (i === 0 && live >= 1)
            return (
              <span
                key={i}
                className={`depth-ring ${lit ? 'lit' : ''}`}
                style={{
                  inset: `${i * 4}px`,
                  transitionDelay: `${i * 28}ms`
                }}
              />
            )
          })}
        </div>
        <div className="depth-readout">
          <span key={bump} className="depth-num">
            {live}
          </span>
          <span className="depth-caption">{depthLabel(live)}</span>
        </div>
      </div>

      <div className="depth-track-wrap">
        <input
          className="depth-range"
          type="range"
          min={0}
          max={10}
          step={1}
          value={live}
          disabled={disabled}
          aria-label="Recursive subagent max depth"
          onChange={(e) => {
            const next = Number(e.target.value)
            setLive(next)
            onChange(next)
          }}
        />
        <div className="depth-track" style={{ ['--depth-pct' as string]: `${pct}%` }}>
          <div className="depth-fill" />
          <div className="depth-thumb" />
        </div>
        <div className="depth-ticks">
          {Array.from({ length: 11 }, (_, i) => (
            <button
              key={i}
              type="button"
              className={`depth-tick ${i === live ? 'active' : ''} ${i < live ? 'passed' : ''}`}
              disabled={disabled}
              onClick={() => {
                setLive(i)
                onChange(i)
              }}
              aria-label={`Depth ${i}`}
            >
              {i === 0 || i === 5 || i === 10 ? i : ''}
            </button>
          ))}
        </div>
      </div>

      <div className="depth-legend">
        {LABELS.map((label) => (
          <span key={label}>{label}</span>
        ))}
      </div>
    </div>
  )
}

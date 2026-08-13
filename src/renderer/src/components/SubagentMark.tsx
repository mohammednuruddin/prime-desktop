const COLORS = [
  '#e76f98',
  '#826fd5',
  '#d9972f',
  '#309a7c',
  '#db6858',
  '#4f82cf',
  '#7f9f3d',
  '#bd64b9'
]

function hashSeed(seed: string): number {
  let hash = 2166136261
  for (let index = 0; index < seed.length; index++) {
    hash ^= seed.charCodeAt(index)
    hash = Math.imul(hash, 16777619)
  }
  return hash >>> 0
}

export default function SubagentMark({ seed }: { seed: string }): JSX.Element {
  const hash = hashSeed(seed || 'subagent')
  const shape = hash % 8
  const color = COLORS[(hash >>> 4) % COLORS.length]

  return (
    <span className="subagent-mark" style={{ color }} aria-hidden="true">
      <svg viewBox="0 0 24 24">
        {shape === 0 && (
          <>
            <circle cx="12" cy="5.5" r="3.5" />
            <circle cx="18.5" cy="12" r="3.5" />
            <circle cx="12" cy="18.5" r="3.5" />
            <circle cx="5.5" cy="12" r="3.5" />
          </>
        )}
        {shape === 1 && <path d="M12 2.5l2.35 6.1L21 12l-6.65 3.4L12 21.5l-2.35-6.1L3 12l6.65-3.4L12 2.5z" />}
        {shape === 2 && (
          <>
            <path d="M12 2.8l7.9 4.55v9.3L12 21.2l-7.9-4.55v-9.3L12 2.8z" />
            <circle className="subagent-mark-cutout" cx="12" cy="12" r="3" />
          </>
        )}
        {shape === 3 && (
          <>
            <rect x="9" y="2.5" width="6" height="9.5" rx="3" />
            <rect x="12" y="9" width="9.5" height="6" rx="3" />
            <rect x="9" y="12" width="6" height="9.5" rx="3" />
            <rect x="2.5" y="9" width="9.5" height="6" rx="3" />
          </>
        )}
        {shape === 4 && (
          <>
            <path d="M12 2.8l5.1 8.8H6.9L12 2.8z" />
            <path d="M6.5 12.4l5.1 8.8H1.4l5.1-8.8z" />
            <path d="M17.5 12.4l5.1 8.8H12.4l5.1-8.8z" />
          </>
        )}
        {shape === 5 && (
          <>
            <circle cx="12" cy="12" r="4" />
            <path className="subagent-mark-stroke" d="M3.3 14.8c2.5 3.4 8.2 4.4 12.8 2.1s6.2-7 3.6-9.6S11.3 3.5 6.9 6 1 12 3.3 14.8z" />
          </>
        )}
        {shape === 6 && (
          <>
            <rect x="3" y="3" width="8" height="8" rx="2.4" />
            <rect x="13" y="3" width="8" height="8" rx="2.4" />
            <rect x="3" y="13" width="8" height="8" rx="2.4" />
            <rect x="13" y="13" width="8" height="8" rx="2.4" />
          </>
        )}
        {shape === 7 && (
          <>
            <path d="M3 7l4-4 4 4-4 4-4-4z" />
            <path d="M13 7l4-4 4 4-4 4-4-4z" />
            <path d="M8 17l4-4 4 4-4 4-4-4z" />
          </>
        )}
      </svg>
    </span>
  )
}

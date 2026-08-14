const CELLS = [0, 1, 2, 3, 4, 5, 6, 7, 8]

export default function WorkingMark({ label = 'Working', className = '' }: { label?: string; className?: string }): JSX.Element {
  return (
    <span className={['working-mark', className].filter(Boolean).join(' ')} role="img" aria-label={label} title={label}>
      {CELLS.map((cell) => {
        return <i key={cell} />
      })}
    </span>
  )
}

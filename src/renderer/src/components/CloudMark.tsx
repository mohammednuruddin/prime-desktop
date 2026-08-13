interface Props {
  size?: number
  className?: string
}

export default function CloudMark({ size = 22, className }: Props): JSX.Element {
  const height = Math.round(size * (54 / 64))
  return (
    <svg
      className={className}
      viewBox="0 0 64 54"
      width={size}
      height={height}
      fill="none"
      stroke="currentColor"
      strokeWidth="2"
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden
    >
      <path d="M18 42C10 42 4 36 4 28C4 21 9 16 16 15.5C18 9 24 4 32 4C41 4 48 10 49.5 18C56 19 60 24 60 31C60 38 54 42 46 42Z" />
      <path d="M26 27L23 30L26 33" />
      <line x1="30" y1="33" x2="34" y2="27" />
      <path d="M38 27L41 30L38 33" />
    </svg>
  )
}

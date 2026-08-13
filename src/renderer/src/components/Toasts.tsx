interface Toast {
  id: string
  kind: 'info' | 'success' | 'warning' | 'error'
  text: string
}

export default function Toasts({ toasts }: { toasts: Toast[] }): JSX.Element {
  return (
    <div className="toasts">
      {toasts.map((t) => (
        <div key={t.id} className={`toast ${t.kind}`}>
          {t.text}
        </div>
      ))}
    </div>
  )
}

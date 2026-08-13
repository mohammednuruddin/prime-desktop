import { useState } from 'react'
import type { UiDialog } from '@shared/types'

interface Props {
  dialogs: UiDialog[]
  onRespond: (id: string, value: unknown, cancelled: boolean) => void
}

export default function DialogHost({ dialogs, onRespond }: Props): JSX.Element {
  const [inputValue, setInputValue] = useState('')
  const dialog = dialogs[0]
  if (!dialog) return <></>

  return (
    <div className="dialog-backdrop">
      <div className="dialog">
        <div className="dialog-title">{dialog.title}</div>
        {dialog.method === 'confirm' && dialog.message && <div className="dialog-message">{dialog.message}</div>}
        {dialog.method === 'select' && (
          <div className="dialog-options">
            {dialog.options?.map((opt) => (
              <button key={opt} className="dialog-option" onClick={() => onRespond(dialog.id, opt, false)}>
                {opt}
              </button>
            ))}
          </div>
        )}
        {(dialog.method === 'input' || dialog.method === 'editor') && (
          <>
            <textarea
              autoFocus
              className="dialog-input"
              rows={dialog.method === 'editor' ? 8 : 2}
              placeholder={dialog.prefill ?? ''}
              defaultValue={dialog.prefill ?? ''}
              onChange={(e) => setInputValue(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === 'Enter' && !e.shiftKey) {
                  e.preventDefault()
                  onRespond(dialog.id, inputValue, false)
                }
                if (e.key === 'Escape') onRespond(dialog.id, null, true)
              }}
            />
            <div className="dialog-actions">
              <button className="btn ghost" onClick={() => onRespond(dialog.id, null, true)}>
                Cancel
              </button>
              <button className="btn primary" onClick={() => onRespond(dialog.id, inputValue, false)}>
                OK
              </button>
            </div>
          </>
        )}
        {dialog.method === 'confirm' && (
          <div className="dialog-actions">
            <button className="btn ghost" onClick={() => onRespond(dialog.id, null, true)}>
              Cancel
            </button>
            <button className="btn danger" onClick={() => onRespond(dialog.id, true, false)}>
              Confirm
            </button>
          </div>
        )}
      </div>
    </div>
  )
}

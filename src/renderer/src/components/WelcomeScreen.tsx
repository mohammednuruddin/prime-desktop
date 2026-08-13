import CloudMark from './CloudMark'
import type { BinaryState } from '@shared/types'

interface Props {
  onOpen: () => void
  binary: BinaryState | null
  onInstall: () => void
}

export default function WelcomeScreen({ onOpen, binary, onInstall }: Props): JSX.Element {
  const installing = binary?.status === 'installing'
  const missing = binary?.status === 'error'

  return (
    <div className="codex-chat-layout">
      <div className="codex-messages-area">
        <div className="codex-welcome-center">
          <div className="codex-cloud-icon">
            <CloudMark size={56} />
          </div>
          <h1 className="codex-center-title">What should we work on?</h1>
          <p className="welcome-lead">Open a project folder to start a thread. Prime Agent will work in that directory.</p>
          <div className="welcome-actions">
            <button className="welcome-open-btn" onClick={onOpen}>
              Open a project
            </button>
            {missing && (
              <button className="welcome-secondary-btn" onClick={onInstall} disabled={installing}>
                {installing ? 'Installing Prime Agent' : 'Install Prime Agent'}
              </button>
            )}
          </div>
          {binary?.error && <p className="welcome-err">{binary.error}</p>}
        </div>
      </div>
    </div>
  )
}

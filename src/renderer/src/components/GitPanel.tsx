import { useCallback, useEffect, useState } from 'react'
import type { GitChange, GitStatus } from '@shared/types'

const EMPTY_STATUS: GitStatus = {
  isRepo: true,
  branch: null,
  upstream: null,
  ahead: 0,
  behind: 0,
  changes: []
}

function DiffView({ diff }: { diff: string }): JSX.Element {
  if (!diff.trim()) return <div className="git-diff-empty">No textual diff available.</div>
  return (
    <div className="git-diff">
      {diff.split('\n').map((line, index) => {
        const className = line.startsWith('+') && !line.startsWith('+++')
          ? 'add'
          : line.startsWith('-') && !line.startsWith('---')
            ? 'del'
            : line.startsWith('@@')
              ? 'hunk'
              : ''
        return <div className={`git-diff-line ${className}`} key={index}>{line || ' '}</div>
      })}
    </div>
  )
}

function changeLabel(change: GitChange, staged: boolean): string {
  const code = staged ? change.indexStatus : (change.indexStatus === '?' ? '?' : change.worktreeStatus)
  if (code === 'A' || code === '?') return 'A'
  if (code === 'D') return 'D'
  if (code === 'R') return 'R'
  if (code === 'C') return 'C'
  if (code === 'U') return 'U'
  return 'M'
}

interface Selection {
  path: string
  staged: boolean
}

export default function GitPanel({ agentId }: { agentId: string | null }): JSX.Element {
  const [status, setStatus] = useState<GitStatus>(EMPTY_STATUS)
  const [selection, setSelection] = useState<Selection | null>(null)
  const [diff, setDiff] = useState('')
  const [loading, setLoading] = useState(true)
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState('')
  const [message, setMessage] = useState('')

  const refresh = useCallback(async () => {
    if (!agentId) return
    try {
      const next = await window.prime.gitStatus(agentId) as GitStatus
      setStatus(next)
      setError('')
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : String(reason))
    } finally {
      setLoading(false)
    }
  }, [agentId])

  useEffect(() => {
    setLoading(true)
    setSelection(null)
    setDiff('')
    void refresh()
    const timer = window.setInterval(() => void refresh(), 5000)
    const off = window.prime.onEvent((raw) => {
      const event = raw as { agentId?: string; type?: string }
      if (event.agentId === agentId && (event.type === 'turn_end' || event.type === 'session_replaced')) void refresh()
    })
    return () => {
      window.clearInterval(timer)
      off()
    }
  }, [agentId, refresh])

  useEffect(() => {
    if (!agentId || !selection) {
      setDiff('')
      return
    }
    let active = true
    void window.prime.gitFileDiff(agentId, selection.path, selection.staged)
      .then((value: string) => { if (active) setDiff(value) })
      .catch((reason: unknown) => { if (active) setError(reason instanceof Error ? reason.message : String(reason)) })
    return () => { active = false }
  }, [agentId, selection])

  const run = async (action: () => Promise<unknown>, after?: () => void) => {
    setBusy(true)
    setError('')
    try {
      const result = await action() as GitStatus
      setStatus(result)
      after?.()
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : String(reason))
    } finally {
      setBusy(false)
    }
  }

  const commit = async () => {
    if (!agentId || !message.trim()) return
    setBusy(true)
    setError('')
    try {
      const result = await window.prime.gitCommit(agentId, message) as { sha: string; status: GitStatus }
      setStatus(result.status)
      setMessage('')
      setSelection(null)
      setDiff('')
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : String(reason))
    } finally {
      setBusy(false)
    }
  }

  if (!agentId) return <div className="sp-empty"><span>No active project</span></div>
  if (loading) return <div className="sp-empty"><span>Loading source control…</span></div>
  if (!status.isRepo) return <div className="sp-empty"><span>This folder is not a Git repository.</span></div>

  const staged = status.changes.filter((change) => change.staged)
  const unstaged = status.changes.filter((change) => change.unstaged)

  return (
    <div className="git-panel">
      <div className="git-toolbar">
        <div className="git-branch" title={status.upstream ?? undefined}>
          <BranchIcon />
          <span>{status.branch ?? 'HEAD'}</span>
          {(status.ahead > 0 || status.behind > 0) && (
            <span className="git-sync-state">↑{status.ahead} ↓{status.behind}</span>
          )}
        </div>
        <button className="git-refresh" onClick={() => void refresh()} title="Refresh" aria-label="Refresh source control">↻</button>
      </div>

      <div className="git-workspace">
        <div className="git-changes">
          <ChangeGroup
            title="Staged Changes"
            changes={staged}
            staged
            selected={selection}
            disabled={busy}
            onSelect={setSelection}
            onAll={() => agentId && void run(() => window.prime.gitUnstageAll(agentId), () => {
              setSelection(null)
              setDiff('')
            })}
            onAction={(path) => agentId && void run(() => window.prime.gitUnstage(agentId, [path]), () => {
              if (selection?.path === path && selection.staged) setSelection({ path, staged: false })
            })}
          />
          <ChangeGroup
            title="Changes"
            changes={unstaged}
            staged={false}
            selected={selection}
            disabled={busy}
            onSelect={setSelection}
            onAll={() => agentId && void run(() => window.prime.gitStageAll(agentId))}
            onAction={(path) => agentId && void run(() => window.prime.gitStage(agentId, [path]), () => {
              if (selection?.path === path && !selection.staged) setSelection({ path, staged: true })
            })}
          />
          {status.changes.length === 0 && <div className="git-clean">Working tree clean</div>}
        </div>
        {selection && (
          <div className="git-preview">
            <div className="git-preview-head">
              <span>{selection.path}</span>
              <button onClick={() => setSelection(null)} aria-label="Close diff">×</button>
            </div>
            <DiffView diff={diff} />
          </div>
        )}
      </div>

      {error && <div className="git-error" role="status">{error}</div>}
      <div className="git-commit">
        <textarea
          value={message}
          onChange={(event) => setMessage(event.target.value)}
          onKeyDown={(event) => {
            if ((event.metaKey || event.ctrlKey) && event.key === 'Enter') {
              event.preventDefault()
              void commit()
            }
          }}
          placeholder={staged.length ? 'Commit message' : 'Stage changes to commit'}
          disabled={busy || staged.length === 0}
          rows={2}
        />
        <button disabled={busy || staged.length === 0 || !message.trim()} onClick={() => void commit()}>
          {busy ? 'Working…' : `Commit${staged.length ? ` (${staged.length})` : ''}`}
        </button>
      </div>
    </div>
  )
}

function ChangeGroup({
  title,
  changes,
  staged,
  selected,
  disabled,
  onSelect,
  onAll,
  onAction
}: {
  title: string
  changes: GitChange[]
  staged: boolean
  selected: Selection | null
  disabled: boolean
  onSelect: (selection: Selection) => void
  onAll: () => void
  onAction: (path: string) => void
}): JSX.Element | null {
  if (changes.length === 0) return null
  return (
    <section className="git-group">
      <div className="git-group-head">
        <span>{title}</span>
        <span className="git-group-count">{changes.length}</span>
        <button disabled={disabled} onClick={onAll}>{staged ? 'Unstage all' : 'Stage all'}</button>
      </div>
      {changes.map((change) => {
        const active = selected?.path === change.path && selected.staged === staged
        return (
          <div className={`git-change ${active ? 'active' : ''}`} key={`${staged ? 's' : 'u'}-${change.path}`}>
            <button className="git-change-main" onClick={() => onSelect({ path: change.path, staged })} title={change.path}>
              <span className={`git-change-status ${changeLabel(change, staged).toLowerCase()}`}>
                {changeLabel(change, staged)}
              </span>
              <span className="git-change-path">{change.path}</span>
            </button>
            <button
              className="git-change-action"
              disabled={disabled}
              onClick={() => onAction(change.path)}
              title={staged ? 'Unstage' : 'Stage'}
              aria-label={`${staged ? 'Unstage' : 'Stage'} ${change.path}`}
            >
              {staged ? '−' : '+'}
            </button>
          </div>
        )
      })}
    </section>
  )
}

function BranchIcon(): JSX.Element {
  return (
    <svg viewBox="0 0 24 24" aria-hidden="true">
      <circle cx="6" cy="5" r="2" />
      <circle cx="18" cy="7" r="2" />
      <circle cx="6" cy="19" r="2" />
      <path d="M6 7v10M8 12h3a7 7 0 007-3" />
    </svg>
  )
}

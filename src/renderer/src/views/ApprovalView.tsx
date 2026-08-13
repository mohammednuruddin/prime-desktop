import { useCallback, useEffect, useState } from 'react'
import type { Checkpoint, FileDiff, PermissionRule } from '@shared/types'

interface Props {
  activeAgentId: string | null
}

export default function ApprovalView({ activeAgentId }: Props): JSX.Element {
  const [checkpoints, setCheckpoints] = useState<Checkpoint[]>([])
  const [diffs, setDiffs] = useState<FileDiff[]>([])
  const [perms, setPerms] = useState<PermissionRule[]>([])
  const [selected, setSelected] = useState<FileDiff | null>(null)
  const [restoring, setRestoring] = useState<string | null>(null)

  const load = useCallback(() => {
    if (!activeAgentId) return
    void window.prime.gitList(activeAgentId).then(setCheckpoints)
    void window.prime.gitDiffFiles(activeAgentId).then((d) => {
      setDiffs(d)
      setSelected((s) => s ?? d[0] ?? null)
    })
    void window.prime.permissionsList().then(setPerms)
  }, [activeAgentId])

  useEffect(() => {
    setCheckpoints([])
    setDiffs([])
    setSelected(null)
    load()
    const timer = setInterval(load, 5000)
    return () => clearInterval(timer)
  }, [load])

  if (!activeAgentId) {
    return (
      <div className="view">
        <header className="view-header">
          <h2>Review</h2>
          <p className="view-sub">Open a project folder to inspect agent changes.</p>
        </header>
      </div>
    )
  }

  return (
    <div className="view review-page">
      <header className="view-header">
        <h2>Review</h2>
        <p className="view-sub">
          Each prompt creates a git checkpoint so you can inspect the diff and restore with one click.
        </p>
      </header>

      <div className="approval-layout">
        <div className="approval-col">
          <section className="panel">
            <div className="panel-head">Checkpoints</div>
            {checkpoints.length === 0 && <div className="cmd-empty">No checkpoints yet — send a prompt and a git checkpoint is created first.</div>}
            {checkpoints.map((c) => (
              <div key={c.id} className="checkpoint-row">
                <div className="checkpoint-main">
                  <code>{c.id}</code>
                  <span className="checkpoint-label">{c.label}</span>
                  <span className="checkpoint-time">{new Date(c.createdAt).toLocaleString()}</span>
                </div>
                <div className="checkpoint-files">{c.dirtyFiles.length} dirty files</div>
                <button
                  className="btn danger small"
                  disabled={restoring !== null}
                  onClick={async () => {
                    setRestoring(c.id)
                    await window.prime.gitRestore(activeAgentId, c.id)
                    setRestoring(null)
                    load()
                  }}
                >
                  {restoring === c.id ? 'Restoring…' : 'Restore'}
                </button>
              </div>
            ))}
          </section>

          <section className="panel">
            <div className="panel-head">Changed files</div>
            {diffs.length === 0 && <div className="cmd-empty">Working tree is clean.</div>}
            {diffs.map((d) => (
              <button
                key={d.path}
                className={`file-row ${selected?.path === d.path ? 'active' : ''}`}
                onClick={() => setSelected(d)}
              >
                <span className={`file-status ${d.status}`}>{d.status[0].toUpperCase()}</span>
                <span className="file-path">{d.path}</span>
              </button>
            ))}
          </section>
        </div>

        <div className="approval-main">
          <section className="panel grow">
            <div className="panel-head">{selected ? `${selected.status} · ${selected.path}` : 'Diff'}</div>
            {selected ? (
              selected.diff ? (
                <pre className="diff-pre">{selected.diff}</pre>
              ) : (
                <div className="cmd-empty">
                  No staged/unstaged diff for this file (may be newly added, untracked, or outside git).
                </div>
              )
            ) : (
              <div className="cmd-empty">Select a file to inspect its diff.</div>
            )}
          </section>
        </div>
      </div>

      <section className="panel">
        <div className="panel-head">Permission rules</div>
        <div className="row-gap">
          <input id="perm-pattern" className="field" placeholder="command pattern, e.g. rm -rf or *" />
          <select id="perm-action" className="field">
            <option value="deny">deny</option>
            <option value="allow">allow</option>
          </select>
          <select id="perm-scope" className="field">
            <option value="global">global</option>
            <option value="project">this project</option>
          </select>
          <button
            className="btn primary small"
            onClick={() => {
              const pattern = (document.getElementById('perm-pattern') as HTMLInputElement).value.trim()
              if (!pattern) return
              const action = (document.getElementById('perm-action') as HTMLSelectElement).value as 'allow' | 'deny'
              const scope = (document.getElementById('perm-scope') as HTMLSelectElement).value as 'global' | 'project'
              void window.prime.permissionsSet(pattern, action, scope, scope === 'project' ? activeAgentId : undefined).then(() =>
                void window.prime.permissionsList().then(setPerms)
              )
              ;(document.getElementById('perm-pattern') as HTMLInputElement).value = ''
            }}
          >
            Add rule
          </button>
        </div>
        {perms.length === 0 && <div className="cmd-empty">No rules — all commands are asked before running (via extension dialogs).</div>}
        {perms.map((p, i) => (
          <div key={i} className="perm-row">
            <code>{p.pattern}</code>
            <span className={`perm-action ${p.action}`}>{p.action}</span>
            <span className="perm-scope">{p.scope}{p.scope === 'project' ? ' · ' + (p.projectPath ?? '') : ''}</span>
            <button
              className="btn ghost small"
              onClick={() => {
                void window.prime.permissionsRemove(i).then(() => void window.prime.permissionsList().then(setPerms))
              }}
            >
              remove
            </button>
          </div>
        ))}
      </section>
    </div>
  )
}

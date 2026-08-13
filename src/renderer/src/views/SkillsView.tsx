import { useEffect, useState } from 'react'
import type { SkillInfo } from '@shared/types'

interface Props {
  activeAgentId: string | null
}

export default function SkillsView({ activeAgentId }: Props): JSX.Element {
  const [skills, setSkills] = useState<SkillInfo[]>([])
  const [pkgSource, setPkgSource] = useState('')
  const [result, setResult] = useState<string | null>(null)
  const [query, setQuery] = useState('')

  useEffect(() => {
    if (!activeAgentId) return
    void window.prime.skillsList(activeAgentId).then(setSkills)
  }, [activeAgentId])

  const prompts = skills.filter((s) => s.source === 'prompt')
  const skillList = skills.filter((s) => s.source === 'skill')
  const visibleSkills = skillList.filter((skill) => `${skill.name} ${skill.description}`.toLowerCase().includes(query.toLowerCase()))

  return (
    <div className="view plugins-page">
      <header className="view-header">
        <div>
          <h2>Skills</h2>
          <p className="view-sub">Reusable tools and prompts the agent can load into a thread.</p>
        </div>
      </header>

      <section className="panel plugin-install-panel">
        <div className="panel-head">Install a skill</div>
        <div className="row-gap">
          <input
            className="field grow"
            placeholder="Package name, Git URL, or local path"
            value={pkgSource}
            onChange={(e) => setPkgSource(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === 'Enter' && pkgSource.trim()) {
                void window.prime.skillsInstall(pkgSource.trim()).then((r) => {
                  const res = r as { ok: boolean; error?: string }
                  setResult(res.ok ? 'Installed.' : `Failed: ${res.error ?? ''}`)
                })
              }
            }}
          />
          <button
            className="btn primary small"
            onClick={() => {
              void window.prime.skillsInstall(pkgSource.trim()).then((r) => {
                const res = r as { ok: boolean; error?: string }
                setResult(res.ok ? 'Installed.' : `Failed: ${res.error ?? ''}`)
              })
            }}
          >
            Install
          </button>
        </div>
        {result && <div className="hint-text pad-top">{result}</div>}
      </section>

      <section className="panel plugin-list-panel">
        <div className="panel-toolbar plugin-toolbar">
          <div className="plugin-heading">
            <div className="panel-title">Installed</div>
            <div className="panel-count">{skillList.length} skills</div>
          </div>
          <label className="plugin-search-wrap">
            <svg viewBox="0 0 20 20" aria-hidden="true">
              <circle cx="8.5" cy="8.5" r="5.25" />
              <path d="m12.5 12.5 4 4" />
            </svg>
            <span className="visually-hidden">Search installed skills</span>
            <input
              className="plugin-search"
              value={query}
              onChange={(event) => setQuery(event.target.value)}
              placeholder="Search skills"
            />
          </label>
        </div>
        {visibleSkills.length === 0 && <div className="empty-state">{skillList.length === 0 ? 'No skills loaded.' : 'No skills match this search.'}</div>}
        <div className="plugin-list">
          {visibleSkills.map((s) => (
            <div key={s.name} className="plugin-row">
              <div className="plugin-glyph">{s.name.slice(0, 1).toUpperCase()}</div>
              <div className="plugin-copy">
                <div className="skill-name">{s.name.replace('skill:', '')}</div>
                <div className="skill-desc">{s.description || 'No description provided.'}</div>
              </div>
              {s.location && (
                <div className="plugin-meta" title={`${s.location}${s.path ? ` · ${s.path}` : ''}`}>
                  <span className="skill-loc">{s.location}</span>
                </div>
              )}
            </div>
          ))}
        </div>
      </section>

      <section className="panel">
        <div className="panel-head">Prompt templates <span className="panel-count">{prompts.length}</span></div>
        {prompts.length === 0 && <div className="empty-state">No prompt templates loaded.</div>}
        {prompts.map((p) => (
          <div key={p.name} className="perm-row">
            <code>/{p.name}</code>
            <span className="skill-desc">{p.description || ''}</span>
            {p.path && <span className="perm-scope">{p.path}</span>}
          </div>
        ))}
      </section>
    </div>
  )
}

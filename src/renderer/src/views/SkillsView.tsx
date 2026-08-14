import { useEffect, useState } from 'react'
import type { SkillInfo } from '@shared/types'

interface Props {
  activeAgentId: string | null
}

export default function SkillsView({ activeAgentId }: Props): JSX.Element {
  const [skills, setSkills] = useState<SkillInfo[]>([])
  const [resources, setResources] = useState<ResourceSnapshot | null>(null)
  const [pkgSource, setPkgSource] = useState('')
  const [result, setResult] = useState<string | null>(null)
  const [query, setQuery] = useState('')
  const [packageOutput, setPackageOutput] = useState('')
  const [mcpServers, setMcpServers] = useState<Record<string, McpServer>>({})
  const [mcpDraft, setMcpDraft] = useState({ name: '', url: '', oauth: true, enabled: true, bearerTokenEnvVar: '' })

  useEffect(() => {
    load()
  }, [activeAgentId])

  const load = () => {
    if (activeAgentId) {
      void window.prime.skillsList(activeAgentId).then(setSkills)
      void window.prime.agentHarness(activeAgentId, 'resources').then((value) => setResources(value as ResourceSnapshot))
    }
    void window.prime.agentHarness(activeAgentId ?? '', 'package', { command: 'list' }).then((value) => {
      setPackageOutput((value as { output?: string }).output ?? '')
    })
    void window.prime.agentHarness(activeAgentId ?? '', 'mcp_get').then((value) => {
      setMcpServers((value as { servers?: Record<string, McpServer> }).servers ?? {})
    })
  }

  const packageAction = (command: 'install' | 'remove' | 'update', source = '') => {
    setResult(`${command === 'update' ? 'Updating' : command === 'remove' ? 'Removing' : 'Installing'}…`)
    void window.prime.agentHarness(activeAgentId ?? '', 'package', { command, source }).then((value) => {
      const response = value as { ok?: boolean; output?: string }
      setResult(response.ok === false ? response.output || 'Failed.' : 'Done.')
      setPkgSource('')
      load()
    }).catch((error: Error) => setResult(error.message))
  }

  const prompts = skills.filter((s) => s.source === 'prompt')
  const skillList = skills.filter((s) => s.source === 'skill')
  const visibleSkills = skillList.filter((skill) => `${skill.name} ${skill.description}`.toLowerCase().includes(query.toLowerCase()))

  return (
    <div className="view plugins-page">
      <header className="view-header">
        <div>
        <h2>Resources</h2>
        <p className="view-sub">Prime Agent packages, extensions, skills, prompts, themes, and MCP servers.</p>
        </div>
      </header>

      <section className="panel plugin-install-panel">
        <div className="panel-head">Packages</div>
        <div className="row-gap">
          <input
            className="field grow"
            placeholder="Package name, Git URL, or local path"
            value={pkgSource}
            onChange={(e) => setPkgSource(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === 'Enter' && pkgSource.trim()) {
                packageAction('install', pkgSource.trim())
              }
            }}
          />
          <button
            className="btn primary small"
            onClick={() => {
              packageAction('install', pkgSource.trim())
            }}
          >
            Install
          </button>
          <button className="btn small" disabled={!pkgSource.trim()} onClick={() => packageAction('remove', pkgSource.trim())}>Remove</button>
          <button className="btn small" onClick={() => packageAction('update')}>Update all</button>
        </div>
        {result && <div className="hint-text pad-top">{result}</div>}
        {packageOutput && <pre className="resource-package-output">{packageOutput}</pre>}
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

      <section className="panel">
        <div className="panel-head">Extensions & themes</div>
        <div className="resource-groups">
          <div>
            <div className="resource-group-title">Extensions <span>{resources?.extensions.length ?? 0}</span></div>
            {resources?.extensions.map((item) => (
              <div className="resource-row" key={item.path}>
                <code>{item.path.split('/').pop()}</code>
                <span>{item.sourceInfo?.scope ?? 'loaded'}</span>
              </div>
            ))}
            {!resources?.extensions.length && <div className="empty-state">No extensions loaded.</div>}
          </div>
          <div>
            <div className="resource-group-title">Themes <span>{resources?.themes.length ?? 0}</span></div>
            {resources?.themes.map((item, index) => (
              <div className="resource-row" key={`${item.sourcePath}-${index}`}>
                <code>{item.name || item.sourcePath?.split('/').pop() || 'theme'}</code>
                <span>{item.sourceInfo?.scope ?? 'loaded'}</span>
              </div>
            ))}
            {!resources?.themes.length && <div className="empty-state">No themes loaded.</div>}
          </div>
        </div>
        {resources && Object.values(resources.diagnostics).flat().map((diagnostic, index) => (
          <div className={`resource-diagnostic ${diagnostic.type}`} key={`${index}-${diagnostic.message}`}>
            <strong>{diagnostic.type}</strong> {diagnostic.message}
          </div>
        ))}
      </section>

      <section className="panel">
        <div className="panel-head">MCP servers</div>
        <p className="setting-desc">Remote HTTP servers are stored in Prime Agent settings. OAuth login remains available through <code>/mcp login name</code>.</p>
        {Object.entries(mcpServers).map(([name, server]) => (
          <div className="mcp-row" key={name}>
            <div>
              <strong>{name}</strong>
              <span>{server.url}</span>
            </div>
            <span className={server.enabled === false ? 'badge-warn' : 'badge-ok'}>{server.enabled === false ? 'disabled' : server.oauth ? 'OAuth' : server.bearerTokenEnvVar || 'static'}</span>
            <button
              className="btn ghost small"
              onClick={() => {
                void window.prime.agentHarness(activeAgentId ?? '', 'mcp_set', { name, remove: true }).then((value) => {
                  setMcpServers((value as { servers?: Record<string, McpServer> }).servers ?? {})
                })
              }}
            >
              remove
            </button>
          </div>
        ))}
        <div className="mcp-form">
          <input className="field" placeholder="server name" value={mcpDraft.name} onChange={(event) => setMcpDraft((value) => ({ ...value, name: event.target.value }))} />
          <input className="field grow" placeholder="https://example.com/mcp" value={mcpDraft.url} onChange={(event) => setMcpDraft((value) => ({ ...value, url: event.target.value }))} />
          <input className="field" placeholder="Bearer token env var (optional)" value={mcpDraft.bearerTokenEnvVar} onChange={(event) => setMcpDraft((value) => ({ ...value, bearerTokenEnvVar: event.target.value }))} />
          <label><input type="checkbox" checked={mcpDraft.oauth} onChange={(event) => setMcpDraft((value) => ({ ...value, oauth: event.target.checked }))} /> OAuth</label>
          <label><input type="checkbox" checked={mcpDraft.enabled} onChange={(event) => setMcpDraft((value) => ({ ...value, enabled: event.target.checked }))} /> enabled</label>
          <button
            className="btn primary small"
            disabled={!mcpDraft.name.trim() || !mcpDraft.url.trim()}
            onClick={() => {
              void window.prime.agentHarness(activeAgentId ?? '', 'mcp_set', mcpDraft).then((value) => {
                setMcpServers((value as { servers?: Record<string, McpServer> }).servers ?? {})
                setMcpDraft({ name: '', url: '', oauth: true, enabled: true, bearerTokenEnvVar: '' })
              }).catch((error: Error) => setResult(error.message))
            }}
          >
            Save server
          </button>
        </div>
      </section>
    </div>
  )
}

interface ResourceSnapshot {
  extensions: { path: string; sourceInfo?: { scope?: string } }[]
  themes: { name?: string; sourcePath?: string; sourceInfo?: { scope?: string } }[]
  diagnostics: Record<string, { type: string; message: string }[]>
}

interface McpServer {
  type?: string
  url?: string
  oauth?: boolean
  enabled?: boolean
  bearerTokenEnvVar?: string
}

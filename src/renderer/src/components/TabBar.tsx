interface Props {
  inspectorOpen: boolean
  onToggleInspector: () => void
  sidebarCollapsed: boolean
  onToggleSidebar: () => void
  onNewChat: () => void
  onOpenProject: () => void
  projectName?: string
}

export default function TabBar({
  inspectorOpen,
  onToggleInspector,
  sidebarCollapsed,
  onToggleSidebar,
  onNewChat,
  onOpenProject,
  projectName
}: Props): JSX.Element {
  return (
    <div className="codex-topbar">
      {sidebarCollapsed && (
        <div className="topbar-leading">
          <button className="topbar-nav-btn" onClick={onToggleSidebar} title="Open sidebar" aria-label="Open sidebar">
            <SidebarIcon />
          </button>
          <button className="topbar-nav-btn" disabled title="Back" aria-label="Back">
            <ChevronIcon direction="left" />
          </button>
          <button className="topbar-nav-btn" disabled title="Forward" aria-label="Forward">
            <ChevronIcon direction="right" />
          </button>
          <button className="topbar-nav-btn" onClick={onNewChat} title="New chat" aria-label="New chat">
            <ComposeIcon />
          </button>
          <button className="topbar-project-btn" onClick={onOpenProject} title="Open project">
            <CodeIcon />
            {projectName && <span>{projectName}</span>}
          </button>
        </div>
      )}
      <div className="topbar-drag-region" />
      <button
        className={`topbar-inspector-btn ${inspectorOpen ? 'active' : ''}`}
        onClick={onToggleInspector}
        title={inspectorOpen ? 'Close inspector' : 'Open inspector'}
        aria-label={inspectorOpen ? 'Close inspector' : 'Open inspector'}
      >
        <svg viewBox="0 0 20 20" width="16" height="16" fill="none" stroke="currentColor" strokeWidth="1.5">
          <rect x="2.5" y="3" width="15" height="14" rx="2.5" />
          <path d="M12.5 3v14" />
        </svg>
      </button>
    </div>
  )
}

function SidebarIcon(): JSX.Element {
  return <svg viewBox="0 0 24 24" aria-hidden><rect x="3" y="4" width="18" height="16" rx="2" /><path d="M9 4v16" /></svg>
}

function ChevronIcon({ direction }: { direction: 'left' | 'right' }): JSX.Element {
  return <svg viewBox="0 0 24 24" aria-hidden><path d={direction === 'left' ? 'M15 18l-6-6 6-6' : 'M9 6l6 6-6 6'} /></svg>
}

function ComposeIcon(): JSX.Element {
  return <svg viewBox="0 0 24 24" aria-hidden><path d="M12 20H5a1 1 0 01-1-1V5a1 1 0 011-1h7" /><path d="M14 4h6v6M20 4l-9 9-1 4 4-1 6-6" /></svg>
}

function CodeIcon(): JSX.Element {
  return <svg viewBox="0 0 24 24" aria-hidden><path d="M8 5H6a2 2 0 00-2 2v2a2 2 0 01-2 2 2 2 0 012 2v2a2 2 0 002 2h2M16 5h2a2 2 0 012 2v2a2 2 0 002 2 2 2 0 00-2 2v2a2 2 0 01-2 2h-2" /></svg>
}

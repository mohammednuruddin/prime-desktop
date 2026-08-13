import { contextBridge, ipcRenderer } from 'electron'
import { IPC } from '@shared/types'

const api = {
  initial: () => ipcRenderer.invoke('state:initial'),
  binaryGet: () => ipcRenderer.invoke(IPC.binaryGet),
  binaryInstall: () => ipcRenderer.invoke(IPC.binaryInstall),
  chooseFolder: () => ipcRenderer.invoke(IPC.chooseFolder),
  tabsList: () => ipcRenderer.invoke(IPC.tabsList),
  tabAdd: (path: string) => ipcRenderer.invoke(IPC.tabAdd, path),
  tabRemove: (tabId: string) => ipcRenderer.invoke(IPC.tabRemove, tabId),
  tabSelect: (tabId: string) => ipcRenderer.invoke(IPC.tabSelect, tabId),
  agentCreate: (tabId: string) => ipcRenderer.invoke(IPC.agentCreate, tabId),
  agentState: () => ipcRenderer.invoke(IPC.agentState),
  agentCommand: (agentId: string, cmd: unknown) => ipcRenderer.invoke(IPC.agentCommand, agentId, cmd),
  agentMessages: (agentId: string) => ipcRenderer.invoke(IPC.agentMessages, agentId),
  agentStats: (agentId: string) => ipcRenderer.invoke(IPC.agentStats, agentId),
  agentSessions: (agentId?: string) => ipcRenderer.invoke(IPC.agentSessions, agentId),
  agentResume: (agentId: string, path: string) => ipcRenderer.invoke(IPC.agentResume, agentId, path),
  agentCommands: (agentId: string) => ipcRenderer.invoke(IPC.agentCommands, agentId),
  agentHarness: (agentId: string, action: string, input?: Record<string, unknown>) =>
    ipcRenderer.invoke(IPC.agentHarness, agentId, action, input),
  terminalStart: (agentId: string, cols?: number, rows?: number) =>
    ipcRenderer.invoke(IPC.terminalStart, agentId, cols, rows),
  terminalWrite: (agentId: string, data: string) => ipcRenderer.invoke(IPC.terminalWrite, agentId, data),
  terminalResize: (agentId: string, cols: number, rows: number) =>
    ipcRenderer.invoke(IPC.terminalResize, agentId, cols, rows),
  terminalRestart: (agentId: string, cols?: number, rows?: number) =>
    ipcRenderer.invoke(IPC.terminalRestart, agentId, cols, rows),
  terminalClear: (agentId: string) => ipcRenderer.invoke(IPC.terminalClear, agentId),
  terminalClose: (agentId: string) => ipcRenderer.invoke(IPC.terminalClose, agentId),
  onTerminalData: (cb: (event: { agentId: string; data: string; startOffset: number; endOffset: number }) => void): (() => void) => {
    const listener = (_: unknown, payload: { agentId: string; data: string; startOffset: number; endOffset: number }) => cb(payload)
    ipcRenderer.on(IPC.terminalData, listener)
    return () => ipcRenderer.removeListener(IPC.terminalData, listener)
  },
  onTerminalExit: (cb: (event: { agentId: string; exitCode: number }) => void): (() => void) => {
    const listener = (_: unknown, payload: { agentId: string; exitCode: number }) => cb(payload)
    ipcRenderer.on(IPC.terminalExit, listener)
    return () => ipcRenderer.removeListener(IPC.terminalExit, listener)
  },
  fleetObserve: (agentId: string, sessionId: string) => ipcRenderer.invoke(IPC.fleetObserve, agentId, sessionId),
  fleetUnobserve: (sessionId: string) => ipcRenderer.invoke(IPC.fleetUnobserve, sessionId),
  fleetSend: (agentId: string, target: string, message: string, mode?: string) =>
    ipcRenderer.invoke(IPC.fleetSend, agentId, target, message, mode),
  fleetSchedules: () => ipcRenderer.invoke(IPC.fleetSchedules),
  fleetScheduleAdd: (agentId: string, schedule: string, prompt: string) =>
    ipcRenderer.invoke(IPC.fleetScheduleAdd, agentId, schedule, prompt),
  fleetScheduleCancel: (agentId: string, jobId: string) => ipcRenderer.invoke(IPC.fleetScheduleCancel, agentId, jobId),
  fleetHeartbeat: (agentId: string) => ipcRenderer.invoke(IPC.fleetHeartbeat, agentId),
  fleetHeartbeatAction: (agentId: string, action: string) => ipcRenderer.invoke(IPC.fleetHeartbeatAction, agentId, action),
  gitList: (agentId: string) => ipcRenderer.invoke(IPC.gitList, agentId),
  gitRestore: (agentId: string, sha: string) => ipcRenderer.invoke(IPC.gitRestore, agentId, sha),
  gitDiffFiles: (agentId: string) => ipcRenderer.invoke(IPC.gitDiffFiles, agentId),
  gitStatus: (agentId: string) => ipcRenderer.invoke(IPC.gitStatus, agentId),
  gitFileDiff: (agentId: string, path: string, staged: boolean) =>
    ipcRenderer.invoke(IPC.gitFileDiff, agentId, path, staged),
  gitStage: (agentId: string, paths: string[]) => ipcRenderer.invoke(IPC.gitStage, agentId, paths),
  gitUnstage: (agentId: string, paths: string[]) => ipcRenderer.invoke(IPC.gitUnstage, agentId, paths),
  gitStageAll: (agentId: string) => ipcRenderer.invoke(IPC.gitStageAll, agentId),
  gitUnstageAll: (agentId: string) => ipcRenderer.invoke(IPC.gitUnstageAll, agentId),
  gitCommit: (agentId: string, message: string) => ipcRenderer.invoke(IPC.gitCommit, agentId, message),
  permissionsList: () => ipcRenderer.invoke(IPC.permissionsList),
  permissionsSet: (pattern: string, action: 'allow' | 'deny', scope: 'global' | 'project', projectPath?: string) =>
    ipcRenderer.invoke(IPC.permissionsSet, pattern, action, scope, projectPath),
  permissionsRemove: (index: number) => ipcRenderer.invoke(IPC.permissionsRemove, index),
  dashboardSpend: () => ipcRenderer.invoke(IPC.dashboardSpend),
  dashboardModels: () => ipcRenderer.invoke(IPC.dashboardModels),
  autonomyGet: () => ipcRenderer.invoke(IPC.autonomyGet),
  autonomySet: (patch: unknown) => ipcRenderer.invoke(IPC.autonomySet, patch),
  skillsList: (agentId: string) => ipcRenderer.invoke(IPC.skillsList, agentId),
  skillsInstall: (source: string) => ipcRenderer.invoke(IPC.skillsInstall, source),
  dialogRespond: (dialogId: string, value: unknown, cancelled = false) =>
    ipcRenderer.invoke(IPC.dialogRespond, dialogId, value, cancelled),
  settingsGet: () => ipcRenderer.invoke(IPC.settingsGet),
  settingsSet: (patch: unknown) => ipcRenderer.invoke(IPC.settingsSet, patch),
  revealInFinder: (path: string) => ipcRenderer.invoke(IPC.revealInFinder, path),
  openExternal: (url: string) => ipcRenderer.invoke(IPC.openExternal, url),
  quit: () => ipcRenderer.invoke(IPC.quit),
  chooseFile: () => ipcRenderer.invoke(IPC.chooseFile),
  chooseSave: (defaultName?: string) => ipcRenderer.invoke(IPC.chooseSave, defaultName),
  authList: () => ipcRenderer.invoke(IPC.authList),
  authSet: (provider: string, key: string) => ipcRenderer.invoke(IPC.authSet, provider, key),
  authRemove: (provider: string) => ipcRenderer.invoke(IPC.authRemove, provider),
  authOpenTui: () => ipcRenderer.invoke(IPC.authOpenTui),
  rlmGet: (agentId: string) => ipcRenderer.invoke(IPC.rlmGet, agentId),
  rlmSet: (agentId: string, maxDepth: number, global?: boolean) => ipcRenderer.invoke(IPC.rlmSet, agentId, maxDepth, global),
  exportHtml: (agentId: string, outputPath?: string) => ipcRenderer.invoke(IPC.exportHtml, agentId, outputPath),
  shareSession: (agentId: string) => ipcRenderer.invoke(IPC.shareSession, agentId),
  changelog: () => ipcRenderer.invoke(IPC.changelog),
  logsReveal: () => ipcRenderer.invoke(IPC.logsReveal),
  windowFullscreen: (mode?: string) => ipcRenderer.invoke(IPC.windowFullscreen, mode),
  onEvent: (cb: (e: unknown) => void): (() => void) => {
    const listener = (_: unknown, payload: unknown) => cb(payload)
    ipcRenderer.on(IPC.eventStream, listener)
    return () => {
      ipcRenderer.removeListener(IPC.eventStream, listener)
    }
  },
  onToast: (cb: (e: unknown) => void): (() => void) => {
    const listener = (_: unknown, payload: unknown) => cb(payload)
    ipcRenderer.on(IPC.toastStream, listener)
    return () => {
      ipcRenderer.removeListener(IPC.toastStream, listener)
    }
  },
  onMenuOpenFolder: (cb: () => void): (() => void) => {
    const listener = () => cb()
    ipcRenderer.on('menu:open-folder', listener)
    return () => {
      ipcRenderer.removeListener('menu:open-folder', listener)
    }
  }
}

contextBridge.exposeInMainWorld('prime', api)

export type PrimeApi = typeof api

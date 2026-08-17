import { app, BrowserWindow, Menu, nativeTheme } from 'electron'
import { join } from 'path'
import { BinaryManager } from './binary'
import { AgentManager } from './agentManager'
import { registerIpc } from './ipc'
import { getState } from './store'
import type { AppSettings } from '@shared/types'
import { resolveThemeMode } from '@shared/themes'
import { logError } from './logger'

let mainWindow: BrowserWindow | null = null
let manager: AgentManager | null = null

function createWindow(settings: AppSettings): void {
  const variant = resolveThemeMode(settings.themeMode, nativeTheme.shouldUseDarkColors)
  const windowTheme = variant === 'dark' ? settings.darkTheme : settings.lightTheme
  mainWindow = new BrowserWindow({
    width: 1280,
    height: 860,
    minWidth: 980,
    minHeight: 640,
    title: 'Prime',
    titleBarStyle: 'hiddenInset',
    trafficLightPosition: { x: 16, y: 18 },
    backgroundColor: windowTheme.opaqueWindows ? windowTheme.surface : '#00000000',
    vibrancy: process.platform === 'darwin' && !windowTheme.opaqueWindows
      ? (variant === 'light' ? 'under-window' : 'sidebar')
      : undefined,
    visualEffectState: process.platform === 'darwin' && !windowTheme.opaqueWindows ? 'active' : undefined,
    webPreferences: {
      preload: join(__dirname, '../preload/index.mjs'),
      contextIsolation: true,
      nodeIntegration: false,
      sandbox: false
    }
  })

  mainWindow.on('closed', () => {
    mainWindow = null
  })

  const devUrl = process.env['ELECTRON_RENDERER_URL']
  if (devUrl) {
    void mainWindow.loadURL(devUrl)
  } else {
    void mainWindow.loadFile(join(__dirname, '../renderer/index.html'))
  }
}

function updateWindowTheme(settings: AppSettings): void {
  const window = mainWindow
  if (!window || window.isDestroyed()) return
  const variant = resolveThemeMode(settings.themeMode, nativeTheme.shouldUseDarkColors)
  const windowTheme = variant === 'dark' ? settings.darkTheme : settings.lightTheme
  window.setBackgroundColor(windowTheme.opaqueWindows ? windowTheme.surface : '#00000000')
  if (process.platform === 'darwin') {
    window.setVibrancy(windowTheme.opaqueWindows ? null : (variant === 'light' ? 'under-window' : 'sidebar'))
  }
}

function buildMenu(): void {
  const template: Electron.MenuItemConstructorOptions[] = [
    {
      label: app.name,
      submenu: [
        { role: 'about' },
        { type: 'separator' },
        {
          label: 'Open Folder…',
          accelerator: 'CmdOrCtrl+O',
          click: () => mainWindow?.webContents.send('menu:open-folder')
        },
        { type: 'separator' },
        { role: 'hide' },
        { role: 'hideOthers' },
        { role: 'unhide' },
        { type: 'separator' },
        { role: 'quit' }
      ]
    },
    {
      label: 'Edit',
      submenu: [
        { role: 'undo' },
        { role: 'redo' },
        { type: 'separator' },
        { role: 'cut' },
        { role: 'copy' },
        { role: 'paste' },
        { role: 'selectAll' }
      ]
    },
    {
      label: 'View',
      submenu: [
        { role: 'reload' },
        { role: 'toggleDevTools' },
        { type: 'separator' },
        { role: 'resetZoom' },
        { role: 'zoomIn' },
        { role: 'zoomOut' },
        { type: 'separator' },
        { role: 'togglefullscreen' }
      ]
    },
    {
      label: 'Window',
      submenu: [{ role: 'minimize' }, { role: 'zoom' }, { type: 'separator' }, { role: 'front' }]
    }
  ]
  Menu.setApplicationMenu(Menu.buildFromTemplate(template))
}

app.whenReady().then(async () => {
  buildMenu()

  const binary = new BinaryManager()
  manager = new AgentManager(binary)
  registerIpc(() => mainWindow, manager, binary)

  const state = await getState()
  await binary.check()

  createWindow(state.settings)
  nativeTheme.on('updated', () => {
    void getState().then((latest) => updateWindowTheme(latest.settings))
  })

  for (const tab of state.tabs) {
    try {
      await manager.openTab(tab, state.settings)
    } catch (error) {
      logError(`Failed to restore project tab ${tab.path}`, error)
    }
  }

  app.on('activate', () => {
    if (BrowserWindow.getAllWindows().length === 0) {
      void getState().then((latest) => createWindow(latest.settings))
    }
  })
})

process.on('uncaughtException', (error) => logError('Uncaught main-process exception', error))
process.on('unhandledRejection', (reason) => logError('Unhandled main-process rejection', reason))

app.on('window-all-closed', () => {
  if (process.platform !== 'darwin') app.quit()
})

app.on('before-quit', () => {
  manager?.shutdownAll()
})

app.on('will-quit', () => {
  manager?.shutdownAll()
})

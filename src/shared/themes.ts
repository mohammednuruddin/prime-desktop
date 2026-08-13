import type { ThemeConfig, ThemeMode } from './types'

export const PRIME_LIGHT_THEME: ThemeConfig = {
  accent: '#c97b76',
  contrast: 42,
  fonts: { code: null, ui: null },
  ink: '#5c5870',
  opaqueWindows: true,
  semanticColors: {
    diffAdded: '#2da44e',
    diffRemoved: '#cf222e',
    skill: '#c4846a'
  },
  surface: '#ffffff'
}

export const CODEX_DARK_THEME: ThemeConfig = {
  accent: '#339cff',
  contrast: 60,
  fonts: { code: null, ui: null },
  ink: '#ffffff',
  opaqueWindows: false,
  semanticColors: {
    diffAdded: '#40c977',
    diffRemoved: '#fa423e',
    skill: '#ad7bf9'
  },
  surface: '#00000e'
}

export function resolveThemeMode(mode: ThemeMode, systemDark: boolean): 'light' | 'dark' {
  return mode === 'system' ? (systemDark ? 'dark' : 'light') : mode
}

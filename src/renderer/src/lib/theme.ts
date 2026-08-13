import type { AppSettings, ThemeConfig } from '@shared/types'
import { resolveThemeMode } from '@shared/themes'

const UI_FALLBACK = '"SF Pro Text", "SF Pro Display", -apple-system, BlinkMacSystemFont, "Helvetica Neue", Arial, sans-serif'
const CODE_FALLBACK = 'ui-monospace, "SF Mono", "SFMono-Regular", Menlo, Monaco, "Cascadia Mono", Consolas, monospace'

function hex(value: string, fallback: string): string {
  return /^#[0-9a-f]{6}$/i.test(value) ? value : fallback
}

function rgb(value: string): [number, number, number] {
  const normalized = hex(value, '#000000').slice(1)
  return [
    Number.parseInt(normalized.slice(0, 2), 16),
    Number.parseInt(normalized.slice(2, 4), 16),
    Number.parseInt(normalized.slice(4, 6), 16)
  ]
}

function mix(from: string, to: string, amount: number): string {
  const a = rgb(from)
  const b = rgb(to)
  const channel = (index: number) => Math.round(a[index] + (b[index] - a[index]) * amount)
  return `rgb(${channel(0)} ${channel(1)} ${channel(2)})`
}

function alpha(value: string, opacity: number): string {
  const [r, g, b] = rgb(value)
  return `rgb(${r} ${g} ${b} / ${opacity})`
}

export function applyAppTheme(settings: AppSettings, systemDark: boolean): void {
  const variant = resolveThemeMode(settings.themeMode, systemDark)
  const theme: ThemeConfig = variant === 'dark' ? settings.darkTheme : settings.lightTheme
  const surface = hex(theme.surface, variant === 'dark' ? '#00000e' : '#ffffff')
  const ink = hex(theme.ink, variant === 'dark' ? '#ffffff' : '#5c5870')
  const accent = hex(theme.accent, '#339cff')
  const contrast = Math.max(0, Math.min(100, theme.contrast)) / 100
  const root = document.documentElement

  root.dataset.theme = variant
  root.dataset.themeMode = settings.themeMode
  root.dataset.translucentSidebar = String(!theme.opaqueWindows)
  root.dataset.codeTheme = settings.codeThemeId
  root.style.colorScheme = variant

  const vars: Record<string, string> = {
    '--bg': surface,
    '--surface': surface,
    '--bg-sidebar': mix(surface, ink, 0.025 + contrast * 0.045),
    '--bg-card': mix(surface, ink, variant === 'dark' ? 0.035 : 0),
    '--bg-hover': mix(surface, ink, 0.055 + contrast * 0.075),
    '--bg-active': mix(surface, ink, 0.075 + contrast * 0.09),
    '--bg-input': mix(surface, ink, variant === 'dark' ? 0.045 : 0),
    '--bg-user': mix(surface, ink, 0.04 + contrast * 0.06),
    '--border': mix(surface, ink, 0.075 + contrast * 0.105),
    '--border-subtle': mix(surface, ink, 0.045 + contrast * 0.065),
    '--ink': ink,
    '--text': ink,
    '--text-muted': mix(ink, surface, variant === 'dark' ? 0.38 : 0.34),
    '--text-dim': mix(ink, surface, variant === 'dark' ? 0.56 : 0.58),
    '--ink-dim': mix(ink, surface, 0.38),
    '--accent': accent,
    '--accent-hover': mix(accent, variant === 'dark' ? '#ffffff' : '#000000', 0.14),
    '--accent-light': alpha(accent, variant === 'dark' ? 0.18 : 0.12),
    '--diff-added': hex(theme.semanticColors.diffAdded, '#40c977'),
    '--diff-removed': hex(theme.semanticColors.diffRemoved, '#fa423e'),
    '--skill': hex(theme.semanticColors.skill, '#ad7bf9'),
    '--green': hex(theme.semanticColors.diffAdded, '#40c977'),
    '--red': hex(theme.semanticColors.diffRemoved, '#fa423e'),
    '--purple': hex(theme.semanticColors.skill, '#ad7bf9'),
    '--font-ui': theme.fonts.ui?.trim() || UI_FALLBACK,
    '--font-code': theme.fonts.code?.trim() || CODE_FALLBACK,
    '--theme-shadow': variant === 'dark' ? 'rgb(0 0 0 / .42)' : 'rgb(50 45 70 / .12)'
  }

  for (const [name, value] of Object.entries(vars)) root.style.setProperty(name, value)
  window.dispatchEvent(new CustomEvent('prime-theme-change', { detail: { variant, theme } }))
}

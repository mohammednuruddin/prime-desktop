import { describe, expect, it } from 'vitest'
// @ts-expect-error plain script, no types
import { section, versions } from '../scripts/release-notes.mjs'

const changelog = `# Changelog

## [Unreleased]

### Added

- Next thing.

## [0.2.0] - 2026-11-01

### Fixed

- A thing.

## [0.1.0] - 2026-10-01

- First.
`

describe('release notes', () => {
  it('takes the section of one version and nothing from its neighbours', () => {
    expect(section(changelog, '0.2.0')).toBe('### Fixed\n\n- A thing.')
    expect(section(changelog, '0.1.0')).toBe('- First.')
    expect(section(changelog, 'Unreleased')).toContain('Next thing')
  })

  it('finds nothing for a version that was never written down', () => {
    expect(section(changelog, '0.3.0')).toBe('')
    expect(section(changelog, '0.2')).toBe('')
  })

  it('reads the version from every file that carries it', () => {
    const found = versions((file: string) =>
      file.endsWith('.json') ? '{"version":"1.2.3"}' : '[package]\nname = "x"\nversion = "1.2.3"\n\n[dependencies]\nversion = "9"\n'
    )
    expect(Object.values(found)).toEqual(['1.2.3', '1.2.3', '1.2.3', '1.2.3'])
  })
})

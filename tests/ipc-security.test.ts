import { describe, expect, it } from 'vitest'
import { isWithin, requireSafeExternalUrl } from '../src/main/ipcSecurity'

describe('IPC security helpers', () => {
  it('accepts HTTP(S) URLs and rejects dangerous protocols', () => {
    expect(requireSafeExternalUrl('https://example.com/path')).toBe('https://example.com/path')
    expect(() => requireSafeExternalUrl('javascript:alert(1)')).toThrow(/HTTP\(S\)/)
    expect(() => requireSafeExternalUrl('file:///etc/passwd')).toThrow(/HTTP\(S\)/)
  })

  it('checks path containment without prefix confusion', () => {
    expect(isWithin('/tmp/project', '/tmp/project/src')).toBe(true)
    expect(isWithin('/tmp/project', '/tmp/project-other')).toBe(false)
  })
})

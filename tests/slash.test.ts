import { describe, expect, it } from 'vitest'
import { parseSlash } from '../src/shared/slash'

describe('slash command parsing', () => {
  it('keeps quoted arguments together', () => {
    const parsed = parseSlash('/model "openai/gpt-5"')
    expect(parsed?.name).toBe('model')
    expect(parsed?.args).toBe('"openai/gpt-5"')
  })
})

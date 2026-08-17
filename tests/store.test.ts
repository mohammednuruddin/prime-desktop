import { describe, expect, it } from 'vitest'
import { mergeMessage, patchToolExecs } from '../src/renderer/src/lib/store'

describe('renderer event state reducers', () => {
  it('merges tool results into the matching tool call', () => {
    const messages = mergeMessage([], {
      id: 'assistant-1',
      role: 'assistant',
      content: [{ type: 'toolCall', id: 'tool-1', name: 'bash', status: 'running' }]
    })
    const next = mergeMessage(messages, {
      id: 'result-1',
      role: 'toolResult',
      toolCallId: 'tool-1',
      content: 'done'
    })
    expect(next[0].content).toEqual([
      { type: 'toolCall', id: 'tool-1', name: 'bash', status: 'done', result: 'done', isError: false }
    ])
  })

  it('does not regress a completed tool execution on late updates', () => {
    const started = patchToolExecs({}, 'start', { toolCallId: 'tool-1', toolName: 'bash', args: {} })
    const ended = patchToolExecs(started, 'end', { toolCallId: 'tool-1', toolName: 'bash', result: { content: 'done' } })
    const late = patchToolExecs(ended, 'update', { toolCallId: 'tool-1', partialResult: { content: 'stale' } })
    expect(late['tool-1'].status).toBe('done')
    expect(late['tool-1'].output).toBe('done')
  })
})

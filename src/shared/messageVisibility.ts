const IPYTHON_RESTORE_TAG = /<ipython_state_restored(?:\s|>)/i

function contentText(content: unknown): string {
  if (typeof content === 'string') return content
  if (!Array.isArray(content)) return ''

  return content
    .map((block) => {
      const record = block as Record<string, unknown>
      if (typeof record.text === 'string') return record.text
      if (typeof record.content === 'string') return record.content
      return ''
    })
    .join('\n')
}

export function isInternalStateRestoreMessage(message: unknown): boolean {
  const record = message as Record<string, unknown>
  if (record.customType === 'ipython_state_restored') return true
  return IPYTHON_RESTORE_TAG.test(contentText(record.content))
}

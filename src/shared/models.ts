export interface ModelOption {
  key: string
  provider: string
  id: string
  name: string
}

function asRecord(value: unknown): Record<string, unknown> | null {
  return value && typeof value === 'object' && !Array.isArray(value) ? (value as Record<string, unknown>) : null
}

function extractEntries(raw: unknown): unknown[] {
  if (!raw) return []
  if (Array.isArray(raw)) return raw
  const obj = asRecord(raw)
  if (!obj) return []
  if (Array.isArray(obj.models)) return obj.models
  const data = asRecord(obj.data)
  if (data && Array.isArray(data.models)) return data.models
  const providers = asRecord(obj.providers)
  if (providers) {
    const out: unknown[] = []
    for (const [provider, cfg] of Object.entries(providers)) {
      const models = asRecord(cfg)?.models
      if (!Array.isArray(models)) continue
      for (const model of models) {
        const rec = asRecord(model)
        out.push(rec ? { ...rec, provider: rec.provider ?? provider } : model)
      }
    }
    return out
  }
  return []
}

export function parseModelList(raw: unknown): ModelOption[] {
  const seen = new Set<string>()
  const out: ModelOption[] = []
  for (const entry of extractEntries(raw)) {
    if (typeof entry === 'string') {
      const id = entry.trim()
      if (!id || seen.has(id)) continue
      seen.add(id)
      const slash = id.indexOf('/')
      out.push({
        key: id,
        provider: slash >= 0 ? id.slice(0, slash) : '',
        id: slash >= 0 ? id.slice(slash + 1) : id,
        name: slash >= 0 ? id.slice(slash + 1) : id
      })
      continue
    }
    const rec = asRecord(entry)
    if (!rec) continue
    const id = String(rec.id ?? rec.modelId ?? rec.name ?? '').trim()
    if (!id) continue
    const provider = typeof rec.provider === 'string' ? rec.provider : ''
    const key = provider ? `${provider}/${id}` : id
    if (seen.has(key)) continue
    seen.add(key)
    out.push({
      key,
      provider,
      id,
      name: String(rec.name ?? id)
    })
  }
  return out
}

export function modelKeyFromState(model: unknown): string | null {
  if (!model) return null
  if (typeof model === 'string') return model
  const rec = asRecord(model)
  if (!rec) return null
  const id = String(rec.id ?? rec.modelId ?? '').trim()
  const provider = typeof rec.provider === 'string' ? rec.provider : ''
  if (provider && id) return `${provider}/${id}`
  if (id) return id
  const name = String(rec.name ?? '').trim()
  return name || null
}

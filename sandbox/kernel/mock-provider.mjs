// A scripted OpenAI-compatible chat server for testing the real agent engine with no network and no real model.
// First request of a conversation: it calls the agent's Python tool with the code in $MOCK_CODE. After the tool's
// result comes back: it answers with that result. Every request is logged to $MOCK_LOG.
//   MOCK_PORT=18080 MOCK_CODE=probe.py MOCK_LOG=requests.jsonl node mock-provider.mjs
import { appendFileSync, readFileSync } from 'node:fs'
import http from 'node:http'

const port = Number(process.env.MOCK_PORT ?? 18080)
const log = process.env.MOCK_LOG
const code = () => readFileSync(process.env.MOCK_CODE, 'utf8')

const send = (res, chunks) => {
  res.writeHead(200, { 'content-type': 'text/event-stream', 'cache-control': 'no-cache' })
  for (const chunk of chunks) res.write(`data: ${JSON.stringify(chunk)}\n\n`)
  res.write('data: [DONE]\n\n')
  res.end()
}
const base = (delta, finish = null) => ({ id: 'mock-1', object: 'chat.completion.chunk', created: 1, model: 'mock-1', choices: [{ index: 0, delta, finish_reason: finish }] })

http.createServer((req, res) => {
  let body = ''
  req.on('data', (d) => (body += d))
  req.on('end', () => {
    if (!req.url.endsWith('/chat/completions')) { res.writeHead(404).end(); return }
    const request = JSON.parse(body || '{}')
    if (log) appendFileSync(log, JSON.stringify({ tools: (request.tools ?? []).map((t) => ({ name: t.function?.name, params: t.function?.parameters })), roles: (request.messages ?? []).map((m) => m.role) }) + '\n')
    const messages = request.messages ?? []
    const toolResult = [...messages].reverse().find((m) => m.role === 'tool')
    if (toolResult) {
      const text = typeof toolResult.content === 'string' ? toolResult.content : JSON.stringify(toolResult.content)
      send(res, [base({ role: 'assistant', content: `tool said: ${text.slice(0, 1500)}` }), base({}, 'stop')])
      return
    }
    const tools = request.tools ?? []
    const tool = tools.find((t) => /ipython|python|repl/i.test(t.function?.name ?? '')) ?? tools[0]
    if (!tool) { send(res, [base({ role: 'assistant', content: 'no tools were offered' }), base({}, 'stop')]); return }
    const props = Object.keys(tool.function.parameters?.properties ?? {})
    const key = ['code', 'source', 'script', 'command', 'cmd'].find((k) => props.includes(k)) ?? props[0]
    const args = JSON.stringify({ [key]: code() })
    send(res, [
      base({ role: 'assistant', content: null, tool_calls: [{ index: 0, id: 'call_1', type: 'function', function: { name: tool.function.name, arguments: '' } }] }),
      base({ tool_calls: [{ index: 0, function: { arguments: args } }] }),
      base({}, 'tool_calls')
    ])
  })
}).listen(port, '127.0.0.1', () => console.log(`mock provider on ${port}`))

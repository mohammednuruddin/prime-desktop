import { RpcClient } from '../src/main/rpc'

const client = new RpcClient({ binary: '/opt/homebrew/bin/prime-agent', cwd: '/tmp' })
client.on('event', (ev) => {
  const t = ev.type as string
  if (t === 'message_update') {
    const de = (ev as { assistantMessageEvent?: { type?: string; delta?: string } }).assistantMessageEvent
    if (de?.type === 'text_delta') process.stdout.write(de.delta)
  } else if (t === 'tool_execution_start') {
    console.log(`\n[tool] start ${(ev as { toolName?: string }).toolName}`)
  } else if (t === 'tool_execution_end') {
    console.log(`\n[tool] end ${(ev as { toolName?: string }).toolName} isError=${(ev as { isError?: boolean }).isError}`)
  } else if (t !== 'message_start' && t !== 'message_end' && t !== 'turn_start' && t !== 'agent_start' && t !== 'session_action_update') {
    console.log(`\n[event] ${t}`)
  }
})
await client.start()
console.log('sending prompt…')
await client.send({ type: 'prompt', message: 'Reply with exactly: PONG' })
await new Promise((r) => setTimeout(r, 60000))
console.log('\n--- stats ---')
const stats = await client.send({ type: 'get_session_stats' })
console.log(JSON.stringify(stats).slice(0, 500))
client.stop()
setTimeout(() => process.exit(0), 2000)
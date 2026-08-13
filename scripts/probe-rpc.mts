import { RpcClient } from '../src/main/rpc'

async function main() {
  const client = new RpcClient({ binary: '/opt/homebrew/bin/prime-agent', cwd: '/tmp' })
  client.on('event', (ev) => {
    console.log('[event]', ev.type)
  })
  client.on('stderr', (t) => console.log('[stderr]', t.slice(0, 200)))
  await client.start()
  console.log('--- get_state ---')
  const state = await client.send({ type: 'get_state' })
  console.log(JSON.stringify(state).slice(0, 400))
  console.log('--- get_available_models ---')
  const models = await client.send({ type: 'get_available_models' })
  console.log(JSON.stringify(models).slice(0, 300))
  console.log('--- get_messages ---')
  const msgs = await client.send({ type: 'get_messages' })
  const m = msgs as { messages: unknown[] }
  console.log('count:', m.messages.length)
  console.log('--- get_commands ---')
  const cmds = await client.send({ type: 'get_commands' })
  console.log(JSON.stringify(cmds).slice(0, 300))
  client.stop()
  setTimeout(() => process.exit(0), 4000)
}

main().catch((e) => {
  console.error('FAILED:', e.message)
  process.exit(1)
})

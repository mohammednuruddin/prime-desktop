import { BinaryManager } from '../src/main/binary'
import { AgentManager } from '../src/main/agentManager'
import type { AppSettings } from '@shared/types'
import { mkdirSync } from 'fs'
import { join } from 'path'
import { execSync } from 'child_process'

const workdir = join('/tmp', 'prime-desktop-test-' + Date.now())
mkdirSync(workdir, { recursive: true })
try {
  execSync('git init -q && git config user.email t@t.t && git config user.name t && git commit -q -m init --allow-empty', { cwd: workdir })
} catch {
  /* not a git env */
}

const binary = new BinaryManager()
await binary.check()
console.log('binary:', binary.stateSnapshot.status, binary.stateSnapshot.version)

const settings: AppSettings = {
  notifications: false,
  checkpoints: true,
  dockBadge: false,
  thinkingLevel: 'medium',
  autoCompaction: true,
  autoRetry: true,
  model: null
}

const manager = new AgentManager(binary)
const seenTypes = new Set<string>()
manager.on('renderer', ({ channel, payload }) => {
  const e = payload as { type?: string }
  if (channel === 'events' && e?.type) seenTypes.add(e.type)
})

const tab = { id: 'tab-test', path: workdir, name: 'test' }
const info = await manager.openTab(tab, settings)
console.log('agent opened:', info.status, '| path:', info.path)

await new Promise((r) => setTimeout(r, 8000))

const s = await manager.getSessions()
console.log('sessions scan:', s.length >= 0 ? 'ok' : 'err')
const msgs = await manager.getMessages(info.id)
console.log('messages after open:', msgs.length)

const diffs = await manager.diffFiles(info.id)
console.log('diffFiles:', diffs.map((d) => `${d.path}:${d.status}`).slice(0, 5))

await manager.runCommand(info.id, { type: 'set_thinking_level', level: 'low' }, settings)
const stats = await manager.getStats(info.id)
console.log('stats fetched:', !!stats)

manager.shutdownAll()
console.log('--- event types seen ---')
console.log([...seenTypes].slice(0, 30).join(', '))
console.log('OK')
process.exit(0)
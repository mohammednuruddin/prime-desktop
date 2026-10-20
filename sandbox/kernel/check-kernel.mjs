// Runs the real Prime Agent engine twice against a scripted fake model that has the agent run Python, once with the
// kernel unprotected and once with the kernel inside the sandbox, and compares what that Python could do.
//
//   node sandbox/kernel/check-kernel.mjs --agent <prime-agent exe> --venv <kernel venv dir> \
//        --helper <desktop-sandbox exe> --client <prime-agent-client.js>
//
// The kernel venv is a Python 3.11+ environment with the agent's `prime-agent-runtime` and its default packages
// installed (see docs/memory/agent-link-rust-daemon.md). The client is the vendored daemon client from the agent-link
// branch. No network is needed and no real model is used. Exit 0 when the protected run holds and the unprotected
// run shows what it protects.
import { spawn, spawnSync } from 'node:child_process'
import { copyFileSync, lstatSync, mkdirSync, mkdtempSync, readFileSync, readlinkSync, realpathSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { dirname, join, resolve } from 'node:path'
import { fileURLToPath, pathToFileURL } from 'node:url'

const here = dirname(fileURLToPath(import.meta.url))
const args = process.argv.slice(2)
const need = (name) => { const i = args.indexOf(name); if (i < 0 || !args[i + 1]) { console.error(`missing ${name}`); process.exit(64) } return resolve(args[i + 1]) }
const agent = need('--agent'); const venv = need('--venv'); const helper = need('--helper'); const clientPath = need('--client')
const win = process.platform === 'win32'
const python = join(venv, win ? 'Scripts/python.exe' : 'bin/python')
// A Windows environment's python.exe is a launcher that needs GUI access, which the sandbox withholds. Inside it the
// base interpreter runs, with the environment's packages on PYTHONPATH. Elsewhere the environment's own python is fine.
function baseInterpreter() {
  const cfg = readFileSync(join(venv, 'pyvenv.cfg'), 'utf8')
  const home = /^home\s*=\s*(.+)$/m.exec(cfg)?.[1]?.trim()
  if (!home) throw new Error('pyvenv.cfg names no base interpreter')
  return join(home, 'python.exe')
}
const sandboxPython = win ? baseInterpreter() : python
const sitePackages = join(venv, 'Lib', 'site-packages')
const sleep = (ms) => new Promise((r) => setTimeout(r, ms))

// Every folder the interpreter is reached through: the venv, and each hop of its symlink chain. A link that points
// through a folder name the sandbox cannot see would dangle, so the policy names them as the host would.
function interpreterRoots() {
  const roots = new Set([venv])
  let current = sandboxPython
  for (let hops = 0; hops < 8; hops++) {
    let real
    try { if (!lstatSync(current).isSymbolicLink()) break; real = resolve(dirname(current), readlinkSync(current)) } catch { break }
    roots.add(dirname(dirname(real)))
    current = real
  }
  roots.add(dirname(dirname(realpathSync(sandboxPython))))
  if (win) roots.add(dirname(sandboxPython))
  return [...roots]
}

const scratch = mkdtempSync(join(tmpdir(), 'check-kernel-'))
const stateDir = join(scratch, 'state'); mkdirSync(stateDir)
const policyFile = join(scratch, 'kernel-policy.json')
writeFileSync(policyFile, JSON.stringify({
  schemaVersion: 1, id: 'kernel', label: 'Agent kernel', readRoots: interpreterRoots(), network: 'none',
  envAllowlist: ['PATH', 'HOME', 'LANG', 'TERM', 'PYTHONDONTWRITEBYTECODE', 'CHECK_HOME', ...(win ? ['SystemRoot', 'PYTHONPATH'] : [])], maxProcesses: 64,
  // The agent's runtime and pandas import ctypes, whose _ctypes module cannot initialise in a container without GUI
  // access on Windows. The helper lists what that gives up among its limitations.
  ...(win ? { allowGui: true } : {})
}))
const wrapper = join(scratch, win ? 'desktop-kernel.exe' : 'desktop-kernel')
copyFileSync(helper, wrapper)
const sandboxEnv = { ...process.env, PRIME_DESKTOP_SANDBOX_STATE_DIR: stateDir }
const attested = spawnSync(helper, ['attest'], { env: sandboxEnv, encoding: 'utf8' })
if (attested.status !== 0) { console.error('the helper would not attest this host:\n' + attested.stdout + attested.stderr); process.exit(2) }

const port = 18000 + Math.floor(Math.random() * 1000)
const mock = spawn(process.execPath, [join(here, 'mock-provider.mjs')], { env: { ...process.env, MOCK_PORT: String(port), MOCK_CODE: join(here, 'probe.py') }, stdio: 'ignore' })
await sleep(800)
const { DaemonClient, DaemonAgentConnection } = await import(pathToFileURL(clientPath).href)

async function runOnce(label, kernelPython) {
  const home = join(scratch, `home-${label}`); const work = join(scratch, `work-${label}`); const agentDir = join(home, 'agent')
  mkdirSync(join(home, '.ssh'), { recursive: true }); mkdirSync(agentDir, { recursive: true }); mkdirSync(work)
  writeFileSync(join(home, '.ssh', 'id_test'), 'PRIVATE-KEY-MATERIAL')
  writeFileSync(join(agentDir, 'models.json'), JSON.stringify({ providers: { mock: { baseUrl: `http://127.0.0.1:${port}`, apiKey: 'none', api: 'openai-completions', models: [{ id: 'mock-1', name: 'Mock', reasoning: false }] } } }))
  const socket = win ? `\\\\.\\pipe\\check-kernel-${label}-${process.pid}` : join(scratch, `${label}.sock`)
  const env = { ...sandboxEnv, HOME: home, USERPROFILE: home, CHECK_HOME: home, PRIME_AGENT_CODING_AGENT_DIR: agentDir, PRIME_AGENT_TELEMETRY: '0',
    PRIME_AGENT_KERNEL_PYTHON: kernelPython, PRIME_DESKTOP_REAL_PYTHON: sandboxPython, ...(win ? { PRIME_DESKTOP_KERNEL_PYTHONPATH: sitePackages } : {}), PRIME_DESKTOP_KERNEL_POLICY: policyFile, PRIME_DESKTOP_SECRET_ENV: '1' }
  const daemon = spawn(agent, ['--mode', 'daemon', '--daemon-socket', socket], { cwd: work, env, stdio: 'ignore', windowsHide: true })
  let client
  try {
    for (let i = 0; i < 160 && !client; i++) { await sleep(250); const c = new DaemonClient(socket); try { await c.connect(500); client = c } catch { c.close() } }
    if (!client) throw new Error('the daemon did not start')
    const created = await client.request({ type: 'create', lifecycle: 'resident', config: { cwd: work, provider: 'mock', model: 'mock-1', thinking: 'off' } }, 60_000)
    if (created.success !== true) throw new Error(`create failed: ${created.error}`)
    const id = created.data.activeSessionId
    const conn = await DaemonAgentConnection.attach(client, id, { closeClientOnDispose: false, supportsExtensionUi: true, sendClientEnv: true })
    const events = []
    conn.subscribe((e) => events.push(e.type === 'session_event' ? e.event : e))
    await conn.prompt('run the probe', {})
    const until = Date.now() + 180_000
    while (!events.some((e) => e.type === 'agent_end') && Date.now() < until) await sleep(300)
    const end = events.find((e) => e.type === 'tool_execution_end')
    const text = end?.result?.content?.[0]?.text ?? ''
    try { return JSON.parse(text.trim().split('\n')[0]) } catch { return { error: text.slice(0, 300) || 'no tool result' } }
  } finally {
    try { await client?.request({ type: 'shutdown', force: true }, 3000).catch(() => {}); client?.close() } catch {}
    daemon.kill(); await sleep(400)
  }
}

let ok = false
try {
  const before = await runOnce('before', python)
  const after = await runOnce('after', wrapper)
  const rows = ['read_private_key', 'list_home', 'network', 'env_has_secret', 'write_cwd', 'subprocess']
  console.log(`${'what the model\'s Python tried'.padEnd(30)} ${'kernel unprotected'.padEnd(34)} kernel in the sandbox`)
  const show = (v) => (typeof v === 'string' && v.startsWith('ERR') ? 'blocked' : JSON.stringify(v)).slice(0, 32)
  for (const key of rows) console.log(`${key.padEnd(30)} ${show(before[key]).padEnd(34)} ${show(after[key])}`)
  const blocked = (v) => typeof v === 'string' && v.startsWith('ERR')
  const exposed = before.read_private_key === 'PRIVATE-KEY-MATERIAL' && Array.isArray(before.list_home) && before.env_has_secret === true
  const held = blocked(after.read_private_key) && blocked(after.list_home) && blocked(after.network) && after.env_has_secret === false
  const works = after.write_cwd === 1 && after.subprocess === 'from-child'
  console.log(`\nunprotected run exposes the key, the home folder and the secret: ${exposed}`)
  console.log(`protected run blocks them and the network: ${held}`)
  console.log(`protected run can still write its workspace and run a subprocess: ${works}`)
  ok = exposed && held && works
} catch (error) {
  console.error('check failed:', error instanceof Error ? error.message : error)
} finally {
  mock.kill()
  try { rmSync(scratch, { recursive: true, force: true }) } catch {}
}
process.exit(ok ? 0 : 1)

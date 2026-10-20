// Puts the pinned Prime Agent release beside the app, in src-tauri/binaries/agent, for `tauri build` to ship under `agent/`.
//   node scripts/tauri-agent.mjs [--target <triple>] [--sign "<command with %1 for the program>"]
// The release and its hashes are the ones the app itself is pinned to (src-tauri/crates/core/src/agent/release.rs). The
// archive is checked against its hash before it is unpacked and the program against its own, then signed with the build's
// signature: a machine that blocks unknown programs (Smart App Control on Windows) accepts a signed one where it refuses
// the same program downloaded later.
import { execFileSync, execSync } from 'node:child_process'
import { createHash } from 'node:crypto'
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { dirname, join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..')
const args = process.argv.slice(2)
const option = (name, fallback) => (args.includes(name) ? args[args.indexOf(name) + 1] : fallback)
const triple = option('--target', execFileSync('rustc', ['-vV'], { encoding: 'utf8' }).match(/^host: (.+)$/m)?.[1])
const KEYS = {
  'x86_64-pc-windows-msvc': 'win32-x64',
  'aarch64-apple-darwin': 'darwin-arm64',
  'x86_64-apple-darwin': 'darwin-x64',
  'x86_64-unknown-linux-gnu': 'linux-x64',
  'aarch64-unknown-linux-gnu': 'linux-arm64'
}
const key = KEYS[triple]
if (!key) throw new Error(`No Prime Agent build for ${triple}.`)

const source = readFileSync(join(root, 'src-tauri', 'crates', 'core', 'src', 'agent', 'release.rs'), 'utf8')
const version = source.match(/pub const VERSION: &str = "([^"]+)"/)?.[1]
const repo = source.match(/pub const REPO: &str = "([^"]+)"/)?.[1]
const asset = [...source.matchAll(/key: "([^"]+)",\s*file: "([^"]+)",\s*sha256: "([^"]+)",\s*executable_sha256: "([^"]+)"/g)].find((m) => m[1] === key)
if (!version || !repo || !asset) throw new Error(`release.rs has no pinned ${key} asset.`)
const [, , file, archiveHash, programHash] = asset

const out = join(root, 'src-tauri', 'binaries', 'agent')
rmSync(out, { recursive: true, force: true })
mkdirSync(out, { recursive: true })
const scratch = mkdtempSync(join(tmpdir(), 'desktop-agent-'))
try {
  const archive = join(scratch, file)
  execFileSync('curl', ['-L', '--fail', '--silent', '--show-error', '--retry', '5', '--retry-all-errors', '--connect-timeout', '30', '-o', archive, `https://github.com/${repo}/releases/download/v${version}/${file}`], { stdio: 'inherit' })
  const sha = (path) => createHash('sha256').update(readFileSync(path)).digest('hex')
  if (sha(archive) !== archiveHash) throw new Error(`${file} does not match its pinned checksum.`)
  execFileSync('tar', ['-xzf', archive, '-C', out])
  const program = join(out, triple.includes('windows') ? 'prime-agent.exe' : 'prime-agent')
  if (!existsSync(program)) throw new Error('The archive holds no prime-agent program.')
  if (sha(program) !== programHash) throw new Error('The unpacked program does not match its pinned checksum.')
  const sign = option('--sign')
  if (sign) execSync(sign.replace('%1', `"${program}"`), { stdio: 'inherit' })
} finally {
  rmSync(scratch, { recursive: true, force: true })
}
console.log(`Prime Agent ${version} for ${triple} in ${out}`)

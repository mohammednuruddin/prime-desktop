// Puts the programs the app ships beside itself where `tauri build` looks for them: src-tauri/binaries/<name>-<target triple>.
//   node scripts/tauri-sidecars.mjs [--target <triple>] [--sandbox-dir <folder with the built helper and probes>]
// The sandbox helper and its probe suite come from the sandbox workspace (build it first: `cargo build --release` in sandbox/;
// on Windows through signed-cargo, which signs what Smart App Control blocks). uv is the pinned release below, checked
// against its published checksum, because the app builds the agent's Python environment with it on first run.
import { execFileSync } from 'node:child_process'
import { createHash } from 'node:crypto'
import { chmodSync, copyFileSync, existsSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, rmSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { dirname, join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

const UV_VERSION = '0.11.12'
const UV = {
  'x86_64-pc-windows-msvc': ['zip', 'e46956a6b088a0382101c797eef945c1b03826e629e968d434cf838d42d85b6b'],
  'aarch64-pc-windows-msvc': ['zip', '393de1abc2f663cb9dd24405c7a7b31119e2a734609a233d9b89415821f39bf9'],
  'x86_64-unknown-linux-gnu': ['tar.gz', '9acdecddacba550ee616c02bb4616d894352022550c5977524556fd5077ce1d4'],
  'aarch64-unknown-linux-gnu': ['tar.gz', 'd6e3e5183e71bbd40400da3d2913743cefb98835d8312a5e7908c33865597515'],
  'aarch64-apple-darwin': ['tar.gz', 'bb7c6ef869ec00cd1452f4884acf23d00b153c356ba9197ae99a1bc1ceadb7f3'],
  'x86_64-apple-darwin': ['tar.gz', '32fb217e6181384bf6534b31adcc66cd552eff98643c4bb35832be8552486912']
}

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..')
const args = process.argv.slice(2)
const option = (name, fallback) => (args.includes(name) ? args[args.indexOf(name) + 1] : fallback)
const triple = option('--target', execFileSync('rustc', ['-vV'], { encoding: 'utf8' }).match(/^host: (.+)$/m)?.[1])
if (!triple) throw new Error('Could not tell the target triple; pass --target.')
const exe = triple.includes('windows') ? '.exe' : ''
const built = resolve(root, option('--sandbox-dir', join('sandbox', 'target', 'release')))
const out = join(root, 'src-tauri', 'binaries')
mkdirSync(out, { recursive: true })

// The probe suite starts `probe-agent` from beside itself.
for (const name of ['desktop-sandbox', 'desktop-sandbox-probes', 'probe-agent']) {
  const from = join(built, name + exe)
  if (!existsSync(from)) throw new Error(`${from} is missing; build the sandbox workspace first.`)
  copyFileSync(from, join(out, `${name}-${triple}${exe}`))
}

const entry = UV[triple]
if (!entry) throw new Error(`No pinned uv for ${triple}.`)
const [kind, sha256] = entry
const scratch = mkdtempSync(join(tmpdir(), 'desktop-uv-'))
try {
  const archive = join(scratch, `uv.${kind}`)
  const url = `https://github.com/astral-sh/uv/releases/download/${UV_VERSION}/uv-${triple}.${kind}`
  // curl comes with Windows, macOS and Linux, follows redirects, and retries a dropped connection; Node's own fetch timed
  // out on a machine where curl worked.
  execFileSync('curl', ['-L', '--fail', '--silent', '--show-error', '--retry', '5', '--retry-all-errors', '--connect-timeout', '30', '-o', archive, url], { stdio: 'inherit' })
  const bytes = readFileSync(archive)
  const actual = createHash('sha256').update(bytes).digest('hex')
  if (actual !== sha256) throw new Error(`uv ${UV_VERSION} for ${triple} does not match its pinned checksum (got ${actual}).`)
  // bsdtar (Windows, macOS) reads zip files too; GNU tar does not, so Linux uses unzip for them.
  if (kind === 'zip' && process.platform === 'linux') execFileSync('unzip', ['-q', archive, '-d', scratch])
  else execFileSync('tar', ['-xf', archive, '-C', scratch])
  const found = [scratch, ...readdirSync(scratch, { withFileTypes: true }).filter((d) => d.isDirectory()).map((d) => join(scratch, d.name))]
    .map((dir) => join(dir, `uv${exe}`))
    .find(existsSync)
  if (!found) throw new Error('The uv archive holds no uv program.')
  const target = join(out, `uv-${triple}${exe}`)
  copyFileSync(found, target)
  if (!exe) chmodSync(target, 0o755)
} finally {
  rmSync(scratch, { recursive: true, force: true })
}
console.log(`sidecars for ${triple} in ${out}`)

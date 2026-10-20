// Assembles the folder the Tauri host serves: the renderer build, with the one change below.
//   npm run build && node scripts/tauri-stage.mjs [renderer dir]
// The renderer installs its own `window.prime` over Tauri's invoke (src/renderer/src/lib/tauriPrime.ts).
import { cpSync, existsSync, mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs'
import { dirname, join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..')
const dist = join(root, 'src-tauri', 'frontend-dist')
const renderer = resolve(root, process.argv[2] ?? join('out', 'renderer'))
if (!existsSync(join(renderer, 'index.html'))) throw new Error(`No renderer build at ${renderer}; run the build first.`)
rmSync(dist, { recursive: true, force: true })
mkdirSync(dist, { recursive: true })
cpSync(renderer, dist, { recursive: true })
// The renderer's own <meta> policy (written for Electron) is enforced on top of the host's header policy and blocks the
// host's IPC origin. In Tauri the header is the one authority, so the meta tag is dropped from the staged page.
const page = join(dist, 'index.html')
const html = readFileSync(page, 'utf8')
const stripped = html.replace(/<meta\s+http-equiv="Content-Security-Policy"[^>]*>/i, '')
if (stripped === html) throw new Error('index.html has no CSP meta tag to drop; the stage step is out of date')
writeFileSync(page, stripped)
// The renderer's pages refer to /build/icon_32x32.png; serve the icons it asks for.
mkdirSync(join(dist, 'build'), { recursive: true })
for (const name of ['icon_32x32.png', 'icon_128x128.png']) cpSync(join(root, 'build', name), join(dist, 'build', name))
console.log(`staged ${dist}`)

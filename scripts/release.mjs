// Cuts a release: bumps the version everywhere it lives, dates the CHANGELOG, commits, tags and pushes.
// .github/workflows/release.yml then builds the installers and publishes them with that CHANGELOG section as the notes.
//
//   npm run release            # 0.1.0 -> 0.1.1
//   npm run release minor      # 0.1.0 -> 0.2.0
//   npm run release 1.0.0      # exact version
import { execFileSync } from 'node:child_process'
import { readFileSync, writeFileSync } from 'node:fs'
import { section } from './release-notes.mjs'

const run = (command, args) => execFileSync(command, args, { encoding: 'utf8', stdio: ['ignore', 'pipe', 'inherit'] }).trim()
const bump = process.argv[2] ?? 'patch'
if (!/^(patch|minor|major|\d+\.\d+\.\d+(-[0-9A-Za-z.-]+)?)$/.test(bump)) {
  console.error(`Unknown version "${bump}". Use patch, minor, major or an exact version like 1.2.0.`)
  process.exit(1)
}
if (run('git', ['rev-parse', '--abbrev-ref', 'HEAD']) !== 'main') {
  console.error('Releases are cut from main.')
  process.exit(1)
}
if (run('git', ['status', '--porcelain'])) {
  console.error('Commit or stash your changes first; the release is built from what is pushed.')
  process.exit(1)
}

const edit = (file, change) => writeFileSync(file, change(readFileSync(file, 'utf8')))
const current = JSON.parse(readFileSync('package.json', 'utf8')).version
const [major, minor, patch] = current.split('.').map(Number)
const next = { patch: `${major}.${minor}.${patch + 1}`, minor: `${major}.${minor + 1}.0`, major: `${major + 1}.0.0` }[bump] ?? bump

const changelog = readFileSync('CHANGELOG.md', 'utf8')
if (!/^\s*-/m.test(section(changelog, 'Unreleased'))) {
  console.error('CHANGELOG.md has nothing under [Unreleased]; write what changed first.')
  process.exit(1)
}
const today = new Date().toISOString().slice(0, 10)
edit('CHANGELOG.md', (text) => text.replace('## [Unreleased]', `## [Unreleased]\n\n## [${next}] - ${today}`))
for (const file of ['package.json', 'src-tauri/tauri.conf.json']) edit(file, (text) => text.replace(`"version": "${current}"`, `"version": "${next}"`))
for (const file of ['src-tauri/Cargo.toml', 'src-tauri/crates/core/Cargo.toml']) {
  edit(file, (text) => text.replace(`version = "${current}"`, `version = "${next}"`))
}
// The lock file names the two workspace crates; keep it in step so `cargo --locked` in CI still passes.
edit('src-tauri/Cargo.lock', (text) =>
  text.replace(/(name = "(?:desktop-core|prime-desktop-host)"\nversion = )"[^"]+"/g, `$1"${next}"`)
)
// Only the two version fields change; `npm install --package-lock-only` would also prune optional dependencies
// that are absent from the machine cutting the release.
edit('package-lock.json', (text) => {
  const lock = JSON.parse(text)
  lock.version = lock.packages[''].version = next
  return `${JSON.stringify(lock, null, 2)}\n`
})
run('git', ['add', '-A'])
run('git', ['commit', '-m', `Release ${next}`])
run('git', ['tag', '-a', `v${next}`, '-m', `Prime Desktop ${next}`])
execFileSync('git', ['push', '--follow-tags', 'origin', 'main'], { stdio: 'inherit' })
console.log(`\nPushed v${next}. GitHub Actions is building the release.`)

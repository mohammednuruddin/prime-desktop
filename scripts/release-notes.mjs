// Prints the CHANGELOG section for a release, after checking that every file that carries the version agrees with the tag.
//   node scripts/release-notes.mjs v0.2.0
import { readFileSync } from 'node:fs'

export const VERSIONED = {
  'package.json': (text) => JSON.parse(text).version,
  'src-tauri/tauri.conf.json': (text) => JSON.parse(text).version,
  'src-tauri/Cargo.toml': (text) => /^\[package\][^[]*?^version = "([^"]+)"/ms.exec(text)?.[1],
  'src-tauri/crates/core/Cargo.toml': (text) => /^\[package\][^[]*?^version = "([^"]+)"/ms.exec(text)?.[1]
}

export function versions(read = (file) => readFileSync(file, 'utf8')) {
  return Object.fromEntries(Object.entries(VERSIONED).map(([file, pick]) => [file, pick(read(file))]))
}

/** The bullets under `## [version]`, up to the next `## `. Empty when the section is missing. */
export function section(changelog, version) {
  const lines = changelog.split('\n')
  const start = lines.findIndex((line) => new RegExp(`^## \\[${version.replace(/\./g, '\\.')}\\]`).test(line))
  if (start < 0) return ''
  const end = lines.findIndex((line, at) => at > start && line.startsWith('## '))
  return lines.slice(start + 1, end < 0 ? undefined : end).join('\n').trim()
}

if (import.meta.url === `file://${process.argv[1]}`) {
  const tag = process.argv[2] ?? ''
  const version = tag.replace(/^v/, '')
  if (!/^\d+\.\d+\.\d+(-[0-9A-Za-z.-]+)?$/.test(version)) throw new Error(`"${tag}" is not a version tag like v1.2.3`)
  const wrong = Object.entries(versions()).filter(([, found]) => found !== version)
  if (wrong.length) throw new Error(`${tag} does not match: ${wrong.map(([file, found]) => `${file} says ${found}`).join('; ')}`)
  const notes = section(readFileSync('CHANGELOG.md', 'utf8'), version)
  if (!/^\s*-/m.test(notes)) throw new Error(`CHANGELOG.md has no entries under [${version}]`)
  console.log(notes)
}

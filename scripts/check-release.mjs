import { existsSync } from 'node:fs'

if (!existsSync('LICENSE') && !existsSync('LICENSE.md') && !existsSync('COPYING')) {
  console.error('Release blocked: add a LICENSE, LICENSE.md, or COPYING file before distribution.')
  process.exit(1)
}

console.log('Release metadata check passed.')

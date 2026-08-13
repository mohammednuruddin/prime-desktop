import { chmodSync, existsSync, readdirSync } from 'node:fs'
import { join } from 'node:path'

if (process.platform !== 'win32') {
  const prebuilds = join(process.cwd(), 'node_modules', 'node-pty', 'prebuilds')
  if (existsSync(prebuilds)) {
    for (const platform of readdirSync(prebuilds)) {
      const helper = join(prebuilds, platform, 'spawn-helper')
      if (existsSync(helper)) chmodSync(helper, 0o755)
    }
  }
}

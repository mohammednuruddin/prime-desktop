import { readFile, writeFile, mkdir } from 'fs/promises'
import { homedir } from 'os'
import { join } from 'path'
import { existsSync } from 'fs'
import type { PermissionRule } from '@shared/types'

const PERM_FILE = join(homedir(), 'Library', 'Application Support', 'PrimeDesktop', 'permissions.json')

let cache: PermissionRule[] | null = null

async function load(): Promise<PermissionRule[]> {
  if (cache) return cache
  try {
    if (existsSync(PERM_FILE)) {
      cache = JSON.parse(await readFile(PERM_FILE, 'utf8')) as PermissionRule[]
    } else {
      cache = []
    }
  } catch {
    cache = []
  }
  return cache
}

async function save(rules: PermissionRule[]): Promise<void> {
  cache = rules
  await mkdir(join(PERM_FILE, '..'), { recursive: true })
  await writeFile(PERM_FILE, JSON.stringify(rules, null, 2))
}

export function matchPermission(command: string, projectPath: string | null): { decision: 'allow' | 'deny' | 'ask'; rule?: PermissionRule } {
  const rules = (cache ?? []).slice().reverse()
  for (const rule of rules) {
    if (rule.scope === 'project' && (!projectPath || rule.projectPath !== projectPath)) continue
    if (rule.pattern === '*' || command.includes(rule.pattern)) {
      return { decision: rule.action === 'deny' ? 'deny' : 'allow', rule }
    }
  }
  return { decision: 'ask' }
}

export async function listRules(): Promise<PermissionRule[]> {
  return (await load()).slice().reverse()
}

export async function addRule(rule: PermissionRule): Promise<PermissionRule[]> {
  const rules = await load()
  rules.push(rule)
  await save(rules)
  return rules.slice().reverse()
}

export async function removeRule(indexFromEnd: number): Promise<PermissionRule[]> {
  const rules = await load()
  if (!Number.isInteger(indexFromEnd) || indexFromEnd < 0 || indexFromEnd >= rules.length) {
    throw new Error('Permission rule index is out of range')
  }
  rules.splice(rules.length - 1 - indexFromEnd, 1)
  await save(rules)
  return rules.slice().reverse()
}

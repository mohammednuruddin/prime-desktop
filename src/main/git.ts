import { execFile } from 'child_process'
import { promisify } from 'util'
import { existsSync } from 'fs'
import { join } from 'path'
import type { GitChange, GitStatus } from '@shared/types'

const exec = promisify(execFile)

async function git(args: string[], cwd: string): Promise<string> {
  const { stdout } = await exec('git', args, { cwd, timeout: 20000, maxBuffer: 20 * 1024 * 1024 })
  return stdout
}

function errorText(error: unknown): string {
  const value = error as { stderr?: string; stdout?: string; message?: string }
  return String(value.stderr || value.stdout || value.message || error).trim()
}

export async function isGitRepo(dir: string): Promise<boolean> {
  try {
    await git(['rev-parse', '--is-inside-work-tree'], dir)
    return true
  } catch {
    return false
  }
}

export async function statusShort(dir: string): Promise<{ path: string; status: string }[]> {
  try {
    const out = await git(['status', '--porcelain=v1', '--untracked-files=all'], dir)
    return out
      .split('\n')
      .filter(Boolean)
      .map((line) => ({ status: line.slice(0, 2).trim(), path: line.slice(3) }))
  } catch {
    return []
  }
}

export async function gitStatus(dir: string): Promise<GitStatus> {
  if (!(await isGitRepo(dir))) {
    return { isRepo: false, branch: null, upstream: null, ahead: 0, behind: 0, changes: [] }
  }
  const [statusOutput, branch, upstream] = await Promise.all([
    git(['status', '--porcelain=v1', '-z', '--untracked-files=all'], dir),
    git(['symbolic-ref', '--quiet', '--short', 'HEAD'], dir).then((value) => value.trim()).catch(() =>
      git(['rev-parse', '--short', 'HEAD'], dir).then((value) => value.trim()).catch(() => 'HEAD')),
    git(['rev-parse', '--abbrev-ref', '--symbolic-full-name', '@{upstream}'], dir)
      .then((value) => value.trim())
      .catch(() => null)
  ])
  let ahead = 0
  let behind = 0
  if (upstream) {
    const counts = await git(['rev-list', '--left-right', '--count', `HEAD...${upstream}`], dir).catch(() => '')
    const [left, right] = counts.trim().split(/\s+/).map(Number)
    ahead = Number.isFinite(left) ? left : 0
    behind = Number.isFinite(right) ? right : 0
  }

  const records = statusOutput.split('\0')
  const changes: GitChange[] = []
  for (let index = 0; index < records.length; index++) {
    const record = records[index]
    if (!record) continue
    const indexStatus = record[0] ?? ' '
    const worktreeStatus = record[1] ?? ' '
    const path = record.slice(3)
    const renamed = indexStatus === 'R' || indexStatus === 'C' || worktreeStatus === 'R' || worktreeStatus === 'C'
    const originalPath = renamed ? records[++index] : undefined
    changes.push({
      path,
      originalPath: originalPath || undefined,
      indexStatus,
      worktreeStatus,
      staged: indexStatus !== ' ' && indexStatus !== '?',
      unstaged: worktreeStatus !== ' ' || indexStatus === '?'
    })
  }
  return { isRepo: true, branch: branch || 'HEAD', upstream, ahead, behind, changes }
}

export async function gitFileDiff(dir: string, path: string, staged: boolean): Promise<string> {
  try {
    const diff = await git(staged
      ? ['diff', '--cached', '--no-ext-diff', '--', path]
      : ['diff', '--no-ext-diff', '--', path], dir)
    if (diff || staged) return diff
    const tracked = await git(['ls-files', '--error-unmatch', '--', path], dir).then(() => true).catch(() => false)
    if (tracked) return diff
    try {
      return await git(['diff', '--no-index', '--no-ext-diff', '--', '/dev/null', path], dir)
    } catch (error) {
      return String((error as { stdout?: string }).stdout ?? '')
    }
  } catch (error) {
    throw new Error(errorText(error))
  }
}

export async function stageFiles(dir: string, paths: string[]): Promise<void> {
  if (paths.length === 0) return
  try {
    await git(['add', '--', ...paths], dir)
  } catch (error) {
    throw new Error(errorText(error))
  }
}

export async function unstageFiles(dir: string, paths: string[]): Promise<void> {
  if (paths.length === 0) return
  const hasHead = await git(['rev-parse', '--verify', 'HEAD'], dir).then(() => true).catch(() => false)
  if (!hasHead) {
    try {
      await git(['rm', '--cached', '-q', '--', ...paths], dir)
    } catch (error) {
      throw new Error(errorText(error))
    }
    return
  }
  try {
    await git(['restore', '--staged', '--', ...paths], dir)
  } catch (error) {
    throw new Error(errorText(error))
  }
}

export async function stageAll(dir: string): Promise<void> {
  try {
    await git(['add', '-A'], dir)
  } catch (error) {
    throw new Error(errorText(error))
  }
}

export async function unstageAll(dir: string): Promise<void> {
  const hasHead = await git(['rev-parse', '--verify', 'HEAD'], dir).then(() => true).catch(() => false)
  if (!hasHead) {
    try {
      await git(['rm', '--cached', '-r', '-q', '--', '.'], dir)
    } catch (error) {
      throw new Error(errorText(error))
    }
    return
  }
  try {
    await git(['reset', '-q', 'HEAD', '--', '.'], dir)
  } catch (error) {
    throw new Error(errorText(error))
  }
}

export async function commitStaged(dir: string, message: string): Promise<{ sha: string; summary: string }> {
  const trimmed = message.trim()
  if (!trimmed) throw new Error('Enter a commit message.')
  try {
    const summary = (await git(['commit', '-m', trimmed], dir)).trim()
    const sha = (await git(['rev-parse', '--short', 'HEAD'], dir)).trim()
    return { sha, summary }
  } catch (error) {
    throw new Error(errorText(error))
  }
}

export async function commitAll(dir: string, message: string): Promise<string | null> {
  if (!(await isGitRepo(dir))) return null
  try {
    await git(['add', '-A'], dir)
    await git(['commit', '-m', message, '--no-verify'], dir)
    const { stdout } = await exec('git', ['rev-parse', '--short', 'HEAD'], { cwd: dir })
    return stdout.trim()
  } catch {
    return null
  }
}

export async function changedFiles(dir: string): Promise<string[]> {
  const st = await statusShort(dir)
  return st.map((s) => s.path)
}

export async function diffForFile(dir: string, path: string): Promise<string> {
  try {
    const out = await git(['diff', '--', path], dir)
    if (out.trim()) return out
  } catch {
    /* ignore */
  }
  try {
    const out = await git(['diff', '--cached', '--', path], dir)
    return out
  } catch {
    return ''
  }
}

export async function diffAll(dir: string): Promise<string> {
  try {
    const out = await git(['diff'], dir)
    if (out.trim()) return out
  } catch {
    /* ignore */
  }
  try {
    const out = await git(['diff', '--cached'], dir)
    return out
  } catch {
    return ''
  }
}

export async function isDirty(dir: string): Promise<boolean> {
  const st = await statusShort(dir)
  return st.length > 0
}

export async function checkoutCommit(dir: string, ref: string): Promise<void> {
  await git(['checkout', ref, '--', '.'], dir)
}

export async function stashCreate(dir: string, message: string): Promise<string | null> {
  try {
    const { stdout } = await exec('git', ['stash', 'create', '-m', message], { cwd: dir })
    const sha = stdout.trim()
    return sha || null
  } catch {
    return null
  }
}

export async function gitLog(dir: string, ref: string): Promise<string | null> {
  try {
    const { stdout } = await exec('git', ['log', '--oneline', '-1', ref], { cwd: dir })
    return stdout.trim()
  } catch {
    return null
  }
}

export async function fileExistsInDir(dir: string, rel: string): Promise<boolean> {
  return existsSync(join(dir, rel))
}

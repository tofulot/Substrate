import { execFileSync } from 'node:child_process'
import { copyFile, mkdir, chmod } from 'node:fs/promises'
import { join, resolve } from 'node:path'
const root = resolve(import.meta.dirname, '..')
const executable = process.platform === 'win32' ? 'rg.exe' : 'rg'
const path = execFileSync(process.platform === 'win32' ? 'where' : 'which', [executable], { encoding: 'utf8' }).trim().split('\n')[0]
if (!path) throw new Error('Install ripgrep before packaging Substrate')
const target = join(root, 'resources/bin', executable)
await mkdir(join(root, 'resources/bin'), { recursive: true })
await copyFile(path, target)
await chmod(target, 0o755)
console.log('Prepared ripgrep for the desktop bundle')

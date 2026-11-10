import { _electron as electron, expect } from '@playwright/test'
import { mkdtemp, mkdir, writeFile, readFile, rm } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { resolve, join } from 'node:path'
import { createServer } from 'node:http'
import { execFileSync } from 'node:child_process'
const root = resolve(import.meta.dirname, '..')
const temp = await mkdtemp(join(tmpdir(), 'substrate-desktop-test-'))
const repo = join(temp, 'workspace')
await mkdir(repo)
await writeFile(join(repo, 'hello.ts'), 'export const hello = "world"\n')
execFileSync('git', ['init', '-q', repo])
execFileSync('git', ['-C', repo, 'add', '.'])
execFileSync('git', ['-C', repo, '-c', 'user.name=Fixture', '-c', 'user.email=fixture@example.invalid', 'commit', '-qm', 'Initial'])
let round = 0
const server = createServer(async (req, res) => {
  let body = ''; for await (const chunk of req) body += chunk
  if (req.url === '/v1/models') { res.writeHead(200, { 'Content-Type': 'application/json' }); res.end(JSON.stringify({ data: [{ id: 'subq-preview' }, { id: 'subq-snap-1.1' }] })); return }
  const data = JSON.parse(body)
  expect(data.model).toBe('subq-preview'); expect(data.store).toBe(false)
  if (round === 1) expect(data.messages.at(-1).content).toContain('hello.ts')
  const rounds = [
    ['search_repository', { query: 'export const' }],
    ['read_file', { path: 'hello.ts' }],
    ['apply_patch', { path: 'hello.ts', old_text: '"world"', new_text: '"substrate"' }],
    ['execute_command', { command: 'echo "Desktop integration verified"' }]
  ]
  res.writeHead(200, { 'Content-Type': 'text/event-stream' })
  if (round < rounds.length) {
    const [name, args] = rounds[round]
    res.write(`data: ${JSON.stringify({ choices: [{ delta: { tool_calls: [{ index: 0, id: `call-${round}`, function: { name, arguments: JSON.stringify(args) } }] }, finish_reason: null }] })}\n\n`)
    res.write(`data: ${JSON.stringify({ choices: [{ delta: {}, finish_reason: 'tool_calls' }] })}\n\n`)
  } else {
    for (const content of ['Updated ', '`hello.ts`', ' and verified the command.']) {
      res.write(`data: ${JSON.stringify({ choices: [{ delta: { content }, finish_reason: null }] })}\n\n`)
      await new Promise(r => setTimeout(r, 80))
    }
    res.write(`data: ${JSON.stringify({ choices: [{ delta: {}, finish_reason: 'stop' }] })}\n\n`)
  }
  round++
  res.end('data: [DONE]\n\n')
})
await new Promise(r => server.listen(0, '127.0.0.1', r))
let app
const errors = []
try {
  app = await electron.launch({ ...(process.env.SUBSTRATE_TEST_EXECUTABLE ? { executablePath: process.env.SUBSTRATE_TEST_EXECUTABLE } : {}), args: [...(process.env.SUBSTRATE_TEST_EXECUTABLE ? [] : [root]), `--user-data-dir=${join(temp, 'user')}`], env: { ...process.env, SUBSTRATE_DB: join(temp, 'sessions.db'), SUBQ_API_KEY: 'fixture-key', SUBSTRATE_API_BASE: `http://127.0.0.1:${server.address().port}/v1`, ELECTRON_RENDERER_URL: '' } })
  const page = await app.firstWindow()
  page.on('pageerror', error => errors.push(error.message))
  await expect(page.getByText('Harness ready', { exact: true })).toBeVisible()
  await expect(page.getByRole('heading', { name: 'What are we building today?' })).toBeVisible()
  await mkdir(join(root, 'test-results'), { recursive: true })
  await page.screenshot({ animations: 'disabled', path: join(root, 'test-results/welcome.png') })
  // Replace only the native file-picker with a deterministic selection; everything
  // else uses the real preload, Electron IPC, Rust binary and SQLite database.
  await app.evaluate(({ dialog }, repo) => { dialog.showOpenDialog = async () => ({ canceled: false, filePaths: [repo] }) }, repo)
  await page.getByRole('button', { name: 'Start by opening a workspace' }).click()
  await expect(page.getByText('workspace', { exact: true }).first()).toBeVisible()
  await page.getByRole('button', { name: /^Settings/ }).click()
  await page.getByRole('button', { name: 'Test connection' }).click()
  await expect(page.getByText('Connected · subq-preview is available')).toBeVisible()
  await page.getByRole('button', { name: 'Close settings' }).click()
  await page.getByRole('button', { name: 'Toggle file panel' }).click()
  await page.getByRole('button', { name: 'hello.ts' }).click()
  await expect(page.locator('.monaco-editor')).toBeVisible()
  await page.getByRole('button', { name: 'Toggle terminal' }).click()
  await expect(page.locator('.xterm-screen')).toBeVisible()
  await page.getByRole('textbox', { name: 'Message Substrate' }).fill('Update hello and verify it')
  await page.getByRole('button', { name: 'Send message' }).click()
  await page.getByRole('button', { name: 'Review', exact: true }).click()
  await expect(page.getByRole('heading', { name: 'A change worth a look.' })).toBeVisible()
  await expect(page.locator('.approval-editor .monaco-diff-editor')).toBeVisible()
  expect(await readFile(join(repo, 'hello.ts'), 'utf8')).toContain('"world"')
  await page.screenshot({ animations: 'disabled', path: join(root, 'test-results/approval.png') })
  await page.keyboard.press('Escape')
  await expect(page.getByRole('heading', { name: 'A change worth a look.' })).not.toBeVisible()
  await page.getByRole('button', { name: 'Review', exact: true }).click()
  await page.getByRole('button', { name: 'Approve edit' }).click()
  await page.getByRole('button', { name: 'Review', exact: true }).click()
  await expect(page.getByText('echo "Desktop integration verified"', { exact: true })).toBeVisible()
  await page.getByRole('button', { name: 'Approve command' }).click()
  await expect(page.getByText('All systems quiet')).toBeVisible({ timeout: 15000 })
  expect(await readFile(join(repo, 'hello.ts'), 'utf8')).toContain('"substrate"')
  await expect(page.locator('.message-assistant')).toContainText('Updated hello.ts and verified the command.')
  await expect(page.locator('.code-container .monaco-editor')).toContainText('substrate')
  await page.getByRole('button', { name: 'Back to files' }).click()
  await page.getByRole('button', { name: 'Changes', exact: false }).click()
  await page.getByRole('button', { name: 'hello.ts M' }).click()
  await expect(page.locator('.code-container .monaco-diff-editor')).toBeVisible()
  await expect(page.locator('.code-container .line-delete').first()).toBeVisible()
  await expect(page.locator('.code-container .monaco-diff-editor')).toContainText('world')
  await page.screenshot({ animations: 'disabled', path: join(root, 'test-results/workspace.png') })
  await page.reload()
  await expect(page.locator('.message-assistant')).toContainText('Updated hello.ts and verified the command.')
  // Renderer has no Node primitives, no raw IPC and no credential retrieval.
  expect(await page.evaluate(() => ({ node: typeof window.require, process: typeof window.process, exposed: Object.keys(window.substrate).sort() }))).toEqual({ node: 'undefined', process: 'undefined', exposed: ['onEvent','openProject','platform','rpc','setApiKey'] })
  const secretError = await page.evaluate(async () => { try { await window.substrate.rpc('config.credentials', { key: 'bad' }); return '' } catch (e) { return e.message } })
  expect(secretError).toContain('Unsupported operation')
  expect(errors).toEqual([])
  console.log('PASS: Electron → preload → Rust → SubQ fixture; files, approvals, Monaco diff, terminal, persistence and renderer isolation')
} finally {
  await app?.close()
  await new Promise(r => server.close(r))
  await rm(temp, { recursive: true, force: true })
}

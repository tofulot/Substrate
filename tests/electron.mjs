import { _electron as electron, expect } from '@playwright/test'
import { mkdtemp, mkdir, writeFile, readFile, realpath, stat, rm } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { resolve, join } from 'node:path'
import { createServer } from 'node:http'
import { randomUUID } from 'node:crypto'
import { execFileSync } from 'node:child_process'
const root = resolve(import.meta.dirname, '..')
const temp = await mkdtemp(join(tmpdir(), 'substrate-desktop-test-'))
const projects = join(temp, 'Documents', 'Substrate', 'Projects')
const data = join(temp, 'Documents', 'Substrate', 'Data')
const profile = join(temp, 'user')
const repo = join(temp, 'workspace')
await mkdir(repo)
await writeFile(join(repo, 'hello.ts'), 'export const hello = "world"\n')
execFileSync('git', ['init', '-q', repo])
execFileSync('git', ['-C', repo, 'add', '.'])
execFileSync('git', ['-C', repo, '-c', 'user.name=Fixture', '-c', 'user.email=fixture@example.invalid', 'commit', '-qm', 'Initial'])
await writeFile(join(repo, '.env'), 'PRIVATE_TEST_SECRET=hidden')
await writeFile(join(repo, '.gitignore'), '.env\n')
let round = 0
let snapCalls = 0
const mac = process.platform === 'darwin'
const commandCheck = (mac ? 'command -v mount >/dev/null && ' : '') + "grep -q 'export const hello = \"substrate\"' hello.ts && echo 'Desktop integration verified'"
const devRg = mac && !process.env.SUBSTRATE_TEST_EXECUTABLE
  ? process.env.SUBSTRATE_RG ?? execFileSync('which', ['rg'], { encoding: 'utf8' }).trim()
  : undefined
const fixtureKey = `fixture-${randomUUID()}`
const server = createServer(async (req, res) => {
  let body = ''; for await (const chunk of req) body += chunk
  if (req.headers.authorization !== `Bearer ${fixtureKey}`) {
    res.writeHead(401).end()
    return
  }
  if (req.url === '/v1/models') { res.writeHead(200, { 'Content-Type': 'application/json' }); res.end(JSON.stringify({ data: [{ id: 'subq-savant-1.4' }, { id: 'subq-snap-1.2' }] })); return }
  const data = JSON.parse(body)
  if (req.url === '/v1/systemone') {
    expect(data.model).toBe('subq-snap-1.2')
    expect(body).not.toContain('PRIVATE_TEST_SECRET')
    expect(Object.keys(data.questions).length).toBeLessThanOrEqual(128)
    snapCalls++
    res.writeHead(200, { 'Content-Type': 'application/json' })
    res.end(JSON.stringify({ answers: Object.fromEntries(data.state.files.map(file => [file.id, { type: 'score', score: file.path === 'hello.ts' ? 3 : 1, confidence: 0.95 }])), usage: { input_tokens: 40, output_tokens: 0 } }))
    return
  }
  expect(data.model).toBe('subq-savant-1.4'); expect(data.store).toBe(false)
  const context = data.messages[1].content
  expect(context).toContain('Repository snapshot ')
  expect(context).not.toContain('PRIVATE_TEST_SECRET')
  const source = context.split('\n').filter(line => line.startsWith('{')).map(line => JSON.parse(line)).find(file => file.path === 'hello.ts')
  expect(source.content).toContain(round >= 3 ? '"substrate"' : '"world"')
  if (round === 1) expect(data.messages.at(-1).content).toContain('hello.ts')
  if (round === 4) expect(JSON.parse(data.messages.at(-1).content)).toMatchObject({ exit_code: 0, stdout: 'Desktop integration verified\n' })
  const rounds = [
    ['search_repository', { query: 'export const' }],
    ['read_file', { path: 'hello.ts' }],
    ['apply_patch', { path: 'hello.ts', old_text: '"world"', new_text: '"substrate"' }],
    ['run_check', { command: commandCheck, purpose: 'Verify the updated export' }],
    null,
    ['apply_patch', { path: 'automatic.ts', old_text: '', new_text: 'export const applied = true\n' }],
    ['run_check', { command: "grep -qx 'export const applied = true' automatic.ts", purpose: 'Verify the automatic code edit' }],
    ['apply_patch', { path: 'automatic.ts', old_text: 'true', new_text: 'false' }]
  ]
  res.writeHead(200, { 'Content-Type': 'text/event-stream' })
  if (rounds[round]) {
    const [name, args] = rounds[round]
    res.write(`data: ${JSON.stringify({ choices: [{ delta: { tool_calls: [{ index: 0, id: `call-${round}`, function: { name, arguments: JSON.stringify(args) } }] }, finish_reason: null }] })}\n\n`)
    res.write(`data: ${JSON.stringify({ choices: [{ delta: {}, finish_reason: 'tool_calls' }] })}\n\n`)
  } else {
    const deltas = round === 4
      ? ['Updated ', '`hello.ts`', ' and verified the command.']
      : ['Applied the code edit.']
    for (const content of deltas) {
      res.write(`data: ${JSON.stringify({ choices: [{ delta: { content }, finish_reason: null }] })}\n\n`)
      await new Promise(r => setTimeout(r, 80))
    }
    res.write(`data: ${JSON.stringify({ choices: [{ delta: {}, finish_reason: 'stop' }] })}\n\n`)
  }
  if (round !== 1) res.write(`data: ${JSON.stringify({ choices: [], usage: { prompt_tokens: 2000+round, completion_tokens: 30, prompt_tokens_details: { cached_tokens: 0 } } })}\n\n`)
  round++
  res.end('data: [DONE]\n\n')
})
await new Promise(r => server.listen(0, '127.0.0.1', r))
let app
const errors = []
try {
  const executablePath = process.env.SUBSTRATE_TEST_EXECUTABLE
  // Exercise safeStorage without depending on the runner's login Keychain or its prompts.
  const launchOptions = {
    ...(executablePath ? { executablePath } : {}),
    args: [...(executablePath ? [] : [root]), ...(mac ? ['--use-mock-keychain'] : []), `--user-data-dir=${profile}`],
    env: { ...process.env, ...(mac ? { PATH: '/usr/bin:/bin', CFFIXED_USER_HOME: temp } : {}), ...(devRg ? { SUBSTRATE_RG: devRg } : {}), HOME: temp, SUBQ_API_KEY: '', SUBSTRATE_API_BASE: `http://127.0.0.1:${server.address().port}/v1`, ELECTRON_RENDERER_URL: '' },
  }
  delete launchOptions.env.SUBSTRATE_DB
  app = await electron.launch({ ...launchOptions, env: { ...launchOptions.env, SUBSTRATE_DB: join(profile, 'sessions.db') } })
  const setupPage = await app.firstWindow()
  await setupPage.getByRole('button', { name: /^Settings/ }).click()
  await setupPage.getByLabel(/^API key/).fill(fixtureKey)
  await setupPage.getByLabel('Repository context budget').selectOption('1048576')
  await expect(setupPage.getByLabel('Model turns per task')).toHaveValue('64')
  await setupPage.getByLabel('Model turns per task').fill('96')
  await setupPage.getByRole('button', { name: 'Save settings' }).click()
  await expect.poll(() => setupPage.evaluate(() => window.substrate.rpc('config.get'))).toMatchObject({
    has_key: true,
    context_budget_bytes: 1048576,
    max_steps: 96,
  })
  const encryptedKey = await readFile(join(profile, 'subq-key.enc'))
  expect(encryptedKey.includes(fixtureKey)).toBe(false)
  const legacySession = await setupPage.evaluate(project => window.substrate.rpc('session.create', { project }), repo)
  await app.close()

  app = await electron.launch(launchOptions)
  const page = await app.firstWindow()
  page.on('pageerror', error => errors.push(error.message))
  await expect(page.getByText('Harness ready', { exact: true })).toBeVisible()
  expect((await stat(join(data, 'sessions.db'))).isFile()).toBe(true)
  expect((await stat(projects)).isDirectory()).toBe(true)
  expect((await page.evaluate(() => window.substrate.rpc('session.list'))).map(session => session.id)).toContain(legacySession.id)
  const approvalMode = page.getByRole('combobox', { name: 'Approval mode' })
  await expect(approvalMode).toHaveValue('ask')
  await expect(page.getByRole('heading', { name: 'What are we building today?' })).toBeVisible()
  await mkdir(join(root, 'test-results'), { recursive: true })
  await page.screenshot({ animations: 'disabled', path: join(root, 'test-results/welcome.png') })
  // Replace only the native file-picker with a deterministic selection; everything
  // else uses the real preload, Electron IPC, Rust binary and SQLite database.
  await app.evaluate(({ dialog }, { repo, projects }) => {
    dialog.showOpenDialog = async (_window, options) => {
      if (options.defaultPath !== projects) throw new Error('Unexpected workspace picker default')
      return { canceled: false, filePaths: [repo] }
    }
  }, { repo, projects })
  await page.getByRole('button', { name: 'Start by opening a workspace' }).click()
  await expect(page.getByText('workspace', { exact: true }).first()).toBeVisible()
  // Project selection renders before the Rust session and its first snapshot finish.
  await page.waitForFunction(() => Boolean(localStorage.getItem('substrate.session')))
  const baseline = await page.evaluate(async () => (await window.substrate.rpc('context.get', { session_id: localStorage.getItem('substrate.session') })).snapshot)
  await page.getByRole('button', { name: /^Settings/ }).click()
  await expect(page.getByLabel(/^API key/)).toHaveValue('')
  await expect(page.getByLabel('Model turns per task')).toHaveValue('96')
  await expect(page.getByText('A key is securely stored', { exact: true })).toBeVisible()
  await expect(page.getByLabel('Repository context budget')).toHaveValue('1048576')
  await page.getByRole('button', { name: 'Test connection' }).click()
  await expect(page.getByText('Connected · subq-savant-1.4 is available')).toBeVisible()
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
  await expect(page.getByRole('dialog').locator('.command-preview')).toHaveText(`$ ${commandCheck}`)
  await expect(page.getByText('Verify the updated export', { exact: true })).toBeVisible()
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
  await page.getByRole('button', { name: 'Insights', exact: true }).click()
  await expect(page.getByText('Snapshot v3', { exact: true })).toBeVisible()
  await expect(page.getByText('5 model call(s).', { exact: false })).toBeVisible()
  await expect(page.getByText('8,009', { exact: true })).toBeVisible()
  await expect(page.getByText('4/5 calls', { exact: true }).first()).toBeVisible()
  const report = await page.evaluate(async () => window.substrate.rpc('usage.get', { session_id: localStorage.getItem('substrate.session') }))
  expect(report.calls.length).toBe(5)
  expect(snapCalls).toBe(0)
  expect(report.calls.filter(call => call.kind === 'context_selection').length).toBe(0)
  await expect(page.getByText('Local task selection', { exact: true })).toBeVisible()
  expect(report.calls.filter(call => call.metrics.usage === null).length).toBe(1)
  await writeFile(join(repo, 'hello.ts'), 'export const hello = "substrate" // external\n')
  await expect(page.getByText('Workspace changed', { exact: true })).toBeVisible({ timeout: 15000 })
  await page.getByRole('button', { name: 'Refresh context snapshot' }).click()
  await expect(page.getByText('Snapshot v4', { exact: true })).toBeVisible()
  await expect(page.getByText('Matches workspace', { exact: true })).toBeVisible()
  await page.screenshot({ animations: 'disabled', path: join(root, 'test-results/insights.png') })
  await page.getByLabel('Context snapshot version').selectOption(baseline.id)
  await expect(page.getByText('Snapshot v1', { exact: true })).toBeVisible()
  await page.getByRole('button', { name: 'hello.ts', exact: false }).last().click()
  await expect(page.locator('.code-container .monaco-diff-editor')).toContainText('world')
  await expect(page.locator('.code-container .monaco-diff-editor')).toContainText('external')
  await page.reload()
  await expect(page.locator('.message-assistant')).toContainText('Updated hello.ts and verified the command.')
  await page.getByRole('button', { name: 'Toggle file panel' }).click()
  await page.getByRole('button', { name: 'Insights', exact: true }).click()
  await expect(page.getByText('Snapshot v4', { exact: true })).toBeVisible()
  await expect(page.getByText('8,009', { exact: true })).toBeVisible()
  // Renderer has no Node primitives, no raw IPC and no credential retrieval.
  expect(await page.evaluate(() => ({ node: typeof window.require, process: typeof window.process, exposed: Object.keys(window.substrate).sort() }))).toEqual({
    node: 'undefined', process: 'undefined',
    exposed: ['createProject', 'onEvent', 'openProject', 'platform', 'rpc', 'setApiKey']
  })
  await expect(page.evaluate(() => window.substrate.rpc('config.credentials', { key: 'bad' }))).rejects.toThrow(/Unsupported operation/)
  await page.getByRole('textbox', { name: 'Message Substrate' }).fill('Apply a code edit automatically')
  await page.getByRole('button', { name: 'Send message' }).click()
  await expect(page.getByText(/^Waiting for your approval/)).toBeVisible()
  await expect(approvalMode).toBeEnabled()
  expect(await stat(join(repo, 'automatic.ts')).catch(error => error.code)).toBe('ENOENT')
  await approvalMode.selectOption({ label: 'Approve all code edits' })
  await expect(approvalMode).toHaveValue('approve_edits')
  await expect.poll(() => readFile(join(repo, 'automatic.ts'), 'utf8').catch(() => '')).toBe('export const applied = true\n')
  await page.getByRole('button', { name: 'Review', exact: true }).click()
  await expect(page.getByRole('button', { name: 'Approve command' })).toBeVisible()
  await page.reload()
  await expect(approvalMode).toHaveValue('approve_edits')
  await approvalMode.selectOption({ label: 'Ask before acting' })
  await expect(approvalMode).toHaveValue('ask')
  await page.getByRole('button', { name: 'Review', exact: true }).click()
  await page.getByRole('button', { name: 'Approve command' }).click()
  await page.getByRole('button', { name: 'Review', exact: true }).click()
  await expect(page.getByRole('button', { name: 'Approve edit' })).toBeVisible()
  expect(await readFile(join(repo, 'automatic.ts'), 'utf8')).toBe('export const applied = true\n')
  await page.getByRole('button', { name: 'Decline', exact: true }).click()
  await expect(page.locator('.message-assistant').last()).toContainText('Applied the code edit.')
  expect(await readFile(join(repo, 'automatic.ts'), 'utf8')).toBe('export const applied = true\n')
  await expect(approvalMode).toBeEnabled()
  const newFolder = page.getByRole('button', { name: 'New folder' })
  const folderDialog = page.getByRole('dialog', { name: 'New workspace folder' })
  const folderName = folderDialog.getByLabel('Folder name')
  const createFolder = folderDialog.getByRole('button', { name: 'Create folder' })
  await newFolder.click()
  await folderName.fill('../outside')
  await createFolder.click()
  await expect(folderDialog.getByRole('alert')).toContainText('Enter a folder name')
  await folderName.fill('A new project')
  await createFolder.click()
  await expect(folderDialog).toBeHidden()
  const newProject = await realpath(join(projects, 'A new project'))
  expect((await stat(newProject)).isDirectory()).toBe(true)
  await expect(page.locator('.workspace-card')).toContainText('A new project')
  const newSession = await page.evaluate(() => window.substrate.rpc('session.resume', { session_id: localStorage.getItem('substrate.session') }))
  expect(newSession.project).toBe(newProject)
  const history = page.getByRole('navigation', { name: 'Chat history' })
  const newProjectChats = history.getByRole('list', { name: 'A new project chats' })
  const originalChats = history.getByRole('list', { name: 'workspace chats' })
  const originalChat = originalChats.getByRole('button', { name: 'Update hello and verify it' })
  await expect(newProjectChats.getByRole('button', { name: 'New conversation' })).toHaveAttribute('aria-current', 'true')
  await expect(originalChat).toBeVisible()
  await page.getByRole('button', { name: 'New conversation' }).first().click()
  await expect(newProjectChats.getByRole('button', { name: 'New conversation' })).toHaveCount(2)
  await page.getByRole('textbox', { name: 'Message Substrate' }).fill('Draft for the new project')
  await history.getByRole('button', { name: 'workspace', exact: true }).click()
  await expect(originalChat).toBeHidden()
  await page.getByRole('button', { name: 'Search chat history' }).click()
  const historySearch = page.getByRole('searchbox', { name: 'Search by project or chat title' })
  await historySearch.fill('A new project')
  await expect(newProjectChats.getByRole('button')).toHaveCount(2)
  await expect(originalChats).toBeHidden()
  await historySearch.fill('verify it')
  await expect(newProjectChats).toBeHidden()
  await originalChat.click()
  await expect(originalChat).toHaveAttribute('aria-current', 'true')
  await expect(page.locator('.message-assistant').last()).toContainText('Applied the code edit.')
  await expect(page.getByRole('textbox', { name: 'Message Substrate' })).toHaveValue('')
  await page.getByRole('button', { name: 'Search chat history' }).click()
  await newProjectChats.getByRole('button', { name: 'New conversation' }).first().click()
  await expect(page.getByRole('textbox', { name: 'Message Substrate' })).toHaveValue('Draft for the new project')
  await newProjectChats.getByRole('button', { name: 'New conversation' }).last().click()
  await expect.poll(() => page.evaluate(() => localStorage.getItem('substrate.session'))).toBe(newSession.id)
  await newFolder.click()
  await folderName.fill('A new project')
  await createFolder.click()
  await expect(folderDialog.getByRole('alert')).toContainText('already exists')
  await folderDialog.getByRole('button', { name: 'Cancel' }).click()
  expect(errors).toEqual([])
  await app.close()
  app = await electron.launch(launchOptions)
  const reopened = await app.firstWindow()
  await expect(reopened.locator('.workspace-card')).toContainText('A new project')
  const reopenedHistory = reopened.getByRole('navigation', { name: 'Chat history' })
  await expect(reopenedHistory.getByRole('list', { name: 'A new project chats' }).getByRole('button')).toHaveCount(2)
  await expect(reopenedHistory.getByRole('list', { name: 'workspace chats' }).getByRole('button', { name: 'Update hello and verify it' })).toBeVisible()
  const restoredSessionId = await reopened.evaluate(() => localStorage.getItem('substrate.session'))
  expect(restoredSessionId).toBe(newSession.id)
  console.log('PASS: Electron → preload → Rust → SubQ fixture; project chat history, folder creation, migration, files, approvals, Monaco diff, terminal, persistence and renderer isolation')
} finally {
  await app?.close()
  await new Promise(r => server.close(r))
  await rm(temp, { recursive: true, force: true })
}

import { afterAll, expect, test } from 'bun:test'
import { mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join, resolve } from 'node:path'

const directory = await mkdtemp(join(tmpdir(), 'appa-setup-test-'))
afterAll(() => rm(directory, { recursive: true, force: true }))

test('source setup preserves project settings and policy, is repeatable, and refuses conflicting ownership', async () => {
  const project = join(directory, "project with spaces' $(touch APPA_SETUP_INJECTION)")
  const config = join(directory, 'deployment/appa.toml')
  await mkdir(project)
  await writeFile(join(project, 'opencode.json'), JSON.stringify({
    model: 'existing/model', plugins: ['existing-plugin', import.meta.dir],
    commands: { existing: { template: 'Keep this command' } },
    mcp: { servers: { other: { type: 'remote', url: 'http://127.0.0.1:9999/mcp' },
      appa: { type: 'remote', url: 'http://localhost:9911/mcp', timeout: 12345 } } },
  }))
  async function install() {
    const process = Bun.spawn(['bun', resolve(import.meta.dir, 'setup.ts'), '--project', project,
      '--config', config, '--runtime-url', 'http://localhost:9911', '--install-only'], { stdout: 'pipe', stderr: 'pipe' })
    return { code: await process.exited, error: await new Response(process.stderr).text() }
  }
  expect(await install()).toMatchObject({ code: 0 })
  const settings = JSON.parse(await readFile(join(project, 'opencode.json'), 'utf8'))
  expect(settings.model).toBe('existing/model')
  expect(settings.plugins).toContain('existing-plugin')
  expect(settings.commands.existing.template).toBe('Keep this command')
  expect(settings.commands['appa-guide'].template).toContain('project with spaces')
  expect(settings.mcp.servers.other.url).toBe('http://127.0.0.1:9999/mcp')
  expect(settings.mcp.servers.appa.url).toBe('http://localhost:9911/mcp')
  expect(settings.mcp.servers.appa.timeout).toBe(12345)
  expect(settings.plugins.find((plugin: any) => plugin.package === import.meta.dir).options.runtimeURL).toBe('http://localhost:9911')
  expect(await readFile(config, 'utf8')).toContain('batteries/opencode/appa.toml')
  const custom = `${await readFile(config, 'utf8')}\n# Operator customization\n`
  await writeFile(config, custom)
  expect(await install()).toMatchObject({ code: 0 })
  expect(await readFile(config, 'utf8')).toBe(custom)
  expect(JSON.parse(await readFile(join(project, 'opencode.json'), 'utf8'))).toEqual(settings)
  const command = settings.commands['appa-guide'].template.split('run: ')[1].split('. Do not edit')[0]
  const copied = Bun.spawn(['bash', '-c', `${command} --install-only`], { cwd: directory, stdout: 'ignore', stderr: 'pipe' })
  expect(await copied.exited).toBe(0)
  expect(await Bun.file(join(directory, 'APPA_SETUP_INJECTION')).exists()).toBe(false)
  settings.commands['appa-guide'].template = 'User-owned command'
  const conflicting = JSON.stringify(settings)
  await writeFile(join(project, 'opencode.json'), conflicting)
  expect(await install()).toMatchObject({ code: 1, error: expect.stringContaining('not owned') })
  expect(await readFile(join(project, 'opencode.json'), 'utf8')).toBe(conflicting)
  expect(await readFile(config, 'utf8')).toBe(custom)
}, 20_000)

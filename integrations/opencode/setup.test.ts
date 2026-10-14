import { afterAll, expect, test } from 'bun:test'
import { mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join, resolve } from 'node:path'

const directory = await mkdtemp(join(tmpdir(), 'appa-setup-test-'))
afterAll(() => rm(directory, { recursive: true, force: true }))

test('source setup is user-global across projects, preserves settings and policy, and refuses conflicting ownership', async () => {
  const project = join(directory, "project with spaces' $(touch APPA_SETUP_INJECTION)")
  const other = join(directory, 'another project')
  const configHome = join(directory, "user config's $(touch APPA_SETUP_INJECTION)")
  const settingsPath = join(configHome, 'opencode/opencode.json')
  const config = join(configHome, 'appa/opencode/appa.toml')
  const env = { ...process.env, HOME: join(directory, 'home'), XDG_CONFIG_HOME: configHome, OPENCODE_CONFIG_DIR: undefined }
  await mkdir(project)
  await mkdir(other)
  await mkdir(join(configHome, 'opencode'), { recursive: true })
  const projectSettings = '{"model":"project/model"}\n'
  await writeFile(join(project, 'opencode.json'), projectSettings)
  await writeFile(settingsPath, JSON.stringify({
    model: 'existing/model', plugins: ['existing-plugin', import.meta.dir],
    commands: { existing: { template: 'Keep this command' } },
    mcp: { servers: { other: { type: 'remote', url: 'http://127.0.0.1:9999/mcp' },
      appa: { type: 'remote', url: 'http://localhost:9911/mcp', timeout: 12345 } } },
  }))
  async function install(cwd = project) {
    const process = Bun.spawn(['bun', resolve(import.meta.dir, 'setup.ts'),
      '--runtime-url', 'http://localhost:9911', '--install-only'], { cwd, env, stdout: 'pipe', stderr: 'pipe' })
    return { code: await process.exited, error: await new Response(process.stderr).text() }
  }
  expect(await install()).toMatchObject({ code: 0 })
  const settings = JSON.parse(await readFile(settingsPath, 'utf8'))
  expect(settings.model).toBe('existing/model')
  expect(settings.plugins).toContain('existing-plugin')
  expect(settings.commands.existing.template).toBe('Keep this command')
  expect(settings.commands['appa-guide'].template).not.toContain('--project')
  expect(await readFile(join(project, 'opencode.json'), 'utf8')).toBe(projectSettings)
  expect(settings.mcp.servers.other.url).toBe('http://127.0.0.1:9999/mcp')
  expect(settings.mcp.servers.appa.url).toBe('http://localhost:9911/mcp')
  expect(settings.mcp.servers.appa.timeout).toBe(12345)
  expect(settings.plugins.find((plugin: any) => plugin.package === import.meta.dir).options.runtimeURL).toBe('http://localhost:9911')
  expect(await readFile(config, 'utf8')).toContain('batteries/opencode/appa.toml')
  const custom = `${await readFile(config, 'utf8')}\n# Operator customization\n`
  await writeFile(config, custom)
  expect(await install(other)).toMatchObject({ code: 0 })
  expect(await readFile(config, 'utf8')).toBe(custom)
  expect(JSON.parse(await readFile(settingsPath, 'utf8'))).toEqual(settings)
  expect(await Bun.file(join(other, 'opencode.json')).exists()).toBe(false)
  const command = settings.commands['appa-guide'].template.split('run: ')[1].split('. Do not edit')[0]
  const copied = Bun.spawn(['bash', '-c', `${command} --install-only`], { cwd: directory, env, stdout: 'ignore', stderr: 'pipe' })
  expect(await copied.exited).toBe(0)
  expect(await Bun.file(join(directory, 'APPA_SETUP_INJECTION')).exists()).toBe(false)
  settings.commands['appa-guide'].template = 'User-owned command'
  const conflicting = JSON.stringify(settings)
  await writeFile(settingsPath, conflicting)
  expect(await install()).toMatchObject({ code: 1, error: expect.stringContaining('not owned') })
  expect(await readFile(settingsPath, 'utf8')).toBe(conflicting)
  expect(await readFile(config, 'utf8')).toBe(custom)
}, 20_000)

test('source setup respects the OpenCode global directory override and refuses to shadow its JSONC', async () => {
  const global = join(directory, 'custom-opencode')
  await mkdir(global)
  const env = { ...process.env, XDG_CONFIG_HOME: join(directory, 'custom-xdg'), OPENCODE_CONFIG_DIR: global }
  const jsonc = '// Keep operator comments\n{}\n'
  await writeFile(join(global, 'opencode.jsonc'), jsonc)
  const args = ['bun', resolve(import.meta.dir, 'setup.ts'), '--install-only']
  const refused = Bun.spawn(args, { cwd: directory, env, stdout: 'ignore', stderr: 'pipe' })
  expect(await refused.exited).toBe(1)
  expect(await new Response(refused.stderr).text()).toContain('opencode.jsonc')
  expect(await readFile(join(global, 'opencode.jsonc'), 'utf8')).toBe(jsonc)
  expect(await Bun.file(join(global, 'opencode.json')).exists()).toBe(false)
  await rm(join(global, 'opencode.jsonc'))
  const installed = Bun.spawn(args, { cwd: directory, env, stdout: 'ignore', stderr: 'pipe' })
  expect(await installed.exited).toBe(0)
  expect(JSON.parse(await readFile(join(global, 'opencode.json'), 'utf8')).plugins).toHaveLength(1)
  expect(await Bun.file(join(directory, 'custom-xdg/opencode/opencode.json')).exists()).toBe(false)
})

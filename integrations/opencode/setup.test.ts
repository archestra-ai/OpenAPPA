import { afterAll, expect, test } from 'bun:test'
import { mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join, resolve } from 'node:path'
import { configuration, guide } from './setup'

const directory = await mkdtemp(join(tmpdir(), 'appa-setup-test-'))
afterAll(() => rm(directory, { recursive: true, force: true }))

test('setup reads the installer catalog, filters host compatibility, and keeps deployment-relative includes', async () => {
  const catalog = join(directory, 'catalog')
  const config = join(directory, 'deployment/appa.toml')
  await mkdir(join(directory, 'deployment'), { recursive: true })
  await writeFile(config, '[policy]\nversion = 2\n')
  for (const [name, hosts] of [['available', ['opencode']], ['incompatible', ['claude-code']]] as const) {
    await mkdir(join(catalog, name), { recursive: true })
    await writeFile(join(catalog, name, 'appa-package.toml'), `description = "Catalog entry"\n[battery]\nhosts = ${JSON.stringify(hosts)}\n`)
    await writeFile(join(catalog, name, 'appa.toml'), '# Catalog policy\n')
    await writeFile(join(catalog, name, 'README.md'), 'Catalog documentation')
  }
  const previous = process.env.APPA_OPENCODE_BATTERIES
  process.env.APPA_OPENCODE_BATTERIES = catalog
  try {
    const setup = await configuration(config, '/unused/appa')
    const battery = JSON.parse((await setup({ operation: 'battery', battery: 'available' }, []))!)
    expect(battery.path).toBe('batteries/available/appa.toml')
    expect(battery.files['README.md']).toBe('Catalog documentation')
    expect(battery.contracts).toContain('contract')
    await expect(setup({ operation: 'battery', battery: 'incompatible' }, [])).rejects.toThrow('catalog')
    for (const status of ['pending', 'failed', 'needs_auth'] as const) {
      await expect(setup({ operation: 'publish', policy: '[policy]\nversion = 2\n', expected_digest: 'unused' }, [],
        [{ name: 'project-only', status }])).rejects.toThrow(`project-only (${status})`)
      expect(await readFile(config, 'utf8')).toBe('[policy]\nversion = 2\n')
    }
    expect(await guide()).toContain('OpenCode')
  } finally {
    if (previous === undefined) delete process.env.APPA_OPENCODE_BATTERIES
    else process.env.APPA_OPENCODE_BATTERIES = previous
  }
})

test('service batteries expose the policy selected for the deployment, not just the base', async () => {
  const config = join(directory, 'catalog-policy.toml')
  await writeFile(config, '[policy]\nversion = 2\n')
  const previous = { store: process.env.APPA_OPENCODE_BATTERIES, target: process.env.APPA_BATTERY_TARGET }
  process.env.APPA_OPENCODE_BATTERIES = resolve(import.meta.dir, '../../marketplace/batteries')
  try {
    for (const target of ['single-user', 'multi-user']) {
      process.env.APPA_BATTERY_TARGET = target
      const setup = await configuration(config, '/unused/appa')
      for (const name of ['cloudflare', 'notion', 'github']) {
        const battery = JSON.parse((await setup({ operation: 'battery', battery: name }, []))!)
        expect(battery.deployment_target).toBe(target)
        expect(battery.path).toBe(`batteries/${name}/appa.toml`)
        if (name === 'github') {
          expect(battery.target_policy).toBe(`${target}.toml`)
          expect(battery.files[`${target}.toml`]).toContain('github.repository-visibility')
          expect(battery.files[`${target}.toml`]).toContain(target === 'multi-user'
            ? 'repository-visibility-multi-user.py' : 'repository_visibility.py')
        }
      }
      await expect(setup({ operation: 'battery', battery: 'posthog' }, [])).rejects.toThrow('catalog')
    }
  } finally {
    if (previous.store === undefined) delete process.env.APPA_OPENCODE_BATTERIES
    else process.env.APPA_OPENCODE_BATTERIES = previous.store
    if (previous.target === undefined) delete process.env.APPA_BATTERY_TARGET
    else process.env.APPA_BATTERY_TARGET = previous.target
  }
})

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

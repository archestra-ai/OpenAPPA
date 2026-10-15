import { createHash } from 'node:crypto'
import { lstat, mkdir, mkdtemp, readFile, readdir, realpath, rename, rm, symlink, writeFile } from 'node:fs/promises'
import { tmpdir, homedir } from 'node:os'
import { dirname, isAbsolute, join, resolve } from 'node:path'
import { parseArgs } from 'node:util'
import { existsSync } from 'node:fs'
import type { Mcp } from '@opencode/schema/mcp'

const checkout = resolve(import.meta.dir, '../..')
const batteries = join(checkout, 'marketplace/batteries')
const assets = existsSync(join(import.meta.dir, 'guide/SKILL.md')) ? import.meta.dir : undefined
const digest = (text: string) => createHash('sha256').update(text).digest('hex')
const shellQuote = (text: string) => `'${text.replaceAll("'", "'\\''")}'`

export type SetupInput = {
  operation: 'inspect' | 'battery' | 'publish'
  battery?: string
  policy?: string
  expected_digest?: string
}

export async function guide() {
  const root = assets ? join(assets, 'guide') : join(checkout, 'integrations/appa-guide')
  return (await Promise.all(['SKILL.md', 'references/core.md', 'references/opencode.md']
    .map(path => readFile(join(root, path), 'utf8')))).join('\n\n')
}

// Only an operator-launched private setup server receives this capability.
// The target path and executable come from its environment, never tool input.
export async function configuration(config: string, binary: string) {
  if (!isAbsolute(config) || !isAbsolute(binary)) throw new Error('Setup requires absolute config and appa paths.')
  if (!(await lstat(config)).isFile()) throw new Error('Setup config must be a regular file, not a symlink.')
  const target = process.env.APPA_BATTERY_TARGET ?? 'single-user'
  if (!['single-user', 'multi-user'].includes(target)) throw new Error('APPA_BATTERY_TARGET must be single-user or multi-user.')
  const store = process.env.APPA_OPENCODE_BATTERIES || join(dirname(config), 'batteries')
  const catalog = await Promise.all((await readdir(store)).sort().filter(name => !name.includes('.')).map(async name => {
    const manifest = Bun.TOML.parse(await readFile(join(store, name, 'appa-package.toml'), 'utf8')) as any
    return { name, description: manifest.description, hosts: manifest.battery.hosts,
      namespaces: manifest.battery.namespaces ?? [], path: `batteries/${name}/appa.toml`,
      target_policy: manifest.battery.targets?.[target]?.policy }
  })).then(items => items.filter(item => item.hosts.includes('opencode')))
  let publishing = false
  async function describe(path: string, tools: string[] = []) {
    const process = Bun.spawn([binary, 'describe', '--adapter', 'opencode', '--config', path, '--check',
      ...(tools.length ? ['--session-tools', tools.join(',')] : [])], { stdout: 'pipe', stderr: 'pipe' })
    const [stdout, stderr, code] = await Promise.all([new Response(process.stdout).text(), new Response(process.stderr).text(), process.exited])
    if (code !== 0) throw new Error(`Policy validation refused: ${stderr}\n${stdout}`)
    return stdout
  }
  async function documents(path: string, found = new Map<string, string>()) {
    if (found.has(path)) return found
    const text = await readFile(path, 'utf8')
    found.set(path, text)
    const includes = (Bun.TOML.parse(text) as { include?: string[] }).include ?? []
    for (const include of includes) await documents(resolve(dirname(path), include), found)
    return found
  }
  return async (input: SetupInput, tools: { name: string; description: string; canonical: string }[],
    servers: { name: string; status: Mcp.Status['status'] }[] = []) => {
    // The public host API has no acknowledgement binding MCP discovery to the
    // registry. Even connected servers do not prove this is a complete catalog.
    const inventory = { scope: 'observed', servers,
      note: 'Tools are a live registry observation, not proof of complete MCP discovery or policy coverage. Connected servers can still have registry updates pending. Disabled servers are excluded; resolve pending, failed, or needs_auth servers and inspect again before proposing policy.' }
    switch (input.operation) {
      case 'inspect': {
        const policy = await readFile(config, 'utf8')
        return JSON.stringify({ config, policy, digest: digest(policy), deployment_target: target,
          description: await describe(config, tools.map(tool => tool.canonical)),
          documents: Object.fromEntries(await documents(config)), tools, inventory, batteries: catalog })
      }
      case 'battery': {
        const battery = catalog.find(item => item.name === input.battery)
        if (!battery) throw new Error('Choose a battery from the inspected catalog.')
        return JSON.stringify({ ...battery, deployment_target: target, files: Object.fromEntries(await Promise.all(
          ['appa-package.toml', 'appa.toml', 'README.md', ...(battery.target_policy ? [battery.target_policy] : [])]
            .map(async name => [name, await readFile(join(store, battery.name, name), 'utf8')]))),
          contracts: await readFile(assets ? join(assets, 'contracts.md') : join(checkout, 'website/content/docs/contracts.md'), 'utf8') })
      }
      case 'publish': {
        if (!input.policy || !input.expected_digest) throw new Error('Publish requires the approved complete policy and inspected digest.')
        const unavailable = servers.filter(server => server.status !== 'connected' && server.status !== 'disabled')
        if (unavailable.length) throw new Error(`MCP discovery is unresolved: ${unavailable.map(server => `${server.name} (${server.status})`).join(', ')}. Resolve these connections and inspect again before publishing.`)
        if (publishing) throw new Error('Another publish is in progress; inspect again.')
        publishing = true
        const candidate = join(dirname(config), `.appa-setup-${crypto.randomUUID()}.toml`)
        try {
          const previous = await readFile(config, 'utf8')
          if (digest(previous) !== input.expected_digest) throw new Error('Policy changed; inspect and propose again.')
          if (previous === input.policy) return 'Policy unchanged. No new session is needed.'
          await writeFile(candidate, input.policy, { mode: 0o600, flag: 'wx' })
          const description = await describe(candidate, tools.map(tool => tool.canonical))
          if (digest(await readFile(config, 'utf8')) !== input.expected_digest) throw new Error('Policy changed during validation; inspect and propose again.')
          await rename(candidate, config)
          return JSON.stringify({ state: 'saved', description, inventory,
            next: 'Close setup. Restart the protected runtime and start a fresh OpenCode session. Existing sessions retain their opening policy.' })
        } finally {
          publishing = false
          await rm(candidate, { force: true })
        }
      }
    }
  }
}

export async function install(settingsDirectory: string, config: string, runtimeURL: string, binary = join(checkout, 'target/debug/appa')) {
  const path = join(settingsDirectory, 'opencode.json')
  // JSONC is not JSON. Never shadow a configuration we cannot safely merge.
  if (await Bun.file(join(settingsDirectory, 'opencode.jsonc')).exists()) throw new Error('Merge AppenCode into global opencode.jsonc manually before setup; do not shadow it with opencode.json.')
  const settings = await Bun.file(path).exists() ? await Bun.file(path).json() : {}
  const plugins = settings.plugins ?? []
  const plugin = plugins.find((plugin: any) => (typeof plugin === 'string' ? plugin : plugin.package) === import.meta.dir)
  if (plugin && typeof plugin !== 'string' && plugin.options?.runtimeURL && plugin.options.runtimeURL !== runtimeURL) throw new Error('Existing AppenCode plugin names a different runtime.')
  const entry = { ...(typeof plugin === 'object' ? plugin : {}), package: import.meta.dir,
    options: { ...(typeof plugin === 'object' ? plugin.options : {}), runtimeURL } }
  if (plugin) plugins[plugins.indexOf(plugin)] = entry
  else plugins.push(entry)
  if (settings.mcp?.servers?.appa && settings.mcp.servers.appa.url !== `${runtimeURL}/mcp`) throw new Error('Existing appa MCP connection names a different runtime; reconcile it before setup.')
  const command = `AppenCode policy setup runs in a separate operator session. From the project to inspect, ask the operator to run: bun run ${shellQuote(join(import.meta.dir, 'setup.ts'))} --config ${shellQuote(config)} --binary ${shellQuote(binary)} --runtime-url ${shellQuote(runtimeURL)}. Do not edit APPA policy from this coding session.`
  settings.plugins = plugins
  settings.mcp = { ...settings.mcp, servers: { ...settings.mcp?.servers, appa: { ...settings.mcp?.servers?.appa, type: 'remote', url: `${runtimeURL}/mcp`, oauth: false } } }
  if (settings.commands?.['appa-guide'] && settings.commands['appa-guide'].template !== command) throw new Error('Existing /appa-guide command is not owned by this setup; preserve it and resolve the conflict.')
  settings.commands = { ...settings.commands, 'appa-guide': { description: 'Set up or tune AppenCode policy', template: command } }
  await mkdir(dirname(config), { recursive: true })
  const store = join(dirname(config), 'batteries')
  try { await symlink(batteries, store, 'dir') }
  catch (error) {
    if ((error as NodeJS.ErrnoException).code !== 'EEXIST') throw error
    if (await realpath(store) !== await realpath(batteries)) throw new Error('Existing battery store belongs to another deployment; choose a separate config directory.')
  }
  const initial = 'include = ["batteries/opencode/appa.toml"]\n\n[policy]\nversion = 2\n\n[policy.deployment]\ncontext_control = true\n\n[externals]\ntimeout_ms = 2000\nmax_body_bytes = 65536\n'
  try { await writeFile(config, initial, { flag: 'wx', mode: 0o600 }) }
  catch (error) { if ((error as NodeJS.ErrnoException).code !== 'EEXIST') throw error }
  await mkdir(settingsDirectory, { recursive: true })
  await writeFile(path, `${JSON.stringify(settings, null, 2)}\n`)
}

if (import.meta.main) {
  const configHome = process.env.XDG_CONFIG_HOME || join(homedir(), '.config')
  const settingsDirectory = resolve(process.env.OPENCODE_CONFIG_DIR || join(configHome, 'opencode'))
  const { values } = parseArgs({ options: {
    project: { type: 'string', default: process.cwd() },
    config: { type: 'string', default: join(configHome, 'appa/opencode/appa.toml') },
    binary: { type: 'string', default: join(checkout, 'target/debug/appa') },
    'runtime-url': { type: 'string', default: 'http://127.0.0.1:8789' },
    'install-only': { type: 'boolean', default: false },
  } })
  const project = resolve(values.project!), config = resolve(values.config!), binary = resolve(values.binary!)
  const url = new URL(values['runtime-url']!)
  if (url.protocol !== 'http:' || !['127.0.0.1', 'localhost', '[::1]'].includes(url.hostname) || url.username || url.password || url.pathname !== '/') throw new Error('Runtime URL must be an unauthenticated loopback HTTP origin.')
  const runtimeURL = url.origin
  await install(settingsDirectory, config, runtimeURL, binary)
  if (!values['install-only']) {
    const temporary = await mkdtemp(join(tmpdir(), 'appa-opencode-setup-'))
    const reservation = Bun.serve({ hostname: '127.0.0.1', port: 0, fetch: () => new Response() })
    const port = reservation.port!
    reservation.stop(true)
    await writeFile(join(temporary, 'appa.toml'), '[policy]\nversion = 2\n[[policy.tool]]\nname = "host/opencode/appa_setup"\n[externals]\ntimeout_ms = 2000\nmax_body_bytes = 65536\n')
    const runtime = Bun.spawn([binary, 'runtime', '--adapter', 'opencode', '--listen', `127.0.0.1:${port}`,
      '--config', join(temporary, 'appa.toml'), '--db', join(temporary, 'appa.db')],
    { stdout: 'ignore', stderr: Bun.file(join(temporary, 'runtime.log')) })
    try {
      let ready = false
      for (let attempt = 0; attempt < 200 && runtime.exitCode === null; attempt++) {
        try { if ((await fetch(`http://127.0.0.1:${port}/health`)).ok) { ready = true; break } } catch {}
        await Bun.sleep(50)
      }
      if (!ready) throw new Error(`Setup runtime failed: ${await readFile(join(temporary, 'runtime.log'), 'utf8')}`)
      // Keep normal global and project discovery. Inline overrides take precedence
      // without copying project settings or pinning the global guide to one project.
      const setupSettings = JSON.stringify({
        commands: { 'appa-guide': { template: await guide() } },
        mcp: { servers: { appa: { type: 'remote', url: `http://127.0.0.1:${port}/mcp`, oauth: false } } } })
      console.log('Operator setup session: configuration access only. Review the proposal before approving. Close it before protected coding.')
      const session = Bun.spawn([join(import.meta.dir, 'node_modules/.bin/opencode'), '--standalone', project,
        '--prompt', 'Run /appa-guide init. Inspect and propose; wait for my approval before publishing.'],
      { stdin: 'inherit', stdout: 'inherit', stderr: 'inherit', env: { ...process.env,
        OPENCODE_CONFIG_PROJECT_DISABLE: '', OPENCODE_CONFIG_CONTENT: setupSettings,
        APPA_OPENCODE_SETUP_CONFIG: config, APPA_OPENCODE_SETUP_BINARY: binary, APPA_OPENCODE_RUNTIME_URL: `http://127.0.0.1:${port}` } })
      const stop = () => { session.kill() }
      process.once('SIGINT', stop)
      process.once('SIGTERM', stop)
      try { process.exitCode = await session.exited }
      finally {
        process.off('SIGINT', stop)
        process.off('SIGTERM', stop)
      }
    } finally {
      runtime.kill()
      await runtime.exited
      await rm(temporary, { recursive: true, force: true })
    }
  }
  console.log(`Protected runtime: ${shellQuote(binary)} runtime --adapter opencode --listen ${shellQuote(url.host)} --config ${shellQuote(config)} --db ${shellQuote(join(dirname(config), 'appa.db'))}`)
  console.log('Restart that runtime if already running, then start a fresh OpenCode session in the project.')
}

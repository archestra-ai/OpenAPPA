import { afterAll, afterEach, beforeAll, describe, expect, test } from 'bun:test'
import { Effect } from 'effect'
import type { Context } from '@opencode/plugin/effect/plugin'
import { Tool } from '@opencode/schema/tool'
import { mkdir, mkdtemp, readFile, rm, symlink, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { resolve } from 'node:path'
import appencode from './appencode'

// Drive the exported plugin with the published V2 SDK and host-shaped events.
async function host(url: string, directory: string, sessionID = 'ses_test', session: object = {}) {
  process.env.APPA_OPENCODE_RUNTIME_URL = url
  const hooks = new Map<string, Function>()
  const servers: any[] = [{ name: 'appa', status: { status: 'connected' } }]
  const tools = new Map<string, any>([
    ...['read', 'write', 'edit', 'webfetch', 'websearch', 'shell', 'subagent', 'read_credentials', 'unknown'].map(name =>
      [name, { id: name, name, options: { codemode: true, pinned: true } }] as [string, any]),
    ['appa_execute_remedy_plan', {
      id: 'appa_execute_remedy_plan', name: 'execute_remedy_plan', options: { namespace: 'appa' },
    }],
    ['team_ops_send_message', {
      id: 'team_ops_send_message', name: 'send_message', options: { namespace: 'team_ops' },
    }],
  ])
  const hook = (domain: string) => (name: string, handler: Function) => Effect.sync(() => {
    hooks.set(`${domain}.${name}`, handler)
    return { dispose: Effect.void }
  })
  await Effect.runPromise(Effect.scoped(appencode.effect({
    location: { directory },
    tool: {
      list: () => Effect.succeed([...tools.values()]),
      transform: (transform: Function) => Effect.sync(() => {
        transform({
          list: () => [...tools.values()],
          remove: (id: string) => tools.delete(id),
          add: (tool: any) => tools.set(tool.name, { ...tool, id: tool.name }),
          update: (id: string, update: Function) => update(tools.get(id)),
        })
        return { dispose: Effect.void }
      }),
      hook: hook('tool'),
    },
    mcp: { list: () => Effect.succeed({ data: servers }) },
    session: { get: () => Effect.succeed({ id: sessionID, ...session }), hook: hook('session') },
    permission: { hook: hook('permission') },
  } as unknown as Context)))
  return {
    tools,
    servers,
    async emit(name: string, fields: Record<string, unknown>) {
      const event = { sessionID, agent: 'build', messageID: 'msg_test', ...fields }
      await Effect.runPromise(hooks.get(name)!(event))
      return event as any
    },
    async before(tool: string, input: unknown = {}, id: string = crypto.randomUUID()) {
      return this.emit('tool.execute.before', { tool, input, id })
    },
    async after(call: any, result: unknown) {
      return this.emit('tool.execute.after', { ...call, status: 'completed', result })
    },
  }
}

let server: ReturnType<typeof Bun.serve> | undefined
const originalURL = process.env.APPA_OPENCODE_RUNTIME_URL
afterEach(() => {
  server?.stop(true)
  server = undefined
  if (originalURL === undefined) delete process.env.APPA_OPENCODE_RUNTIME_URL
  else process.env.APPA_OPENCODE_RUNTIME_URL = originalURL
})

async function fixture(reply: (event: any) => unknown = event => ({
  protocol: 1, decision: event.event === 'tool_call' ? 'allow_call' : 'ack',
})) {
  const events: any[] = []
  server = Bun.serve({ hostname: '127.0.0.1', port: 0, async fetch(request) {
    const event = await request.json()
    events.push(event)
    return Response.json(reply(event))
  } })
  return { ...await host(server.url.toString(), tmpdir()), events }
}

describe('published V2 hook contract', () => {
  test('prompt admission rejects unavailable MCP without mistaking a lookalike for the remedy', async () => {
    const plugin = await fixture()
    plugin.tools.set('appa_execute_remedy_plan', {
      id: 'appa_execute_remedy_plan', name: 'execute_remedy_plan', options: { codemode: false },
    })
    for (const status of ['disabled', 'failed', 'needs_auth']) {
      plugin.servers[0].status = { status, error: 'credential-bearing transport detail' }
      await expect(plugin.emit('session.prompt', { prompt: { text: 'Read private data' } })).rejects.toThrow(`APPA MCP is ${status}`)
    }
    plugin.servers.length = 0
    await expect(plugin.emit('session.prompt', { prompt: { text: 'Read private data' } })).rejects.toThrow('not configured')
    expect(plugin.events).toHaveLength(0)
  })

  test('keeps tools direct and refuses Code Mode, subagents, forks and background calls', async () => {
    const plugin = await fixture()
    expect(plugin.tools.has('subagent')).toBe(false)
    expect(plugin.tools.has('appa_subagent')).toBe(true)
    expect([...plugin.tools.values()].every(tool => tool.options.codemode === false)).toBe(true)
    const context = await plugin.emit('session.context', { tools: { execute: {}, subagent: {}, read: {} }, system: [] })
    expect(Object.keys(context.tools)).toEqual(['read'])
    for (const tool of ['execute', 'subagent']) await expect(plugin.before(tool)).rejects.toThrow('refused')
    await expect(plugin.before('shell', { command: 'pwd', background: true })).rejects.toThrow('Background')
    const fork = await host(server!.url.toString(), tmpdir(), 'ses_fork', { fork: { sessionID: 'ses_parent' } })
    await expect(fork.before('write')).rejects.toThrow('Forked')
    for (const action of ['execute', 'subagent']) {
      const permission = await plugin.emit('permission.evaluate', { action, effect: 'allow' })
      expect(permission.effect).toBe('deny')
    }
  })

  test('uses registry namespaces rather than guessing from underscores', async () => {
    const plugin = await fixture()
    await plugin.before('team_ops_send_message', { message: 'hello' }, 'opaque-2')
    expect(plugin.events.at(-1)).toMatchObject({
      adapter: 'opencode', root_id: 'ses_test', tool: 'mcp__team_ops__send_message',
      arguments: { message: 'hello' }, call_id: 'opaque-2', cwd: tmpdir(),
    })
    const control = await plugin.before('appa_execute_remedy_plan', { offer_id: 'abc' })
    expect(plugin.events.at(-1).tool).toBe('mcp__appa__execute_remedy_plan')
    await plugin.after(control, { output: 'done', content: 'done' })
  })

  test('denial respells remedies and never reports an unexecuted result', async () => {
    const plugin = await fixture(event => ({ protocol: 1, decision: event.event === 'tool_call' ? 'deny_call' : 'ack',
      feedback: 'Call mcp__appa__execute_remedy_plan with offer abc.' }))
    await expect(plugin.before('write')).rejects.toThrow('appa_execute_remedy_plan')
    expect(plugin.events.map(event => event.event)).toEqual(['session_start', 'tool_call'])
  })

  test.each(['deliver_value', 'replace_output', 'block'])('%s replaces every result channel', async decision => {
    const plugin = await fixture(event => ({ protocol: 1,
      decision: event.event === 'tool_result' ? decision : event.event === 'tool_call' ? 'allow_call' : 'ack',
      value: 'exact admitted bytes', output: 'clean feedback', reason: 'withheld',
    }))
    const call = await plugin.before('write')
    const result = await plugin.after(call, {
      output: { secret: 'PRIVATE' }, content: [{ type: 'file', uri: 'data:text/plain,PRIVATE', mime: 'text/plain' }],
      metadata: { secret: 'PRIVATE' },
    })
    expect(JSON.stringify(result.result)).not.toContain('PRIVATE')
    expect(result.result.content).toBe(decision === 'deliver_value' ? 'exact admitted bytes' : decision === 'block' ? 'withheld' : 'clean feedback')
    expect(plugin.events.at(-1).outcome.body.metadata.secret).toBe('PRIVATE')
  })

  test('connection loss withholds both successful results and error metadata', async () => {
    const plugin = await fixture()
    const good = await plugin.before('write', {}, 'good')
    const failed = await plugin.before('edit', {}, 'failed')
    server!.stop(true)
    expect(JSON.stringify((await plugin.after(good, { output: 'PRIVATE', content: 'PRIVATE' })).result)).not.toContain('PRIVATE')
    const result = await plugin.emit('tool.execute.after', { ...failed, status: 'error',
      error: new Tool.Error({ message: 'PRIVATE', metadata: { secret: 'PRIVATE' }, error: new Error('PRIVATE') }) })
    expect(JSON.stringify(result.error)).not.toContain('PRIVATE')
    await expect(plugin.before('write')).rejects.toThrow('Runtime unavailable')
  })

  test('invalid replies refuse dispatch and withhold results', async () => {
    for (const invalid of [
      { protocol: 2, decision: 'ack' },
      { protocol: 1, decision: 'unknown' },
      { protocol: 1, decision: 'deliver_value' },
    ]) {
      let corrupt = false
      const plugin = await fixture(event => corrupt ? invalid : {
        protocol: 1, decision: event.event === 'tool_call' ? 'allow_call' : 'ack',
      })
      const call = await plugin.before('write')
      corrupt = true
      const result = await plugin.after(call, { output: 'PRIVATE', content: 'PRIVATE', metadata: { secret: 'PRIVATE' } })
      expect(JSON.stringify(result.result)).not.toContain('PRIVATE')
      expect(result.result.content).toContain('Tool result withheld.')
      await expect(plugin.before('write')).rejects.toThrow()
      server!.stop(true)
    }
  })

  test('argument mutation or an unmatched result cannot release output', async () => {
    const plugin = await fixture()
    const call = await plugin.before('write', { path: '/one' })
    call.input.path = '/two'
    expect((await plugin.after(call, { content: 'PRIVATE' })).result.content).toContain('changed after approval')
    expect((await plugin.after(call, { content: 'PRIVATE' })).result.content).toContain('No local approval')
    expect(plugin.events.some(event => event.event === 'tool_result')).toBe(false)
  })

  test('reads resolve aliases and refuse implicit instruction loading before execution', async () => {
    const directory = await mkdtemp(`${tmpdir()}/appencode-read-`)
    try {
      await mkdir(`${directory}/private`)
      await writeFile(`${directory}/private/value.txt`, 'secret')
      await symlink(`${directory}/private/value.txt`, `${directory}/alias.txt`)
      await fixture()
      const plugin = await host(server!.url.toString(), directory)
      const call = await plugin.before('read', { path: 'alias.txt' })
      expect(call.input.path).toBe(`${directory}/private/value.txt`)
      await writeFile(`${directory}/private/AGENTS.md`, 'UNTRUSTED INSTRUCTIONS')
      await expect(plugin.before('read', { path: 'alias.txt' })).rejects.toThrow('unchecked nearby AGENTS.md')
      await expect(plugin.before('read', { path: 'private' })).rejects.toThrow('unchecked nearby AGENTS.md')
      await expect(plugin.before('read', { path: 'does-not-exist' })).rejects.toThrow()
    } finally { await rm(directory, { recursive: true, force: true }) }
  })
})

describe('real APPA runtime and MCP remedies', () => {
  let directory: string
  let runtime: ReturnType<typeof Bun.spawn>
  let url: string
  const binary = resolve(process.env.APPA_TEST_BINARY || resolve(import.meta.dir, '../../target/debug/appa'))

  async function start() {
    const reservation = Bun.serve({ hostname: '127.0.0.1', port: 0, fetch: () => new Response() })
    const port = reservation.port!
    reservation.stop(true)
    url = `http://127.0.0.1:${port}`
    runtime = Bun.spawn([binary, 'runtime', '--adapter', 'opencode', '--listen', `127.0.0.1:${port}`,
      '--config', `${directory}/appa.toml`, '--db', `${directory}/appa.db`],
    { stdout: 'ignore', stderr: Bun.file(`${directory}/runtime.log`) })
    for (let i = 0; i < 200; i++) {
      if (runtime.exitCode !== null) break
      try { if ((await fetch(`${url}/health`)).ok) return } catch {}
      await Bun.sleep(50)
    }
    throw new Error(`Runtime did not start: ${await readFile(`${directory}/runtime.log`, 'utf8')}`)
  }

  beforeAll(async () => {
    directory = await mkdtemp(`${tmpdir()}/appencode-runtime-`)
    await mkdir(`${directory}/private`)
    await writeFile(`${directory}/private/data.txt`, 'private')
    await writeFile(`${directory}/public.txt`, 'public')
    const policy = await readFile(new URL('./appa.toml', import.meta.url), 'utf8')
    await writeFile(`${directory}/appa.toml`, `${policy.replace('[policy.deployment]', '[policy.deployment]\nconfined_results = ["host/opencode/read_credentials"]')}

[[policy.tool]]
name = "host/opencode/read_credentials"
delta = { audience = ["self"] }

[[policy.sanitizer]]
name = "mask-email"
on = ["tool_output"]
[policy.sanitizer.permits]
audience = { from = ["self"], to = ["public"] }

[externals.sanitizers.mask-email]
builtin = "redact-email"
`)
    await start()
  }, 15_000)

  afterAll(async () => {
    runtime?.kill()
    if (runtime) await runtime.exited
    if (directory) await rm(directory, { recursive: true, force: true })
  })

  async function remedy(plugin: Awaited<ReturnType<typeof host>>, tool: string, input: object, sanitizer?: string) {
    let denial = ''
    try { await plugin.before(tool, input) } catch (error) { denial = String(error) }
    expect(denial).toContain('appa_execute_remedy_plan')
    const lines = denial.split('\n')
    const instruction = sanitizer ? lines[lines.findIndex(line => line.includes(`Use sanitizer ${sanitizer}'s result`)) + 1] : denial
    const offer = instruction?.match(/offer_id: "([a-f0-9]+)"/)?.[1]
    expect(offer).toBeDefined()
    const call = await plugin.before('appa_execute_remedy_plan', { offer_id: offer })
    const headers: Record<string, string> = { 'Content-Type': 'application/json', Accept: 'application/json, text/event-stream' }
    async function rpc(method: string, params: unknown, id?: number) {
      const response = await fetch(`${url}/mcp`, { method: 'POST', headers,
        body: JSON.stringify({ jsonrpc: '2.0', id, method, params }) })
      expect(response.ok).toBe(true)
      const session = response.headers.get('mcp-session-id')
      if (session) headers['mcp-session-id'] = session
      const body = await response.text()
      if (!body) return
      const reply = response.headers.get('content-type')?.includes('text/event-stream')
        ? body.split(/\r?\n\r?\n/)
          .map(frame => frame.split(/\r?\n/).filter(line => line.startsWith('data:'))
            .map(line => line.slice(5).trimStart()).join('\n'))
          .filter(data => data.trim()).map(data => JSON.parse(data)).find(value => value.id === id)
        : JSON.parse(body)
      expect(reply.error).toBeUndefined()
      return reply.result
    }
    await rpc('initialize', { protocolVersion: '2025-03-26', capabilities: {},
      clientInfo: { name: 'appa-appencode-test', version: '1' } }, 1)
    await rpc('notifications/initialized', {})
    try {
      const output = await rpc('tools/call', { name: 'execute_remedy_plan', arguments: call.input }, 2)
      expect(output.isError).not.toBe(true)
      await plugin.after(call, { output, content: output.content })
    } finally { await fetch(`${url}/mcp`, { method: 'DELETE', headers }) }
    return plugin.before(tool, input)
  }

  test('private reads admit out of order, and persist through empty context and restart', async () => {
    let plugin = await host(url, directory, 'ses_private')
    const privateCall = await remedy(plugin, 'read', { path: 'private/data.txt' })
    const publicCall = await plugin.before('read', { path: 'public.txt' })
    await plugin.after(publicCall, { output: 'public', content: 'public' })
    await plugin.after(privateCall, { output: 'private', content: 'private' })
    await expect(plugin.before('webfetch', { url: 'https://example.com/?q=private' })).rejects.toThrow('audience')
    runtime.kill()
    await runtime.exited
    await start()
    plugin = await host(url, directory, 'ses_private')
    await plugin.emit('session.context', { tools: {}, system: [], messages: [] })
    await expect(plugin.before('webfetch', { url: 'https://example.com/' })).rejects.toThrow('audience')
    const independent = await host(url, directory, 'ses_independent')
    await independent.before('shell', { command: 'pwd' })
  }, 15_000)

  test('untrusted web results block edits; undeclared tools and arbitrary shell fail closed', async () => {
    const plugin = await host(url, directory, 'ses_web')
    const web = await remedy(plugin, 'webfetch', { url: 'https://example.com/' })
    await plugin.after(web, { output: 'untrusted instructions', content: 'untrusted instructions' })
    await expect(plugin.before('write', { path: 'file', content: 'attack' })).rejects.toThrow('trust')
    await expect(plugin.before('unknown')).rejects.toThrow()
    await expect(plugin.before('shell', { command: 'pwd; curl https://example.com' })).rejects.toThrow()
  })

  test('real sanitizer replaces structured output, text, attachments and metadata without merging', async () => {
    const plugin = await host(url, directory, 'ses_sanitize')
    const call = await remedy(plugin, 'read_credentials', {}, 'mask-email')
    const result = await plugin.after(call, { output: 'alice@corp.example', content: 'mail alice@corp.example',
      metadata: { email: 'alice@corp.example' } })
    expect(JSON.stringify(result.result)).not.toContain('alice@corp.example')
    expect(result.result.content).toContain('[redacted-email]')
    await plugin.before('shell', { command: 'pwd' })
  })
})

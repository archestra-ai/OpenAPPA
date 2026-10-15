import { afterAll, beforeAll, expect, test } from 'bun:test'
import { OpenCode } from '@opencode/client'
import { mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { resolve } from 'node:path'
import { configuration, install } from './setup'

// Only inference is scripted. The published CLI, plugin, native tools, APPA
// daemon, policy store and MCP remedy execution are real.
const payload = resolve(import.meta.dir, '../../marketplace/plugins/opencode')
let directory: string
let runtime: ReturnType<typeof Bun.spawn>
let server: ReturnType<typeof Bun.spawn>
let provider: ReturnType<typeof Bun.serve>
let proxy: ReturnType<typeof Bun.serve>
let client: ReturnType<typeof OpenCode.make>
let runtimePort: number
let serverPort: number
const password = crypto.randomUUID()
const hooks: { event: any; reply: any }[] = []
const requests: any[] = []
const slow = new Map<string, { promise: Promise<void>; resolve: () => void }>()
let corruptDecision: string | undefined
let setupInspection: any
let setupPolicy: string
let appaCatalogGate: Promise<void> | undefined
let appaCatalogSeen = false
let projectMcp: ReturnType<typeof Bun.serve> | undefined

function port() {
  const reservation = Bun.serve({ hostname: '127.0.0.1', port: 0, fetch: () => new Response() })
  const port = reservation.port!
  reservation.stop(true)
  return port
}

async function ready(check: () => Promise<unknown>, process: ReturnType<typeof Bun.spawn>, log: string) {
  for (let i = 0; i < 300; i++) {
    if (process.exitCode !== null) break
    try { await check(); return } catch {}
    await Bun.sleep(50)
  }
  throw new Error(`Process did not start: ${await readFile(log, 'utf8')}`)
}

async function startRuntime(config = `${directory}/appa.toml`, env: Record<string, string> = {}) {
  runtime = Bun.spawn([resolve(process.env.APPA_TEST_BINARY || '../../target/debug/appa'), 'runtime', '--adapter', 'opencode',
    '--listen', `127.0.0.1:${runtimePort}`, '--config', config, '--db', `${directory}/appa.db`],
  { env: { ...process.env, ...env }, stdout: 'ignore', stderr: Bun.file(`${directory}/appa.log`) })
  await ready(async () => { if (!(await fetch(`http://127.0.0.1:${runtimePort}/health`)).ok) throw new Error() }, runtime, `${directory}/appa.log`)
}

async function startServer(setupConfig?: string) {
  server = Bun.spawn([resolve(import.meta.dir, 'node_modules/.bin/opencode'), 'serve', '--hostname', '127.0.0.1', '--port', String(serverPort)],
  { cwd: directory, env: { ...process.env, HOME: `${directory}/home`, XDG_CONFIG_HOME: `${directory}/config`,
    XDG_DATA_HOME: `${directory}/data`, XDG_CACHE_HOME: `${directory}/cache`, XDG_STATE_HOME: `${directory}/state`,
    APPA_OPENCODE_RUNTIME_URL: proxy.url.toString(),
    APPA_OPENCODE_SETUP_CONFIG: setupConfig,
    APPA_OPENCODE_SETUP_BINARY: setupConfig ? resolve(process.env.APPA_TEST_BINARY || '../../target/debug/appa') : undefined,
    OPENCODE_PASSWORD: password, OPENCODE_DISABLE_MODELS_FETCH: '1' },
  stdout: Bun.file(`${directory}/opencode.log`), stderr: Bun.file(`${directory}/opencode-error.log`) })
  client = OpenCode.make({ baseUrl: `http://127.0.0.1:${serverPort}`, headers: {
    authorization: `Basic ${Buffer.from(`opencode:${password}`).toString('base64')}`,
    'x-opencode-directory': encodeURIComponent(directory),
  } })
  await ready(() => client.server.info(), server, `${directory}/opencode-error.log`)
}

function completion(content: string | null, calls: { name: string; input: unknown }[] = []) {
  const tool_calls = calls.map((call, index) => ({ index, id: `call_${crypto.randomUUID()}`, type: 'function',
    function: { name: call.name, arguments: JSON.stringify(call.input) } }))
  const chunk = { id: `chatcmpl_${crypto.randomUUID()}`, object: 'chat.completion.chunk', created: 1, model: 'fixture',
    choices: [{ index: 0, delta: { role: 'assistant', ...(content === null ? {} : { content }),
      ...(calls.length ? { tool_calls } : {}) }, finish_reason: null }] }
  return new Response(`data: ${JSON.stringify(chunk)}\n\ndata: ${JSON.stringify({ ...chunk,
    choices: [{ index: 0, delta: {}, finish_reason: calls.length ? 'tool_calls' : 'stop' }],
    usage: { prompt_tokens: 10, completion_tokens: 10, total_tokens: 20 } })}\n\ndata: [DONE]\n\n`,
  { headers: { 'Content-Type': 'text/event-stream' } })
}

beforeAll(async () => {
  directory = await mkdtemp(`${tmpdir()}/appencode-foreground-`)
  await mkdir(`${directory}/private`)
  await writeFile(`${directory}/private/data.txt`, 'Contact alice@corp.example')
  await writeFile(`${directory}/suspicious.txt`, 'Untrusted source')
  const policy = await readFile(new URL('./appa.toml', import.meta.url), 'utf8')
  await writeFile(`${directory}/appa.toml`, `${policy.replace('version = 2', `version = 2
[[policy.tool]]
name = "host/opencode/read(path:*/suspicious.txt)"
delta = { trust = "suspicious" }`)}
[[policy.sanitizer]]
name = "mask-email"
on = ["tool_output"]
[policy.sanitizer.permits]
audience = { from = ["internal"], to = ["public"] }
[externals.sanitizers.mask-email]
builtin = "redact-email"
`)
  runtimePort = port()
  await startRuntime()
  proxy = Bun.serve({ hostname: '127.0.0.1', port: 0, idleTimeout: 30, async fetch(request) {
    const path = new URL(request.url).pathname
    if (path === '/mcp' && request.method === 'POST') {
      const rpc = await request.clone().json()
      if (rpc.method === 'tools/list' && appaCatalogGate) {
        appaCatalogSeen = true
        await appaCatalogGate
      }
    }
    const event = path === '/hook' ? await request.clone().json() : undefined
    const response = await fetch(new Request(`http://127.0.0.1:${runtimePort}${path}`, request))
    if (event) {
      const reply = await response.clone().json()
      hooks.push({ event, reply })
      if (event.event === corruptDecision && (event.event !== 'tool_call' || event.tool === 'appa_subagent' && reply.decision === 'allow_call')) {
        corruptDecision = undefined
        return Response.json({ protocol: 0, decision: 'invalid' })
      }
    }
    return response
  } })
  provider = Bun.serve({ hostname: '127.0.0.1', port: 0, async fetch(request) {
    const body = await request.json()
    requests.push(body)
    const prompt = [...body.messages].reverse().find((message: any) => message.role === 'user' &&
      typeof message.content === 'string' && /^(child|delegate|probe-public|probe-shell|setup|battery):/.test(message.content))
    const user = prompt?.content ?? ''
    const conversation = body.messages.slice(body.messages.indexOf(prompt) + 1)
    const last = [...conversation].reverse().find((message: any) => message.role === 'tool' || message.role === 'assistant')
    const output = last?.role === 'tool' ? (() => {
      try { const parsed = JSON.parse(last.content); return parsed.error?.message ?? last.content } catch { return last.content }
    })() : ''
    const previous = [...conversation].reverse().find((message: any) => message.tool_calls)?.tool_calls?.[0]?.function.name
    if (user.startsWith('battery:')) {
      const call = JSON.parse(user.slice('battery:'.length))
      if (output.includes('offer_id:') && previous !== 'appa_execute_remedy_plan' && call.remedy !== false) {
        return completion(null, [{ name: 'appa_execute_remedy_plan', input: {
          offer_id: output.match(/offer_id: "([a-f0-9]+)"/)[1],
        } }])
      }
      if (last?.role === 'tool' && !(previous === 'appa_execute_remedy_plan' && output.includes('Authorized'))) {
        return completion(`Battery result: ${output}`)
      }
      return completion(null, [{ name: call.name, input: call.input }])
    }
    if (user.startsWith('setup:')) {
      if (last?.role === 'tool') {
        if (user === 'setup:init') {
          const decoded = JSON.parse(output)
          setupInspection = decoded.digest ? decoded : JSON.parse(decoded.output ?? decoded.content)
          return completion('Proposal: include Microsoft Learn and permit only printf setup-proof in addition to pwd. Approve, or tell me what to change.')
        }
        return completion(`Setup result: ${output}`)
      }
      return completion(null, [{ name: 'appa_setup', input: user === 'setup:init' ? { operation: 'inspect' }
        : { operation: 'publish', expected_digest: setupInspection.digest,
          policy: user === 'setup:invalid' ? 'invalid = [' : setupPolicy } }])
    }
    if (user === 'delegate:parallel') {
      const batch = [...conversation].reverse().find((message: any) => message.tool_calls)?.tool_calls ?? []
      const outcomes = batch.map((call: any) => conversation.find((message: any) => message.tool_call_id === call.id)?.content ?? '')
      if (outcomes.some((output: string) => output.includes('offer_id:'))) {
        return completion(null, outcomes.map((output: string) => ({ name: 'appa_execute_remedy_plan', input: {
          offer_id: JSON.parse(output).error.message.match(/offer_id: "([a-f0-9]+)"/)[1], label: { trust: 'trusted', audience: ['public'] },
        } })))
      }
      if (batch[0]?.function.name === 'appa_subagent') return completion(`Parallel returns: ${outcomes.join(' | ')}`)
      return completion(null, ['parallel-a', 'parallel-b'].map(task => ({ name: 'appa_subagent',
        input: { agent: 'general', description: 'Same description', prompt: `child:${task}` } })))
    }
    if (user.startsWith('probe-public:')) {
      return last?.role === 'tool' ? completion(`Public probe: ${output}`)
        : completion(null, [{ name: 'webfetch', input: { url: 'https://example.invalid/' } }])
    }
    if (user.startsWith('probe-shell:')) {
      return last?.role === 'tool' ? completion(`Public shell: ${output}`)
        : completion(null, [{ name: 'shell', input: { command: user === 'probe-shell:setup' ? 'printf setup-proof' : 'pwd' } }])
    }
    if (output.includes('offer_id:')) {
      const lines = output.split('\n')
      const instruction = user.includes('sanitize') && previous === 'appa_subagent'
        ? lines[lines.findIndex((line: string) => line.includes('Have sanitizer mask-email')) + 1] : output
      const offer_id = instruction?.match(/offer_id: "([a-f0-9]+)"/)?.[1]
      if (!offer_id) return completion('Fixture could not select a return offer')
      return completion(null, [{ name: 'appa_execute_remedy_plan', input: { offer_id,
        ...(previous === 'appa_subagent' ? { label: { trust: user.includes('inherit-trust') ? 'suspicious' : 'trusted',
          audience: [user.includes('inherit-audience') || user.includes('private') && !user.includes('public-floor') ? 'internal' : 'public'] } } : {}) } }])
    }
    if (user.startsWith('child:')) {
      const gate = slow.get(user)
      if (gate) await gate.promise
      if (user === 'child:parallel-a') await Bun.sleep(300)
      if (user === 'child:nested') return last?.role === 'tool' ? completion(`Nested refused: ${output}`)
        : completion(null, [{ name: 'appa_subagent', input: { agent: 'general', description: 'Nested', prompt: 'unbound grandchild' } }])
      if (user === 'child:inherit-audience') return last?.role === 'tool' ? completion(`Inherited audience: ${output}`)
        : completion(null, [{ name: 'webfetch', input: { url: 'https://example.invalid/' } }])
      if (user === 'child:inherit-trust') return last?.role === 'tool' ? completion(`Inherited trust: ${output}`)
        : completion(null, [{ name: 'write', input: { path: `${directory}/unchecked.txt`, content: 'Unchecked write' } }])
      if (user === 'child:failure') return last?.role === 'tool'
        ? Response.json({ error: { message: 'RAW-CHILD-ERROR', type: 'invalid_request_error' } }, { status: 400 })
        : completion('EARLIER-CHILD-ANSWER', [{ name: 'read', input: { path: 'suspicious.txt' } }])
      if (user.includes('private') && (!last || last.role !== 'tool' || previous === 'appa_execute_remedy_plan')) {
        return completion(null, [{ name: 'read', input: { path: 'private/data.txt' } }])
      }
      return completion(user.includes('private') ? output.includes('alice@corp.example') ? 'Contact alice@corp.example' : `Child read refused: ${output}` : user)
    }
    if (!user.startsWith('delegate:')) return completion('Foreground delegation test')
    const input = { agent: 'general', description: 'Foreground test', prompt: `child:${user.slice('delegate:'.length)}` }
    if (user === 'delegate:background-input') Object.assign(input, { background: true })
    if (user === 'delegate:resume-input') Object.assign(input, { sessionID: 'ses_existing' })
    if (user === 'delegate:inherit-trust' && previous !== 'appa_subagent' &&
        !conversation.some((message: any) => message.role === 'tool' && message.content.includes('Untrusted source'))) {
      return completion(null, [{ name: 'read', input: { path: 'suspicious.txt' } }])
    }
    if (last?.role === 'tool') {
      if (previous !== 'appa_execute_remedy_plan' && previous !== 'read') return completion(`Parent received: ${output}`)
    }
    return completion(null, [{ name: 'appa_subagent', input }])
  } })
  await mkdir(`${directory}/config/opencode`, { recursive: true })
  await writeFile(`${directory}/config/opencode/opencode.json`, JSON.stringify({
    plugins: [payload],
    mcp: { servers: { appa: { type: 'remote', url: `${proxy.url}mcp`, oauth: false } } },
  }))
  await writeFile(`${directory}/opencode.json`, JSON.stringify({
    snapshots: false, share: 'disabled', update: 'disable', model: 'appa-test/fixture',
    permissions: [{ action: '*', resource: '*', effect: 'allow' }],
    providers: { 'appa-test': { package: '@opencode/ai/providers/openai-compatible',
      settings: { baseURL: `${provider.url}v1`, apiKey: 'local-test-fixture' },
      models: { fixture: { capabilities: { tools: true }, limit: { context: 128000, output: 4096 } } } } },
    commands: { alternate: { template: 'child:alternate-command', agent: 'general', subagent: true } },
  }))
  serverPort = port()
  await startServer()
}, 45_000)

afterAll(async () => {
  server?.kill()
  if (server) await server.exited
  runtime?.kill()
  if (runtime) await runtime.exited
  provider?.stop(true); proxy?.stop(true); projectMcp?.stop(true)
  if (directory) await rm(directory, { recursive: true, force: true })
})

async function begin(task: string) {
  const session = await client.session.create({ location: { directory } })
  await client.session.prompt({ sessionID: session.id, text: `delegate:${task}` })
  return session
}

async function delegate(task: string) {
  const session = await begin(task)
  await client.session.wait({ sessionID: session.id })
  return { session, history: await client.session.context({ sessionID: session.id }) }
}

test('cold prompts wait for the real remedy executor; unavailable discovery has a deadline', async () => {
  for (const expire of [false, true]) {
    server.kill(); await server.exited
    let release!: () => void
    appaCatalogGate = new Promise<void>(resolve => { release = resolve })
    appaCatalogSeen = false
    try {
      await startServer()
      const session = await client.session.create({ location: { directory } })
      const prior = requests.length
      const submitted = client.session.prompt({ sessionID: session.id, text: 'delegate:private cold-start' })
      // Observe rejection immediately too: otherwise a failing baseline leaves an
      // unhandled rejection while this test is waiting for the catalog request.
      const outcome = submitted.then(() => 'admitted', error => String(error))
      await ready(async () => { if (!appaCatalogSeen) throw new Error('Catalog not requested') }, server, `${directory}/opencode-error.log`)
      expect(await Promise.race([outcome, Bun.sleep(150).then(() => 'waiting')])).toBe('waiting')
      expect(requests.length).toBe(prior)
      if (expire) {
        const started = performance.now()
        // This host reports prompt-hook failures as HTTP 500, without the message.
        expect(await outcome).not.toBe('admitted')
        const elapsed = performance.now() - started
        expect(elapsed).toBeGreaterThan(9_000)
        expect(elapsed).toBeLessThan(12_000)
        expect(requests.length).toBe(prior)
        release()
        await client.session.prompt({ sessionID: session.id, text: 'delegate:after-timeout' })
        await client.session.wait({ sessionID: session.id })
        expect(JSON.stringify(await client.session.context({ sessionID: session.id }))).toContain('Parent received: child:after-timeout')
      } else {
        release()
        expect(await outcome).toBe('admitted')
        await client.session.wait({ sessionID: session.id })
        const first = requests.slice(prior).find(request => request.tools?.length)
        expect(first.tools.some((tool: any) => tool.function.name === 'appa_execute_remedy_plan')).toBe(true)
        expect(JSON.stringify(await client.session.context({ sessionID: session.id }))).toContain('alice@corp.example')
        expect(hooks.some(hook => hook.event.root_id === session.id && hook.event.tool === 'mcp__appa__execute_remedy_plan' &&
          hook.reply.decision === 'pass_control')).toBe(true)
      }
    } finally {
      release()
      appaCatalogGate = undefined
    }
  }
}, 45_000)

test('published CLI runs a checked foreground child and refuses later child prompting', async () => {
  const { session, history } = await delegate('hello')
  expect(JSON.stringify(history)).toContain('Parent received: child:hello')
  const child = (await client.session.list({ parentID: session.id })).data[0]!
  expect(child).toBeDefined()
  const flow = hooks.filter(hook => hook.event.root_id === session.id)
  expect(flow.some(hook => hook.event.event === 'child_start' && hook.event.child_id === child.id && hook.event.spawn_binding)).toBe(true)
  expect(flow.some(hook => hook.event.event === 'spawn_result' && hook.event.value === 'child:hello' && hook.reply.decision === 'ack')).toBe(true)
  await expect(client.session.prompt({ sessionID: child.id, text: 'resume unchecked' })).rejects.toThrow()
  expect(requests.some(request => request.messages.some((message: any) => message.content === 'resume unchecked'))).toBe(false)
}, 45_000)

test('real child sanitizer replaces the return before the parent receives it', async () => {
  const { session, history } = await delegate('sanitize private')
  const parent = JSON.stringify(history)
  expect(parent).toContain('[redacted-email]')
  expect(parent).not.toContain('alice@corp.example')
  const flow = hooks.filter(hook => hook.event.root_id === session.id)
  expect(flow.some(hook => hook.event.child_id && hook.event.tool === 'read' && hook.reply.decision === 'allow_call')).toBe(true)
  expect(flow.some(hook => hook.event.event === 'child_end' && hook.reply.decision === 'child_return')).toBe(true)
  expect(flow.some(hook => hook.event.event === 'spawn_result' && hook.reply.decision === 'ack')).toBe(true)
  await client.session.prompt({ sessionID: session.id, text: 'probe-shell:after-sanitizer' })
  await client.session.wait({ sessionID: session.id })
  expect(hooks.some(hook => hook.event.root_id === session.id && !hook.event.child_id &&
    hook.event.tool === 'shell' && hook.reply.decision === 'allow_call')).toBe(true)
}, 45_000)

test('distinct concurrent children return in reverse order without exchanging identities', async () => {
  const { session, history } = await delegate('parallel')
  const flow = hooks.filter(hook => hook.event.root_id === session.id)
  const returns = flow.filter(hook => hook.event.event === 'spawn_result')
  expect(returns.map(hook => hook.event.value)).toEqual(['child:parallel-b', 'child:parallel-a'])
  expect(new Set(returns.map(hook => hook.event.spawned_id)).size).toBe(2)
  expect(JSON.stringify(history)).toContain('Parallel returns: child:parallel-a | child:parallel-b')
}, 45_000)

test('public return floor refuses a private child read; raw internal return confines the parent', async () => {
  const publicFloor = await delegate('public-floor private')
  expect(JSON.stringify(publicFloor.history)).not.toContain('alice@corp.example')
  expect(hooks.some(hook => hook.event.root_id === publicFloor.session.id && hook.event.child_id &&
    hook.event.tool === 'read' && hook.reply.decision === 'deny_call')).toBe(true)
  const internal = await delegate('private raw')
  expect(JSON.stringify(internal.history)).toContain('alice@corp.example')
  await client.session.prompt({ sessionID: internal.session.id, text: 'probe-public:after-return' })
  await client.session.wait({ sessionID: internal.session.id })
  expect(hooks.some(hook => hook.event.root_id === internal.session.id && !hook.event.child_id &&
    hook.event.tool === 'webfetch' && hook.reply.decision === 'deny_call')).toBe(true)
}, 45_000)

test('a child inherits audience and trust before its first tool call', async () => {
  const internal = await delegate('private raw')
  await client.session.prompt({ sessionID: internal.session.id, text: 'delegate:inherit-audience' })
  await client.session.wait({ sessionID: internal.session.id })
  expect(hooks.some(hook => hook.event.root_id === internal.session.id && hook.event.child_id &&
    hook.event.tool === 'webfetch' && hook.reply.decision === 'deny_call')).toBe(true)
  const suspicious = await delegate('inherit-trust')
  expect(hooks.some(hook => hook.event.root_id === suspicious.session.id && hook.event.child_id &&
    hook.event.tool === 'write' && hook.reply.decision === 'deny_call')).toBe(true)
  expect(JSON.stringify(suspicious.history)).toContain('Inherited trust:')
  expect(await Bun.file(`${directory}/unchecked.txt`).exists()).toBe(false)
}, 45_000)

test('a failed child cannot return earlier prose or raw error details', async () => {
  const { session, history } = await delegate('failure')
  const parent = JSON.stringify(history)
  expect(parent).toContain('Child execution failed; its return is withheld')
  expect(parent).not.toContain('RAW-CHILD-ERROR')
  expect(parent).not.toContain('EARLIER-CHILD-ANSWER')
  const child = (await client.session.list({ parentID: session.id })).data[0]!
  expect(JSON.stringify(await client.session.context({ sessionID: child.id }))).toContain('EARLIER-CHILD-ANSWER')
  expect(hooks.some(hook => hook.event.event === 'spawn_result' && hook.event.root_id === session.id && hook.event.value)).toBe(false)
}, 45_000)

test('lost approval, binding and return replies withhold output without blocking later launches', async () => {
  for (const stage of ['tool_call', 'child_start', 'child_end']) {
    corruptDecision = stage
    const { session, history } = await delegate(`lost-${stage}`)
    expect(corruptDecision).toBeUndefined()
    expect(JSON.stringify(history)).toContain('Invalid runtime reply')
    expect(JSON.stringify(history)).not.toContain(`Parent received: child:lost-${stage}`)
    await client.session.prompt({ sessionID: session.id, text: `delegate:after-${stage}` })
    await client.session.wait({ sessionID: session.id })
    expect(JSON.stringify(await client.session.context({ sessionID: session.id }))).toContain(`Parent received: child:after-${stage}`)
  }
}, 45_000)

test('background input, resume input, nested delegation and alternate commands are refused', async () => {
  for (const task of ['background-input', 'resume-input']) {
    const { session, history } = await delegate(task)
    expect(JSON.stringify(history)).toContain('Only fresh, one-level foreground delegation')
    expect((await client.session.list({ parentID: session.id })).data.length).toBe(0)
  }
  const nested = await delegate('nested')
  expect(JSON.stringify(nested.history)).toContain('Nested refused')
  const child = (await client.session.list({ parentID: nested.session.id })).data[0]!
  expect((await client.session.list({ parentID: child.id })).data.length).toBe(0)
  await expect(client.session.generate({ sessionID: child.id, prompt: 'unmediated generation' })).rejects.toThrow()
  await expect(client.session.command({ sessionID: nested.session.id, name: 'alternate', text: '' })).rejects.toThrow()
  expect(requests.some(request => request.messages.some((message: any) => message.content === 'child:alternate-command'))).toBe(false)
  const fork = await client.session.fork({ sessionID: nested.session.id })
  await expect(client.session.prompt({ sessionID: fork.id, text: 'forked history' })).rejects.toThrow()
}, 45_000)

async function slowChild(task: string) {
  let resolve!: () => void
  const promise = new Promise<void>(done => { resolve = done })
  slow.set(`child:${task}`, { promise, resolve })
  const session = await begin(task)
  await ready(async () => {
    if (!requests.some(request => request.messages.some((message: any) => message.content === `child:${task}`))) throw new Error('Child not running')
  }, server, `${directory}/opencode-error.log`)
  const child = (await client.session.list({ parentID: session.id })).data[0]!
  return { session, child, resolve }
}

test('host background action cannot turn owned foreground delegation into a synthetic return', async () => {
  const { session, resolve } = await slowChild('slow-background')
  await client.session.background({ sessionID: session.id })
  expect((await client.session.context({ sessionID: session.id })).some(message => message.type === 'synthetic')).toBe(false)
  resolve()
  await client.session.wait({ sessionID: session.id })
  const history = await client.session.context({ sessionID: session.id })
  expect(JSON.stringify(history)).toContain('Parent received: child:slow-background')
  expect(history.some(message => message.type === 'synthetic')).toBe(false)
}, 45_000)

test('steering and cancellation cannot leak a late child return or leave the launch queue stuck', async () => {
  const { session, child, resolve } = await slowChild('slow-cancel')
  await expect(client.session.prompt({ sessionID: child.id, text: 'steer unchecked' })).rejects.toThrow()
  await client.session.interrupt({ sessionID: session.id, resume: false })
  resolve()
  await client.session.wait({ sessionID: session.id })
  expect(JSON.stringify(await client.session.context({ sessionID: session.id }))).not.toContain('Parent received: child:slow-cancel')
  await client.session.prompt({ sessionID: session.id, text: 'delegate:after-cancel' })
  await client.session.wait({ sessionID: session.id })
  expect(JSON.stringify(await client.session.context({ sessionID: session.id }))).toContain('Parent received: child:after-cancel')
}, 45_000)

test('APPA restart retains a live child binding; OpenCode restart refuses old children', async () => {
  const live = await slowChild('slow-runtime')
  runtime.kill(); await runtime.exited; await startRuntime()
  live.resolve()
  await client.session.wait({ sessionID: live.session.id })
  expect(JSON.stringify(await client.session.context({ sessionID: live.session.id }))).toContain('Parent received: child:slow-runtime')
  const orphan = await slowChild('slow-host')
  server.kill(9); await server.exited
  orphan.resolve()
  await startServer()
  await expect(client.session.prompt({ sessionID: orphan.child.id, text: 'resume orphan' })).rejects.toThrow()
  await client.session.wait({ sessionID: orphan.child.id })
  expect(hooks.some(hook => hook.event.event === 'spawn_result' && hook.event.root_id === orphan.session.id && hook.event.value)).toBe(false)
  const recovered = await delegate('after-restart')
  expect(JSON.stringify(recovered.history)).toContain('Parent received: child:after-restart')
}, 45_000)

test('operator setup proposes without writing, rejects invalid and stale policies, and publishes for fresh protected sessions only', async () => {
  const before = await client.session.create({ location: { directory } })
  await client.session.prompt({ sessionID: before.id, text: 'probe-shell:setup' })
  await client.session.wait({ sessionID: before.id })
  expect(JSON.stringify(await client.session.context({ sessionID: before.id }))).toContain('arguments match no registered contract')
  const target = `${directory}/deployment/appa.toml`
  const projectSettings = await readFile(`${directory}/opencode.json`, 'utf8')
  await install(`${directory}/config/opencode`, target, proxy.url.toString().replace(/\/$/, ''))
  // Exercise the shipped bundle's configuration()/guide(), not a source plugin
  // added by the developer launcher. Source registration has separate tests.
  const settingsPath = `${directory}/config/opencode/opencode.json`
  const settings = JSON.parse(await readFile(settingsPath, 'utf8'))
  settings.plugins = [payload]
  await writeFile(settingsPath, JSON.stringify(settings))
  expect(await readFile(`${directory}/opencode.json`, 'utf8')).toBe(projectSettings)
  let releaseProject!: () => void
  let projectCatalogSeen = false
  const projectCatalog = new Promise<void>(resolve => { releaseProject = resolve })
  projectMcp = Bun.serve({ hostname: '127.0.0.1', port: 0, idleTimeout: 30, async fetch(request) {
    if (request.method !== 'POST') return new Response(null, { status: 405 })
    const rpc = await request.json()
    if (rpc.id === undefined) return new Response(null, { status: 202 })
    if (rpc.method === 'initialize') return Response.json({ jsonrpc: '2.0', id: rpc.id, result: {
      protocolVersion: rpc.params.protocolVersion, serverInfo: { name: 'project-fixture', version: '1' }, capabilities: { tools: {} },
    } })
    if (rpc.method === 'tools/list') {
      projectCatalogSeen = true
      await projectCatalog
      return Response.json({ jsonrpc: '2.0', id: rpc.id, result: { tools: [
        { name: 'project_marker', description: 'Project-only inventory marker; never execute', inputSchema: { type: 'object', properties: {} } },
      ] } })
    }
    return Response.json({ jsonrpc: '2.0', id: rpc.id, error: { code: -32601, message: 'Marker must not execute' } })
  } })
  await writeFile(`${directory}/opencode.json`, JSON.stringify({ ...JSON.parse(projectSettings), mcp: { servers: {
    'project-only': { type: 'remote', url: `${projectMcp.url}mcp`, oauth: false },
    disabled: { type: 'remote', url: 'http://127.0.0.1:1/mcp', disabled: true, oauth: false },
  } } }))
  const original = await readFile(target, 'utf8')
  setupPolicy = original.replace('opencode/appa.toml"]', 'opencode/appa.toml", "batteries/microsoft-learn/appa.toml"]')
    .replace('[policy]\nversion = 2', '[policy]\nversion = 2\n[[policy.tool]]\nname = "host/opencode/shell(command:printf setup-proof)"\nrequires = { trust = "trusted", audience = { contains = ["public"] } }')
  server.kill(); await server.exited
  runtime.kill(); await runtime.exited
  await writeFile(`${directory}/appa.toml`, '[policy]\nversion = 2\n[[policy.tool]]\nname = "host/opencode/appa_setup"\n[externals]\ntimeout_ms = 2000\nmax_body_bytes = 65536\n')
  await startRuntime()
  await startServer(target)
  const session = await client.session.create({ location: { directory } })
  await ready(async () => {
    const mcp = await client.mcp.list({ location: { directory } })
    if (!projectCatalogSeen || !mcp.data.some(server => server.name === 'appa' && server.status.status === 'connected')) throw new Error('APPA not connected')
  }, server, `${directory}/opencode-error.log`)
  setupInspection = undefined
  await client.session.prompt({ sessionID: session.id, text: 'setup:init' })
  await ready(async () => {
    if (!hooks.some(hook => hook.event.root_id === session.id && hook.event.tool === 'appa_setup')) throw new Error('Inspection not started')
  }, server, `${directory}/opencode-error.log`)
  try {
    await Bun.sleep(150)
    expect(setupInspection).toBeUndefined()
    const started = performance.now()
    await client.session.wait({ sessionID: session.id })
    expect(performance.now() - started).toBeLessThan(12_000)
    expect(setupInspection.inventory).toMatchObject({ scope: 'observed', servers: expect.arrayContaining([
      { name: 'appa', status: 'connected' }, { name: 'project-only', status: 'pending' }, { name: 'disabled', status: 'disabled' },
    ]) })
    expect(setupInspection.inventory.note).toContain('not proof of complete MCP discovery')
    await client.session.prompt({ sessionID: session.id, text: 'setup:approve' })
    await client.session.wait({ sessionID: session.id })
    expect(JSON.stringify(await client.session.context({ sessionID: session.id }))).toContain('project-only (pending)')
    expect(await readFile(target, 'utf8')).toBe(original)
  } finally { releaseProject() }
  // Connected is not a registry barrier. Reinspection observes later registry updates.
  await ready(async () => {
    await client.session.prompt({ sessionID: session.id, text: 'setup:init' })
    await client.session.wait({ sessionID: session.id })
    if (!setupInspection.tools.some((tool: any) => tool.canonical === 'mcp/project-only/project_marker')) throw new Error('Registry update pending')
  }, server, `${directory}/opencode-error.log`)
  expect(setupInspection.inventory.servers).toContainEqual({ name: 'project-only', status: 'connected' })
  expect(JSON.stringify(await client.session.context({ sessionID: session.id }))).toContain('Approve, or tell me what to change.')
  expect(await readFile(target, 'utf8')).toBe(original)
  expect(setupInspection.tools.some((tool: any) => tool.canonical === 'host/opencode/shell')).toBe(true)
  expect(setupInspection.batteries.some((battery: any) => battery.name === 'microsoft-learn' && battery.hosts.includes('opencode'))).toBe(true)
  const setupRequest = [...requests].reverse().find(request => request.messages.some((message: any) => message.content === 'setup:init'))
  expect(setupRequest.tools.map((tool: any) => tool.function.name)).toEqual(['appa_setup'])
  for (const text of ['setup:invalid', 'setup:approve']) {
    if (text === 'setup:approve') await writeFile(target, `${original}\n# Concurrent operator change\n`)
    await client.session.prompt({ sessionID: session.id, text })
    await client.session.wait({ sessionID: session.id })
    expect(await readFile(target, 'utf8')).toBe(text === 'setup:invalid' ? original : `${original}\n# Concurrent operator change\n`)
  }
  expect(JSON.stringify(await client.session.context({ sessionID: session.id }))).toContain('Policy validation refused')
  expect(JSON.stringify(await client.session.context({ sessionID: session.id }))).toContain('Policy changed; inspect and propose again')
  await client.session.prompt({ sessionID: session.id, text: 'setup:init' })
  await client.session.wait({ sessionID: session.id })
  setupPolicy += '\n# Concurrent operator change\n'
  await client.session.prompt({ sessionID: session.id, text: 'setup:approve' })
  await client.session.wait({ sessionID: session.id })
  expect(await readFile(target, 'utf8')).toBe(setupPolicy)
  expect(JSON.stringify(await client.session.context({ sessionID: session.id }))).toContain('saved')
  expect(hooks.some(hook => hook.event.root_id === session.id && hook.event.tool === 'appa_setup' && hook.reply.decision === 'allow_call')).toBe(true)

  // Close operator mode, serve the saved policy, and open an ordinary root.
  server.kill(); await server.exited
  runtime.kill(); await runtime.exited
  await startRuntime(target)
  await startServer()
  const normal = await client.session.create({ location: { directory } })
  await client.session.prompt({ sessionID: normal.id, text: 'probe-shell:setup' })
  await client.session.wait({ sessionID: normal.id })
  expect(JSON.stringify(await client.session.context({ sessionID: normal.id }))).toContain('setup-proof')
  expect(hooks.some(hook => hook.event.root_id === normal.id && hook.event.tool === 'shell' && hook.reply.decision === 'allow_call')).toBe(true)
  const normalRequest = [...requests].reverse().find(request => request.messages.some((message: any) => message.content === 'probe-shell:setup'))
  expect(normalRequest.tools.some((tool: any) => tool.function.name === 'appa_setup')).toBe(false)
  const other = `${directory}/other-project`
  await mkdir(other)
  await writeFile(`${other}/opencode.json`, projectSettings)
  const second = await client.session.create({ location: { directory: other } })
  await client.session.prompt({ sessionID: second.id, text: 'probe-shell:setup' })
  await client.session.wait({ sessionID: second.id })
  expect(JSON.stringify(await client.session.context({ sessionID: second.id }))).toContain('setup-proof')
  expect(hooks.some(hook => hook.event.root_id === second.id && hook.event.tool === 'shell' && hook.reply.decision === 'allow_call')).toBe(true)
  const refusal = await fetch(`${proxy.url}hook`, { method: 'POST', headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ protocol: 1, adapter: 'opencode', event: 'tool_call', root_id: normal.id,
      call_id: 'not-an-operator', tool: 'appa_setup', arguments: { operation: 'inspect' } }) })
  expect((await refusal.json()).decision).not.toBe('allow_call')
}, 60_000)

test('catalog batteries enforce MCP flows, GitHub helper audiences, and explicit Notion review through OpenCode', async () => {
  // External services and inference are fixtures; OpenCode, MCP transport, the
  // packaged plugin, runtime, battery policies and Python helpers are real.
  const calls: { server: string; name: string; arguments: any }[] = []
  const apiReads: string[] = []
  const tools: Record<string, string[]> = {
    docs: ['search_cloudflare_documentation'],
    logs: ['query_worker_observability'],
    notes: ['notion-fetch', 'notion-update-page', 'notion-move-pages'],
    codehub: ['get_file_contents', 'create_or_update_file'],
  }
  const fixture = Bun.serve({ hostname: '127.0.0.1', port: 0, async fetch(request) {
    const path = new URL(request.url).pathname
    if (path === '/audience') return Response.json({ version: 1, answer: { members: ['alice@corp.example'] } })
    if (request.method === 'GET' && !tools[path.slice(1)]) {
      apiReads.push(path)
      if (request.headers.get('authorization') !== 'Bearer ghp_fixture') return new Response(null, { status: 401 })
      if (path === '/user') return Response.json({ login: 'alice' })
      if (path === '/user/emails') return Response.json([{ email: 'alice@corp.example', primary: true, verified: true }])
      if (path === '/users/alice') return Response.json({ email: 'alice@corp.example' })
      if (/^\/repos\/acme\/(private|public)\/collaborators$/.test(path)) return Response.json([{ login: 'alice', type: 'User' }])
      if (/^\/repos\/acme\/(private|public)$/.test(path)) return Response.json({ visibility: path.endsWith('/private') ? 'private' : 'public', fork: false })
      return new Response(null, { status: 404 })
    }
    if (request.method !== 'POST') return new Response(null, { status: 405 })
    const body = await request.json()
    if (body.id === undefined) return new Response(null, { status: 202 })
    const server = path.slice(1)
    let result: unknown
    switch (body.method) {
      case 'initialize': result = { protocolVersion: '2025-03-26', capabilities: { tools: {} }, serverInfo: { name: server, version: '1' } }; break
      case 'tools/list': result = { tools: (tools[server] ?? []).map(name => ({ name, description: name,
        inputSchema: { type: 'object', properties: {}, additionalProperties: true } })) }; break
      case 'tools/call':
        calls.push({ server, ...body.params })
        result = { content: [{ type: 'text', text: `${server}/${body.params.name} result` }] }; break
      default: return Response.json({ jsonrpc: '2.0', id: body.id, error: { code: -32601, message: 'Unknown method' } })
    }
    return Response.json({ jsonrpc: '2.0', id: body.id, result })
  } })
  try {
    server.kill(); await server.exited
    runtime.kill(); await runtime.exited
    const target = `${directory}/batteries-deployment/appa.toml`
    await install(`${directory}/battery-settings`, target, proxy.url.toString().replace(/\/$/, ''))
    const setup = await configuration(target, resolve(process.env.APPA_TEST_BINARY || '../../target/debug/appa'))
    const inspected = JSON.parse((await setup({ operation: 'inspect' }, []))!)
    expect(inspected.batteries.map((battery: any) => battery.name)).toEqual(['cloudflare', 'github', 'microsoft-learn', 'notion', 'opencode'])
    const policy = `include = ["batteries/opencode/appa.toml", "batteries/cloudflare/appa.toml", "batteries/notion/appa.toml", "batteries/github/appa.toml"]
[aliases.mcp]
cloudflare = ["docs", "logs"]
notion = ["notes"]
github = ["codehub"]
[policy]
version = 2
[policy.deployment]
context_control = true
[policy.audience]
internal = ["people:members"]
[[policy.authority]]
name = "notion-operator"
permits = { attention = ["notion-review"] }
[externals]
timeout_ms = 10000
review_timeout_ms = 30000
max_body_bytes = 1048576
[externals.authorities.notion-operator]
builtin = "hitl"
[externals.audience.people]
url = "${fixture.url}audience"
selectors = [{ template = "members", feeds = "internal" }]
`
    expect(JSON.parse((await setup({ operation: 'publish', policy, expected_digest: inspected.digest }, []))!).state).toBe('saved')
    const settingsPath = `${directory}/config/opencode/opencode.json`
    const settings = JSON.parse(await readFile(settingsPath, 'utf8'))
    for (const name of Object.keys(tools)) settings.mcp.servers[name] = { type: 'remote', url: `${fixture.url}${name}`, oauth: false }
    await writeFile(settingsPath, JSON.stringify(settings))
    await startRuntime(target, { GITHUB_API_URL: fixture.url.toString().replace(/\/$/, ''), APPA_PROVIDER_GITHUB_TOKEN: 'ghp_fixture', APPA_BATTERY_TARGET: 'single-user' })
    await startServer()
    await ready(async () => {
      const connected = (await client.mcp.list({ location: { directory } })).data.filter(item => item.status.status === 'connected')
      if (!['appa', ...Object.keys(tools)].every(name => connected.some(server => server.name === name))) throw new Error('MCP discovery pending')
    }, server, `${directory}/opencode-error.log`)
    async function call(sessionID: string, name: string, input: unknown, remedy = true) {
      await client.session.prompt({ sessionID, text: `battery:${JSON.stringify({ name, input, remedy })}` })
      await client.session.wait({ sessionID })
      return JSON.stringify(await client.session.context({ sessionID }))
    }
    const create = () => client.session.create({ location: { directory } })
    const cloudflare = await create()
    expect(await call(cloudflare.id, 'docs_search_cloudflare_documentation', { query: 'alarms' })).toContain('docs/search_cloudflare_documentation result')
    expect(await call(cloudflare.id, 'logs_query_worker_observability', { query: { view: 'events' } })).toContain('logs/query_worker_observability result')
    const beforeLeak = calls.length
    await call(cloudflare.id, 'docs_search_cloudflare_documentation', { query: 'internal log contents' }, false)
    expect(calls.length).toBe(beforeLeak)

    const notion = await create()
    expect(await call(notion.id, 'notes_notion-fetch', { id: 'page-1' })).toContain('notes/notion-fetch result')
    expect(await call(notion.id, 'notes_notion-update-page', { page_id: 'page-1', content: 'trusted notes' })).toContain('notes/notion-update-page result')
    for (const approval of [false, true, 'cancel'] as const) {
      const session = await create()
      const page = `review-${approval}`
      await client.session.prompt({ sessionID: session.id, text: `battery:${JSON.stringify({ name: 'notes_notion-move-pages', input: { page_ids: [page], parent: 'destination' } })}` })
      let form: any
      await ready(async () => {
        form = (await client.session.form.list({ sessionID: 'global' })).find(item => item.metadata?.server === 'appa')
        if (!form) throw new Error('Waiting for review')
      }, server, `${directory}/opencode-error.log`)
      expect(form.metadata.message).toContain(page)
      expect(form.metadata.message).toContain('notion-review')
      expect(calls.some(call => call.arguments.page_ids?.includes(page))).toBe(false)
      if (approval === 'cancel') await client.session.form.cancel({ sessionID: 'global', formID: form.id })
      else await client.session.form.reply({ sessionID: 'global', formID: form.id, answer: { approval } })
      await client.session.wait({ sessionID: session.id })
      expect(calls.filter(call => call.arguments.page_ids?.includes(page))).toHaveLength(approval === true ? 1 : 0)
    }

    const github = await create()
    expect(await call(github.id, 'codehub_get_file_contents', { owner: 'acme', repo: 'private', path: 'secret.txt' })).toContain('codehub/get_file_contents result')
    expect(await call(github.id, 'codehub_create_or_update_file', { owner: 'acme', repo: 'private', path: 'copy.txt', content: 'private data' })).toContain('codehub/create_or_update_file result')
    expect(await call(github.id, 'notes_notion-update-page', { page_id: 'page-1', content: 'shared with the same readers' })).toContain('notes/notion-update-page result')
    const beforePublish = calls.length
    await call(github.id, 'codehub_create_or_update_file', { owner: 'acme', repo: 'public', path: 'leak.txt', content: 'private data' }, false)
    expect(calls.length).toBe(beforePublish)
    const publicReader = await create()
    expect(await call(publicReader.id, 'codehub_get_file_contents', { owner: 'acme', repo: 'public', path: 'README.md' })).toContain('codehub/get_file_contents result')
    const beforeUntrustedWrite = calls.length
    await call(publicReader.id, 'notes_notion-update-page', { page_id: 'page-1', content: 'untrusted public text' }, false)
    expect(calls.length).toBe(beforeUntrustedWrite)
    expect(apiReads).toContain('/repos/acme/private')
    expect(apiReads).toContain('/repos/acme/public')
    expect(apiReads).toContain('/repos/acme/private/collaborators')
    const missing = await create()
    const beforeMissing = calls.length
    await call(missing.id, 'codehub_get_file_contents', { owner: 'acme', repo: 'unavailable', path: 'README.md' }, false)
    expect(calls.length).toBe(beforeMissing)
  } finally {
    fixture.stop(true)
  }
}, 120_000)

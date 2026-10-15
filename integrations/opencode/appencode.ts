import { Plugin } from '@opencode/plugin/effect'
import type { ToolHooks } from '@opencode/plugin/effect/tool'
import { Agent } from '@opencode/schema/agent'
import { Model } from '@opencode/schema/model'
import { SessionMessage } from '@opencode/schema/session-message'
import { Tool } from '@opencode/schema/tool'
import { Cause, Effect, Schema, Semaphore } from 'effect'
import { lstat, realpath, stat } from 'node:fs/promises'
import { dirname, isAbsolute, join, relative, resolve, sep } from 'node:path'
import { configuration, guide } from './setup'

type Decision = Record<string, unknown> & { protocol: 1; decision: string }
type Proposal = { tool: string; arguments: unknown; call_id: string }
type Actor = { root: string; child?: string }

function text(decision: Decision, field: string): string {
  const value = decision[field]
  if (typeof value !== 'string') throw new Error(`[appencode] Runtime reply is missing ${field}.`)
  return value
}

function failure(error: unknown): Tool.Error {
  // Transport errors may contain URLs or credentials. Never relay arbitrary errors.
  return new Tool.Error({ message: error instanceof Error && error.message.startsWith('[appencode]')
    ? error.message : '[appencode] Runtime unavailable or invalid reply. Check APPA_OPENCODE_RUNTIME_URL and the runtime logs.' })
}

export default Plugin.define({
  id: 'appa.appencode',
  effect: Effect.fn(function* (ctx) {
    const base = process.env.APPA_OPENCODE_RUNTIME_URL || ctx.options?.runtimeURL || 'http://127.0.0.1:8789'
    const setupConfig = process.env.APPA_OPENCODE_SETUP_CONFIG
    const setup = setupConfig ? yield* Effect.tryPromise(() => configuration(setupConfig, process.env.APPA_OPENCODE_SETUP_BINARY || '')).pipe(Effect.orDie) : undefined
    const setupGuide = setup ? yield* Effect.tryPromise(guide).pipe(Effect.orDie) : undefined
    const pending = new Map<string, { proposal: Proposal; actor: Actor }>()
    const children = new Map<string, {
      root: Tool.Context['sessionID']; promptID: SessionMessage.ID; prompt: string; context: string
    }>()
    const initialPrompts = new Set<string>()
    const launches = new Map<string, Semaphore.Semaphore>()

    async function post(event: string, actor: Actor, fields: Record<string, unknown> = {}): Promise<Decision> {
      if (event === 'tool_call' && actor.child && !children.has(actor.child)) {
        throw new Error('[appencode] Child has no live foreground delegation.')
      }
      const response = await fetch(`${base.replace(/\/$/, '')}/hook`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ protocol: 1, adapter: 'opencode', event,
          root_id: actor.root, ...(actor.child ? { child_id: actor.child } : {}), ...fields }),
        signal: AbortSignal.timeout(30_000),
        redirect: 'error',
      })
      const reply = await response.json()
      if (!reply || reply.protocol !== 1 || typeof reply.decision !== 'string') {
        throw new Error('[appencode] Invalid runtime reply. Check --adapter opencode and the runtime logs.')
      }
      if (reply.decision === 'refuse') throw new Error(`[appencode] ${text(reply, 'detail')}`)
      if (!response.ok) throw new Error(`[appencode] Runtime returned HTTP ${response.status}.`)
      return reply
    }

    async function open(sessionID: ToolHooks['execute.before']['sessionID']) {
      const session = await Effect.runPromise(ctx.session.get({ sessionID }))
      if (session.fork) throw new Error('[appencode] Forked sessions are not supported. Continue the protected root instead.')
      if (session.parentID) {
        const child = children.get(sessionID)
        if (!child || child.root !== session.parentID) {
          throw new Error('[appencode] Child has no live foreground delegation. Resume and alternate launches are refused.')
        }
        await childHistory(sessionID)
        return { actor: { root: child.root, child: sessionID }, context: child.context }
      }
      // Reopening preserves the runtime's pinned policy and Label. No Label lives here.
      const actor: Actor = { root: sessionID }
      const started = await post('session_start', actor)
      if (started.decision !== 'ack' && started.decision !== 'context') {
        throw new Error('[appencode] Unexpected session-start decision.')
      }
      return { actor, context: started.decision === 'context' ? text(started, 'text') : '' }
    }

    async function childHistory(sessionID: Tool.Context['sessionID']) {
      const child = children.get(sessionID)
      if (!child) throw new Error('[appencode] Child has no live foreground delegation.')
      const history = await Effect.runPromise(ctx.session.context({ sessionID }))
      for (const message of history) {
        switch (message.type) {
          case 'user':
            if (message.id !== child.promptID || message.text !== child.prompt) {
              throw new Error('[appencode] Additional child input is not supported.')
            }
            break
          case 'assistant':
          case 'system': break // Harness instruction updates remain trusted, as in roots.
          case 'idle':
            if (message.error) throw new Error('[appencode] Child execution failed; its return is withheld.')
            break
          default: throw new Error('[appencode] Child synthetic input, compaction and alternate work are not supported.')
        }
      }
      return history
    }

    async function inventory() {
      const tools = await Effect.runPromise(ctx.tool.list())
      return tools.map(tool => {
        const namespace = tool.options?.namespace
        // Namespace comes from the registry. Flattened names such as a_b_c are ambiguous.
        if (namespace?.includes('__') || (!namespace && tool.id.startsWith('mcp__'))) {
          throw new Error('[appencode] Tool registry contains an ambiguous wire spelling.')
        }
        return { name: tool.id, tool: namespace ? `mcp__${namespace}__${tool.name}` : tool.id }
      })
    }

    // Prompt admission precedes the host's tool snapshot. Waiting in context or
    // execute.before is too late to add a newly discovered executor to that request.
    const remedyReady = Effect.gen(function* () {
      for (;;) {
        const tools = yield* ctx.tool.list()
        if (tools.some(tool => tool.options?.namespace === 'appa' &&
            tool.name === 'execute_remedy_plan' && tool.options.codemode === false)) return
        const servers = yield* ctx.mcp.list()
        const server = servers.data.find(server => server.name === 'appa')
        if (!server || !['pending', 'connected'].includes(server.status.status)) {
          return yield* Effect.fail(new Tool.Error({ message:
            `[appencode] APPA MCP is ${server?.status.status ?? 'not configured'}. Connect the appa server before protected work.` }))
        }
        yield* Effect.sleep('50 millis')
      }
    }).pipe(Effect.timeoutOrElse({ duration: '10 seconds', orElse: () =>
      Effect.fail(new Tool.Error({ message: '[appencode] APPA remedy tool was not registered within 10 seconds. Check the appa MCP connection and retry the prompt.' })) }),
    Effect.catch(error => Effect.fail(failure(error))))

    const setupDiscovery = Effect.gen(function* () {
      for (;;) {
        const servers = yield* ctx.mcp.list()
        if (!servers.data.some(server => server.status.status === 'pending')) return
        yield* Effect.sleep('50 millis')
      }
    }).pipe(Effect.timeoutOption('10 seconds'))

    function feedback(value: string, tools: Awaited<ReturnType<typeof inventory>>) {
      // Only runtime-authored feedback is respelled; admitted values stay byte-for-byte intact.
      for (const tool of [...tools].sort((a, b) => b.tool.length - a.tool.length)) {
        value = value.split(tool.tool).join(tool.name)
      }
      // MCP discovery can lag the first denial. The reserved control has a known
      // V2 spelling even before its registry entry appears.
      return value.split('mcp__appa__execute_remedy_plan').join('appa_execute_remedy_plan')
    }

    yield* ctx.tool.transform(editor => {
      editor.remove('subagent')
      if (setup) editor.add({
        name: 'appa_setup',
        options: { codemode: false },
        description: 'Inspect AppenCode policy and live tools, read a catalog battery, or publish a complete approved policy. Publish only after a later operator message approves your proposal. Configuration access only; no coding or shell execution.',
        input: Schema.Struct({
          operation: Schema.Literals(['inspect', 'battery', 'publish']),
          battery: Schema.optionalKey(Schema.String),
          policy: Schema.optionalKey(Schema.String),
          expected_digest: Schema.optionalKey(Schema.String),
        }),
        output: Schema.String,
        execute: (input, call) => Effect.gen(function* () {
          const session = yield* ctx.session.get({ sessionID: call.sessionID })
          if (session.parentID || session.fork) return yield* Effect.fail(new Tool.Error({ message: 'Setup requires a fresh root operator session.' }))
          if (input.operation !== 'battery') yield* setupDiscovery
          const servers = yield* ctx.mcp.list()
          const tools = yield* ctx.tool.list()
          const output = yield* Effect.tryPromise({ try: () => setup(input, tools.map(tool => ({ name: tool.id, description: tool.description,
            canonical: tool.options?.namespace === 'appa' && tool.name === 'execute_remedy_plan' ? 'appa/execute_remedy_plan'
              : tool.options?.namespace ? `mcp/${tool.options.namespace}/${tool.name}` : `host/opencode/${tool.name}` })),
          servers.data.map(server => ({ name: server.name, status: server.status.status }))),
          catch: error => new Tool.Error({ message: error instanceof Error ? error.message : 'AppenCode setup failed.' }) })
          return { output, content: output }
        }).pipe(Effect.catch(error => Effect.fail(error instanceof Tool.Error ? error : failure(error)))),
      })
      editor.add({
        name: 'appa_subagent',
        options: { codemode: false },
        description: 'Delegate to a fresh foreground subagent through APPA. Declare its return using the offered remedy, then retry. APPA authorizes delegation; native subagent permissions do not apply. No background, resume, or nested delegation.',
        input: Schema.Struct({
          agent: Schema.String,
          description: Schema.String,
          prompt: Schema.String,
          model: Schema.optionalKey(Schema.String),
          sessionID: Schema.optionalKey(Schema.String),
          background: Schema.optionalKey(Schema.Boolean),
        }),
        output: Schema.String,
        execute: (input, call) => Effect.gen(function* () {
          if (children.has(call.sessionID) || input.sessionID !== undefined || input.background === true) {
            throw new Error('[appencode] Only fresh, one-level foreground delegation is supported.')
          }
          yield* Effect.tryPromise({ try: () => open(call.sessionID), catch: failure })
          const parent = yield* ctx.session.get({ sessionID: call.sessionID })
          const agents = yield* ctx.agent.list()
          const agent = agents.data.find(agent => agent.id === input.agent || agent.name === input.agent)
          if (!agent || agent.mode === 'primary' || agent.hidden) {
            throw new Error('[appencode] Requested subagent is unavailable.')
          }
          let model = agent.model ?? parent.model
          if (input.model !== undefined) {
            const requested = Model.Ref.parse(input.model)
            const available = yield* ctx.model.list()
            const selected = available.data.find(item => item.providerID === requested.providerID && item.id === requested.id)
            if (!selected || (requested.variant !== undefined && !selected.variants.some(item => item.id === requested.variant))) {
              throw new Error('[appencode] Requested model is unavailable.')
            }
            model = requested
          }
          const proposal: Proposal = { tool: 'appa_subagent', arguments: structuredClone(input), call_id: call.id }
          const tools = yield* Effect.tryPromise({ try: inventory, catch: failure })
          const runtime = (event: string, fields: Record<string, unknown> = {}) =>
            Effect.tryPromise({ try: () => post(event, { root: call.sessionID }, fields), catch: failure })
          const releaseFields = { ...proposal, cwd: ctx.location.directory, inventory: { tools } }
          let release: 'unattempted' | 'uncertain' | 'approved' = 'unattempted'
          let settled = false
          let childID: Tool.Context['sessionID'] | undefined
          const cleanup = Effect.gen(function* () {
            if (childID) {
              children.delete(childID)
              initialPrompts.delete(childID)
              yield* ctx.session.interrupt({ sessionID: childID, resume: false }).pipe(Effect.catchCause(() => Effect.void))
              yield* ctx.session.wait({ sessionID: childID }).pipe(Effect.catchCause(() => Effect.void))
            }
            if (settled) return
            if (release === 'uncertain') {
              // A lost reply is not a lost release. Replay the exact occurrence,
              // then settle it without launching any work.
              const replay = yield* runtime('tool_call', releaseFields)
              release = replay.decision === 'allow_call' ? 'approved' : 'unattempted'
            }
            if (release === 'approved') {
              if (childID) yield* runtime('child_end', { child_id: childID }).pipe(Effect.catchCause(() => Effect.void))
              const ended = yield* runtime('spawn_result', { ...proposal, spawned_id: childID, outcome: { status: 'indeterminate' } })
              settled = ended.decision === 'ack'
            }
          }).pipe(Effect.catchCause(() => Effect.void))
          let launch = launches.get(call.sessionID)
          if (!launch) { launch = Semaphore.makeUnsafe(1); launches.set(call.sessionID, launch) }
          return yield* Effect.gen(function* () {
            const child = yield* launch.withPermits(1)(Effect.gen(function* () {
              release = 'uncertain'
              const decision = yield* runtime('tool_call', releaseFields)
              release = decision.decision === 'allow_call' ? 'approved' : 'unattempted'
              if (decision.decision !== 'allow_call') {
                throw new Error(`[appencode] ${feedback(text(decision, decision.decision === 'deny_call' ? 'feedback' : 'reason'), tools)}`)
              }
              const binding = text(decision, 'spawn_binding')
              if (!binding) throw new Error('[appencode] Spawn approval has no binding.')
              const child = yield* ctx.session.create({
                parentID: call.sessionID, agent: Agent.ID.make(agent.id), model, title: input.description,
              })
              childID = child.id
              if (child.parentID !== call.sessionID || child.fork) throw new Error('[appencode] Invalid child lineage.')
              const started = yield* runtime('child_start', { child_id: child.id, spawn_binding: binding })
              if (started.decision !== 'ack' && started.decision !== 'context') throw new Error('[appencode] Unexpected child-start decision.')
              const owned = { id: child.id, root: call.sessionID, promptID: SessionMessage.ID.create(), prompt: input.prompt,
                context: started.decision === 'context' ? text(started, 'text') : '' }
              children.set(child.id, owned)
              return owned
            }).pipe(Effect.onError(() => cleanup), Effect.uninterruptible))
            initialPrompts.add(child.id)
            yield* ctx.session.prompt({ sessionID: child.id, id: child.promptID, text: child.prompt })
            yield* ctx.session.wait({ sessionID: child.id })
            const history = yield* Effect.tryPromise({ try: () => childHistory(child.id), catch: failure })
            const answer = [...history].reverse().find(message => message.type === 'assistant')
            if (!answer || answer.type !== 'assistant' || answer.error || !answer.time.completed || answer.finish !== 'stop') {
              throw new Error('[appencode] Child did not complete a final answer; its return is withheld.')
            }
            let value = answer.content.filter(part => part.type === 'text').map(part => part.text).join('\n')
            const returned = yield* runtime('child_end', { child_id: childID, value })
            if (returned.decision === 'child_return') {
              value = text(returned, 'value')
              const echoed = yield* runtime('child_end', { child_id: childID, value })
              if (echoed.decision !== 'ack') throw new Error('[appencode] Replacement return was not admitted.')
            } else if (returned.decision !== 'ack') {
              throw new Error(`[appencode] ${feedback(text(returned, 'reason'), tools)}`)
            }
            const replay = yield* runtime('spawn_result', { ...proposal, spawned_id: childID, value,
              outcome: { status: 'success_without_body' } })
            if (replay.decision !== 'ack') throw new Error('[appencode] Foreground return was not confirmed.')
            settled = true
            // No native Job or synthetic observer exists. Every result channel starts clean.
            return { output: value, content: value }
          }).pipe(Effect.ensuring(cleanup))
        }).pipe(Effect.catchCause(cause => {
          if (Cause.hasInterrupts(cause)) return Effect.interrupt
          return Effect.fail(failure(Cause.squash(cause)))
        })),
      })
      for (const tool of editor.list()) {
        editor.update(tool.id, item => {
          // Code Mode has a host-side fetch extension without a public admission hook.
          // Keep tools useful as ordinary direct calls instead of enabling that bypass.
          const { pinned: _pinned, ...options } = item.options ?? {}
          item.options = { ...options, codemode: false }
        })
      }
    })

    yield* ctx.permission.hook('evaluate', event => Effect.sync(() => {
      if (event.action === 'execute' || event.action === 'subagent') {
        event.effect = 'deny'
        event.message = 'Use APPA foreground delegation, not Code Mode or native subagent returns.'
      }
    }))

    yield* ctx.session.hook('prompt', event => Effect.tryPromise({
      try: async () => {
        const session = await Effect.runPromise(ctx.session.get({ sessionID: event.sessionID }))
        if (session.fork) throw new Error('[appencode] Forked sessions are not supported.')
        if (!session.parentID) return
        const child = children.get(event.sessionID)
        if (!child || !initialPrompts.delete(event.sessionID) || event.messageID !== child.promptID ||
            event.prompt.text !== child.prompt || event.prompt.files?.length || event.prompt.agents?.length || event.prompt.skills?.length) {
          throw new Error('[appencode] Child prompts require an exact live foreground delegation; steering and alternate launches are refused.')
        }
      }, catch: failure,
    }).pipe(Effect.andThen(setup ? Effect.void : remedyReady),
      Effect.tapError(error => Effect.logError(error.message)), Effect.orDie))

    for (const kind of ['compaction', 'generate', 'title'] as const) {
      yield* ctx.session.hook(kind, event => Effect.tryPromise({
        try: async () => {
          const session = await Effect.runPromise(ctx.session.get({ sessionID: event.sessionID }))
          if (session.parentID || session.fork) throw new Error('[appencode] Auxiliary child and fork requests are not supported.')
        }, catch: failure,
      }).pipe(Effect.orDie))
    }

    yield* ctx.session.hook('context', event => Effect.tryPromise({
      try: async () => {
        const { context } = await open(event.sessionID)
        delete event.tools.execute
        delete event.tools.subagent
        if (children.has(event.sessionID)) delete event.tools.appa_subagent
        if (setup) {
          for (const name of Object.keys(event.tools)) if (name !== 'appa_setup') delete event.tools[name]
        }
        event.system.push({ type: 'text', text: setupGuide || context ||
          'AppenCode checks direct tool flows through APPA. Use the offered remedy tool, then retry. Do not bypass refused flows.' })
      },
      catch: failure,
    }).pipe(Effect.orDie))

    yield* ctx.tool.hook('execute.before', event => Effect.tryPromise({
      try: async () => {
        if (event.tool === 'appa_subagent') {
          await open(event.sessionID)
          if (children.has(event.sessionID)) throw new Error('[appencode] Nested delegation is not supported.')
          return // The owned executor releases and settles the spawn in one Effect scope.
        }
        if (event.tool === 'execute' || event.tool === 'subagent') {
          throw new Error('[appencode] Code Mode and native subagents are refused. Use direct tools or appa_subagent.')
        }
        if (event.tool === 'shell' && typeof event.input === 'object' && event.input !== null &&
            'background' in event.input && event.input.background === true) {
          throw new Error('[appencode] Background shell results use an unchecked notification path. Use a foreground call.')
        }
        const { actor } = await open(event.sessionID)
        const tools = await inventory()
        const tool = tools.find(tool => tool.name === event.tool)
        if (!tool) throw new Error('[appencode] Tool is absent from the current registry.')
        if (event.tool === 'read') {
          event.input = await checkedRead(event.input, ctx.location.directory)
        }
        const proposal = { tool: tool.tool, arguments: structuredClone(event.input), call_id: event.id }
        const decision = await post('tool_call', actor, {
          ...proposal, cwd: ctx.location.directory, inventory: { tools },
        })
        switch (decision.decision) {
          case 'allow_call':
          case 'pass_control':
            pending.set(`${event.sessionID}:${event.id}`, { proposal, actor })
            return
          case 'deny_call':
            throw new Error(`[appencode] ${feedback(text(decision, 'feedback'), tools)}`)
          case 'block':
            throw new Error(`[appencode] ${feedback(text(decision, 'reason'), tools)}`)
          default:
            throw new Error('[appencode] Unexpected tool-call decision.')
        }
      },
      catch: failure,
    }))

    yield* ctx.tool.hook('execute.after', event => Effect.promise(async () => {
      if (event.tool === 'appa_subagent') return // Only runtime-checked or constant error bytes leave its executor.
      const key = `${event.sessionID}:${event.id}`
      const approved = pending.get(key)
      pending.delete(key)
      const replace = (value: string) => {
        // Replace structured output, text, attachments, metadata and error causes together.
        if (event.status === 'completed') event.result = { output: value, content: value }
        else event.error = new Tool.Error({ message: value })
      }
      try {
        if (!approved) throw new Error('[appencode] No local approval for this result; it is withheld.')
        const { proposal, actor } = approved
        if (JSON.stringify(proposal.arguments) !== JSON.stringify(event.input)) {
          throw new Error('[appencode] Tool arguments changed after approval; the result is withheld.')
        }
        const outcome = event.status === 'error'
          ? { status: 'failure', message: JSON.stringify(event.error) }
          : { status: 'success', body: event.result }
        const decision = await post('tool_result', actor, { ...proposal, outcome })
        switch (decision.decision) {
          case 'ack': return
          case 'deliver_value': replace(text(decision, 'value')); return
          case 'replace_output': replace(feedback(text(decision, 'output'), await inventory())); return
          case 'block': replace(feedback(text(decision, 'reason'), await inventory())); return
          default: throw new Error('[appencode] Unexpected tool-result decision.')
        }
      } catch (error) {
        replace(`${failure(error).message} Tool result withheld.`)
      }
    }))
  }),
})

async function checkedRead(input: unknown, directory: string) {
  if (!input || typeof input !== 'object' || !('path' in input) || typeof input.path !== 'string') {
    throw new Error('[appencode] Read requires a path.')
  }
  // Resolve aliases before policy selection. Missing paths are refused rather than
  // letting the native reader select an unchecked alternate spelling.
  const path = await realpath(resolve(directory, input.path)).catch(() => {
    throw new Error('[appencode] Cannot resolve the read path. Check that it exists and is accessible; alternate filenames are refused in v1.')
  })
  const root = await realpath(directory)
  const within = relative(root, path)
  // External reads do not trigger OpenCode's nearby instruction discovery.
  if (path === root || (!isAbsolute(within) && within !== '..' && !within.startsWith(`..${sep}`))) {
    let current = (await stat(path)).isDirectory() ? path : dirname(path)
    while (current !== root && current !== dirname(current)) {
      const instruction = join(current, 'AGENTS.md')
      if (instruction !== path) {
        try {
          await lstat(instruction)
          throw new Error('[appencode] This read would inject unchecked nearby AGENTS.md instructions. V1 refuses it.')
        } catch (error) {
          if (!error || typeof error !== 'object' || !('code' in error) || error.code !== 'ENOENT') throw error
        }
      }
      current = dirname(current)
    }
  }
  return { ...input, path }
}

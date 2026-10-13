import { Plugin } from '@opencode/plugin/effect'
import type { ToolHooks } from '@opencode/plugin/effect/tool'
import { Tool } from '@opencode/schema/tool'
import { Effect } from 'effect'
import { lstat, realpath, stat } from 'node:fs/promises'
import { dirname, isAbsolute, join, relative, resolve, sep } from 'node:path'

type Decision = Record<string, unknown> & { protocol: 1; decision: string }
type Proposal = { tool: string; arguments: unknown; call_id: string }

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
    const base = process.env.APPA_OPENCODE_RUNTIME_URL || 'http://127.0.0.1:8789'
    const pending = new Map<string, Proposal>()

    async function post(event: string, root: string, fields: Record<string, unknown> = {}): Promise<Decision> {
      const response = await fetch(`${base.replace(/\/$/, '')}/hook`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ protocol: 1, adapter: 'opencode', event, root_id: root, ...fields }),
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
      if (session.parentID || session.fork) {
        throw new Error('[appencode] V1 does not support child or forked sessions. Continue the protected root instead.')
      }
      // Reopening preserves the runtime's pinned policy and Label. No Label lives here.
      const started = await post('session_start', sessionID)
      if (started.decision !== 'ack' && started.decision !== 'context') {
        throw new Error('[appencode] Unexpected session-start decision.')
      }
      return started.decision === 'context' ? text(started, 'text') : ''
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
        event.message = 'AppenCode v1 covers direct tools, not Code Mode or subagent returns.'
      }
    }))

    yield* ctx.session.hook('context', event => Effect.tryPromise({
      try: async () => {
        const context = await open(event.sessionID)
        delete event.tools.execute
        delete event.tools.subagent
        event.system.push({ type: 'text', text: context ||
          'AppenCode checks direct tool flows through APPA. Use the offered remedy tool, then retry. Do not bypass refused flows.' })
      },
      catch: failure,
    }).pipe(Effect.orDie))

    yield* ctx.tool.hook('execute.before', event => Effect.tryPromise({
      try: async () => {
        if (event.tool === 'execute' || event.tool === 'subagent') {
          throw new Error('[appencode] Code Mode and subagents are not supported in v1. Use direct tools.')
        }
        if (event.tool === 'shell' && typeof event.input === 'object' && event.input !== null &&
            'background' in event.input && event.input.background === true) {
          throw new Error('[appencode] Background shell results use an unchecked notification path. Use a foreground call.')
        }
        await open(event.sessionID)
        const tools = await inventory()
        const tool = tools.find(tool => tool.name === event.tool)
        if (!tool) throw new Error('[appencode] Tool is absent from the current registry.')
        if (event.tool === 'read') {
          event.input = await checkedRead(event.input, ctx.location.directory)
        }
        const proposal = { tool: tool.tool, arguments: structuredClone(event.input), call_id: event.id }
        const decision = await post('tool_call', event.sessionID, {
          ...proposal, cwd: ctx.location.directory, inventory: { tools },
        })
        switch (decision.decision) {
          case 'allow_call':
          case 'pass_control':
            pending.set(`${event.sessionID}:${event.id}`, proposal)
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
      const key = `${event.sessionID}:${event.id}`
      const proposal = pending.get(key)
      pending.delete(key)
      const replace = (value: string) => {
        // Replace structured output, text, attachments, metadata and error causes together.
        if (event.status === 'completed') event.result = { output: value, content: value }
        else event.error = new Tool.Error({ message: value })
      }
      try {
        if (!proposal) throw new Error('[appencode] No local approval for this result; it is withheld.')
        if (JSON.stringify(proposal.arguments) !== JSON.stringify(event.input)) {
          throw new Error('[appencode] Tool arguments changed after approval; the result is withheld.')
        }
        const outcome = event.status === 'error'
          ? { status: 'failure', message: JSON.stringify(event.error) }
          : { status: 'success', body: event.result }
        const decision = await post('tool_result', event.sessionID, { ...proposal, outcome })
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

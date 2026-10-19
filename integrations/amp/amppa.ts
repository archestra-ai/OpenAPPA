import type { PluginAPI, ToolCallEvent } from '@ampcode/plugin'

export const description = 'amppa: APPA information-flow policy for Amp tool calls and results.'

type Decision = Record<string, unknown> & { protocol: 1; decision: string }

// Which operational fault a refuse names, as the runtime spells it (RefusalClass in appa-runtime-api).
// amppa installs as this one file, so it carries its own copy; its tests hold it to the runtime's list.
const REFUSAL_CLASSES = [
  'storage', 'untrusted_log', 'policy_unavailable', 'policy_moved', 'inventory_refused', 'engine_invariant',
  'contended', 'resolution_diverged', 'annotation_refused', 'undeclared_tool', 'undeclared_spawn', 'peer_message',
  'principal', 'outstanding_call', 'call_id_reused', 'trajectory_ended', 'trajectory', 'dispatch', 'remedy',
  'spawn_not_taken', 'spawn',
] as const

type RefusalClass = (typeof REFUSAL_CLASSES)[number]
type Refusal = { class: RefusalClass; detail: string }

function isRefusalClass(value: unknown): value is RefusalClass {
  return (REFUSAL_CLASSES as readonly unknown[]).includes(value)
}

/** A refuse decision's class and detail; undefined when either is outside the wire. */
function refusal(reply: Record<string, unknown>): Refusal | undefined {
  const { class: refused, detail } = reply
  return isRefusalClass(refused) && typeof detail === 'string' ? { class: refused, detail } : undefined
}

function text(decision: Decision, field: string): string {
  const value = decision[field]
  if (typeof value !== 'string') throw new Error(`[amppa] Runtime reply is missing ${field}.`)
  return value
}

export default function amppa(amp: PluginAPI) {
  // Separate from clappa's endpoint so both adapters can run on the same machine.
  const base = process.env.APPA_AMP_RUNTIME_URL || 'http://127.0.0.1:8788'
  // Amp emits tool.result even when this plugin rejected the call. Those are
  // host-synthesized messages, not outcomes of runtime dispatches.
  const rejected = new Map<string, Map<string, string>>()

  function reject(event: ToolCallEvent, message: string, action: 'reject-and-continue' | 'error') {
    const calls = rejected.get(event.thread.id) ?? new Map<string, string>()
    calls.set(event.toolUseID, message)
    rejected.set(event.thread.id, calls)
    return { action, message }
  }

  async function post(event: string, root: string, fields: Record<string, unknown> = {}): Promise<Decision> {
    const response = await fetch(`${base.replace(/\/$/, '')}/hook`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ protocol: 1, adapter: 'amp', event, root_id: root, ...fields }),
      signal: AbortSignal.timeout(30_000),
      redirect: 'error',
    })
    const reply = await response.json()
    const invalid = new Error(`[amppa] Invalid runtime reply (HTTP ${response.status}). Check the runtime logs and --adapter amp.`)
    if (!reply || reply.protocol !== 1 || typeof reply.decision !== 'string') throw invalid
    if (reply.decision === 'refuse') {
      const refused = refusal(reply)
      if (!refused) throw invalid
      throw new Error(`[amppa] ${refused.detail} (${refused.class})`)
    }
    if (!response.ok) throw new Error(`[amppa] Runtime returned HTTP ${response.status}.`)
    return reply
  }

  function failure(error: unknown): string {
    // Fetch errors can include the endpoint and credentials. Never echo arbitrary errors.
    return error instanceof Error && error.message.startsWith('[amppa]')
      ? error.message
      : '[amppa] Runtime unavailable or unreadable reply. Check APPA_AMP_RUNTIME_URL and the runtime logs.'
  }

  amp.on('agent.start', async (event, ctx) => {
    try {
      // Reopening an existing root preserves its label and policy. No label lives in
      // plugin memory, so compaction, plugin reload and orb resume cannot reset it.
      const started = await post('session_start', event.thread.id)
      if (started.decision !== 'ack' && started.decision !== 'context') {
        throw new Error('[amppa] Unexpected session-start decision.')
      }
      const prompted = await post('prompt', event.thread.id, { text: event.message })
      if (prompted.decision !== 'ack') throw new Error('[amppa] Unexpected prompt decision.')
      return {
        message: {
          content: started.decision === 'context'
            ? text(started, 'text')
            : 'amppa checks tool flows through APPA. Follow policy feedback; do not bypass a blocked flow through another tool.',
        },
      }
    } catch (error) {
      await ctx.thread.cancel()
      await ctx.ui.notify(failure(error))
      return {}
    }
  })

  amp.on('tool.call', async (event) => {
    try {
      const decision = await post('tool_call', event.thread.id, {
        tool: event.tool,
        arguments: event.input,
        call_id: event.toolUseID,
        cwd: amp.helpers.shellCommandFromToolCall(event)?.dir
          ?? (amp.system.workspaceRoot ? amp.helpers.filePathFromURI(amp.system.workspaceRoot) : undefined),
      })
      switch (decision.decision) {
        case 'allow_call':
        case 'pass_control':
          rejected.get(event.thread.id)?.delete(event.toolUseID)
          return { action: 'allow' }
        case 'deny_call':
          return reject(event, text(decision, 'feedback'), 'reject-and-continue')
        case 'block':
          return reject(event, text(decision, 'reason'), 'reject-and-continue')
        default:
          throw new Error('[amppa] Unexpected tool-call decision.')
      }
    } catch (error) {
      return reject(event, failure(error), 'error')
    }
  })

  amp.on('tool.result', async (event) => {
    const calls = rejected.get(event.thread.id)
    const feedback = calls?.get(event.toolUseID)
    if (calls && feedback !== undefined) {
      calls.delete(event.toolUseID)
      if (calls.size === 0) rejected.delete(event.thread.id)
      return { status: 'done', output: feedback, error: '' }
    }
    try {
      // A cancelled call may already have had effects. Report uncertainty, not failure.
      // Failure messages include *all* model-visible result data, not just event.error.
      const outcome = event.status === 'cancelled'
        ? { status: 'indeterminate' }
        : event.status === 'error'
          ? { status: 'failure', message: JSON.stringify({ error: event.error, output: event.output }) }
          : event.output === undefined
            ? { status: 'success_without_body' }
            : { status: 'success', body: event.output }
      const decision = await post('tool_result', event.thread.id, {
        tool: event.tool,
        arguments: event.input,
        call_id: event.toolUseID,
        outcome,
      })
      switch (decision.decision) {
        case 'ack':
          if (event.status === 'cancelled') {
            return { status: 'cancelled', error: '[amppa] Tool cancelled; partial output withheld.', output: '' }
          }
          return
        case 'deliver_value':
          // These are the admitted bytes, not a JSON envelope to parse or merge.
          return { status: 'done', output: text(decision, 'value'), error: '' }
        case 'replace_output':
          return { status: 'done', output: text(decision, 'output'), error: '' }
        case 'block':
          return { status: 'error', error: text(decision, 'reason'), output: '[amppa] Tool result withheld.' }
        default:
          throw new Error('[amppa] Unexpected tool-result decision.')
      }
    } catch (error) {
      // Explicitly replace both channels. Throwing alone could leave the original
      // result visible if a host treats plugin exceptions as advisory.
      return { status: 'error', error: failure(error), output: '[amppa] Tool result withheld.' }
    }
  })

  amp.on('agent.end', async (event, ctx) => {
    rejected.delete(event.thread.id)
    try {
      const decision = await post('turn_end', event.thread.id)
      if (decision.decision !== 'ack') throw new Error('[amppa] Unexpected turn-end decision.')
    } catch (error) {
      await ctx.ui.notify(failure(error))
    }
  })
}

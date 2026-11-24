import { spawn } from 'node:child_process'
import type { PluginAPI, ToolCallEvent } from '@ampcode/plugin'

export const description = 'amppa: APPA information-flow policy for Amp tool calls and results.'

type Decision = Record<string, unknown> & { protocol: 1; decision: string }

// Which operational fault a refuse names, as the runtime spells it (RefusalClass in appa-runtime-api).
// amppa installs as this one file, so it carries its own copy; its tests hold it to the runtime's list.
const REFUSAL_CLASSES = [
  'storage', 'untrusted_log', 'policy_unavailable', 'policy_moved', 'legacy_family', 'inventory_refused',
  'engine_invariant', 'contended', 'launch_deciding', 'resolution_diverged', 'annotation_refused',
  'undeclared_tool', 'undeclared_spawn', 'peer_message', 'principal', 'outstanding_call', 'call_id_reused',
  'return_retry_changed', 'trajectory_ended', 'trajectory', 'dispatch', 'remedy', 'spawn_not_taken', 'spawn',
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

// SafeBash as the file-tracker serves it (`appa file-tracker mcp` lists the same tool). amppa
// installs as this one file, so it carries its own copy; its tests hold it to the binary's.
export const SAFEBASH = {
  name: 'appa_safebash',
  description:
    'Runs one shell command in a sandbox. The command can read only the files, directories and Unix ' +
    'sockets listed in input_files (plus system directories such as /usr and /etc, the directories on ' +
    'PATH and installed toolchains), write only the files and directories listed in output_files, and ' +
    'connect only to the hosts listed in network_hosts. Use absolute paths. An output file that ' +
    'exists is read as well, so a command may rewrite it. An output written with a trailing / is a ' +
    'directory: the command reads what it holds and writes under it, as a build directory, ' +
    'node_modules/ or a cache needs (for example ~/.cache/go-build/). A command that reaches a Go, ' +
    "cargo or npm registry keeps what it downloads in that package manager's cache, which every later " +
    'command reads without declaring it. A credential file such as .env inside a declared directory ' +
    'stays hidden unless the call names it or git tracks it. Declare every file the command reads or ' +
    'writes: any other file in your workspace, home or /tmp is hidden from it, so ls and find there ' +
    'show only what the call declares. $TMPDIR is a private directory for this call alone, removed ' +
    'when it ends. To list a directory, declare it in input_files. Declare a socket such as ' +
    '/var/run/docker.sock or $SSH_AUTH_SOCK in input_files to reach the service behind it.',
  inputSchema: {
    type: 'object',
    properties: {
      command: { type: 'string', description: 'The shell command to run with bash -c.' },
      input_files: { type: 'array', items: { type: 'string' }, description: 'Absolute paths of the files the command reads.' },
      output_files: { type: 'array', items: { type: 'string' }, description: 'Absolute paths of the files the command writes.' },
      network_hosts: {
        type: 'array', items: { type: 'string' },
        description: 'Hosts the command connects to: example.com, *.example.com, or host:port.',
      },
    },
    required: ['command', 'input_files', 'output_files', 'network_hosts'],
  },
}

// The variables an orb routes its traffic through `appa amp-proxy` with. A SafeBash call's hosts
// are checked with the call itself, and its sandbox's own proxy admits only those, so its traffic
// does not cross the orb proxy a second time under the orb's label.
const PROXY_VARIABLE = /^(https?|all)_proxy$/i

// What a confined command's failure means, said beside its result, so the model declares what it
// lacked instead of concluding that the file is gone or the tool cannot work here. Adapted from
// clappa's preload (appa-file-tracker/claude-bash.js), whose agentic tests found each of these.
const HINTS: { command?: RegExp; output?: RegExp; text: string }[] = [
  {
    command: /(^|[\s;&|(`])(ls|find|tree|fd|du|git\s+(status|diff|ls-files|add|clean|stash))\b|\*/,
    text: '[appa] This command saw only its declared input_files: undeclared files were hidden, so a listing or git status here is incomplete. To see a directory, declare it in input_files.',
  },
  {
    output: /^(?!.*config file).*(read-only file system|EROFS)/im,
    text: '[appa] A write to a path not in output_files fails as a read-only file system. Declare the path in output_files, or its directory with a trailing /, such as node_modules/.vite/ or .git/.',
  },
  {
    output: /could not (lock|write) config file/,
    text: '[appa] The sandbox keeps .git/config read-only, even in a declared directory: a setting there, such as core.hooksPath or core.fsmonitor, would run code outside the sandbox. So git config and git remote add fail, and git push -u pushes but sets no upstream. Tell the person the git command to run themselves, such as git branch --set-upstream-to=origin/<branch>.',
  },
  {
    output: /^(?!.*config file).*(Device or resource busy|EBUSY)/m,
    text: `[appa] A declared output file can be rewritten but not replaced, renamed or removed, so a tool that writes a temporary file and renames it over the output fails. Write through the file instead: sed 's/a/b/' f > "$TMPDIR/f" && cat "$TMPDIR/f" > f in place of sed -i. For a tool that always replaces the file, such as cargo add, declare the file's directory with a trailing / in output_files.`,
  },
  {
    output: /not a git repository/,
    text: "[appa] Git found no repository because the repository's root, the directory that holds .git, is not in input_files. Declare the root in input_files, and its .git/ in output_files if git writes.",
  },
  {
    output: /cannot lock ref|unable to create directory for .*\.git\/|Unable to create '[^']*\.lock'|unable to (create temporary|write new index)/,
    text: "[appa] Git writes branches, its index and new objects under .git. When the repository's .git/ is not in output_files, it is read-only to this command, so git commit, git checkout -b and git switch fail. Declare it with a trailing /, such as <repository>/.git/, beside the files the command changes.",
  },
]

/** `report` with the hints its command and output call for. */
export function hinted(command: unknown, report: string): string {
  const hints = HINTS.filter(hint => hint.command
    ? hint.command.test(typeof command === 'string' ? command : '')
    : hint.output!.test(report))
  return [report, ...hints.map(hint => hint.text)].join('\n')
}

/** Run one SafeBash call in its sandbox: the command's report, or a refusal that ran nothing. */
function safebash(input: Record<string, unknown>, signal: AbortSignal): Promise<string> {
  const env = Object.fromEntries(Object.entries(process.env).filter(([name]) => !PROXY_VARIABLE.test(name)))
  return new Promise((resolve, reject) => {
    const child = spawn(process.env.APPA_AMP_BINARY || 'appa', ['file-tracker', 'run'], { env, signal })
    let stdout = ''
    let stderr = ''
    child.stdout.on('data', chunk => { stdout += chunk })
    child.stderr.on('data', chunk => { stderr += chunk })
    child.on('error', () => reject(new Error('[amppa] SafeBash could not start. Check APPA_AMP_BINARY.')))
    child.on('close', code => {
      if (code !== 0) return reject(new Error(`[amppa] SafeBash refused: ${stderr.trim().replace(/^appa file-tracker: /, '')}`))
      try {
        resolve(hinted(input.command, JSON.parse(stdout)))
      } catch {
        reject(new Error('[amppa] SafeBash sent an unreadable result.'))
      }
    })
    child.stdin.end(JSON.stringify(input))
  })
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
      if (!refused || response.status !== 409) throw invalid
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

  // The shell this policy runs commands in: confined to the files and hosts each call declares,
  // so its result carries only their labels. Exposed directly, so each call reaches tool.call.
  amp.registerTool({
    ...SAFEBASH,
    defer: false,
    title: 'SafeBash',
    execute: (input, ctx) => safebash(input, ctx.signal),
  })

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

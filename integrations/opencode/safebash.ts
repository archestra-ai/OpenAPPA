// appa_safebash: the only shell AppenCode exposes. `appa file-tracker run` runs each
// released call in the Anthropic Sandbox Runtime (`srt`), confined to its declared
// input_files, output_files and network_hosts: the runner amppa and clappa's SafeBash use.
// The file-tracker annotator and recorder see the same canonical arguments. A command the
// person types at the prompt (`!`) runs in the file tracker's typed confinement, in srt.
import { spawn, spawnSync } from 'node:child_process'
import { lstat, mkdir, mkdtemp, readFile, realpath, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { basename, dirname, join, relative, resolve, isAbsolute } from 'node:path'

export type SafeBash = {
  readonly command: string
  readonly input_files: readonly string[]
  readonly output_files: readonly string[]
  readonly network_hosts: readonly string[]
}

/** The file tracker's confinement for a typed command; see `appa file-tracker sandbox`. */
type Confinement = {
  filesystem: { allowRead: string[]; denyRead: string[]; allowWrite: string[]; denyWrite: string[] }
  network: { allowedDomains: string[] }
  prelude: string
  hiddenEnvironment: string[]
  tmpdir: string
}

export const DESCRIPTION = `Run one shell command in a sandbox with bash -c. The command reads only the files, directories and Unix sockets in input_files, plus system directories such as /usr and /etc, the directories on PATH and installed toolchains. It writes only the files and directories in output_files, and connects only to the hosts in network_hosts (example.com, *.example.com, or host:port). Relative paths resolve from the project directory.

Every other file in the workspace, the home directory and /tmp is hidden, so ls, find and globs there show only what the call declares; an empty listing does not mean a file is missing. To list a directory, declare it in input_files. A credential file such as .env inside a declared directory stays hidden unless the call names it or git tracks it. Declare a socket such as /var/run/docker.sock in input_files to reach the service behind it. $TMPDIR is a private directory for this call alone, removed when it ends.

An output file that exists is read as well, so a command may rewrite it, as npm install does package.json, but not replace, rename or remove it: write through it, as sed 's/a/b/' f > "$TMPDIR/f" && cat "$TMPDIR/f" > f does in place of sed -i. An output written with a trailing / is a directory: the command reads what it holds and writes under it, as a build directory, node_modules/ or a cache needs. Declare a file's directory with a trailing / only for a tool that always replaces the file, such as cargo add.

Name only the hosts the command needs: a host other than a package registry's can lower the session's trust. go get through the Go proxy needs proxy.golang.org and sum.golang.org, not github.com; cargo needs index.crates.io and static.crates.io. A call that names a Go, cargo or npm registry keeps what it downloads in that package manager's cache, which every later command reads without declaring it.

The sandbox keeps a repository's config and hooks read-only, even in a declared directory, so git config, git remote add and the upstream that git push -u sets fail: leave those to the person, in their own terminal.`

// A hostname or *.suffix, with an optional port: the srt allowedDomains grammar
// without bare `*`, IP ranges or brackets.
const HOST = /^(\*\.)?[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?(\.[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?)*(:[1-9][0-9]{0,4})?$/

function strings(value: unknown, field: string): string[] {
  if (!Array.isArray(value) || !value.every(item => typeof item === 'string' && item.length > 0)) {
    throw new Error(`[appencode] appa_safebash requires ${field} as an array of non-empty strings.`)
  }
  return value
}

/** The call's own temporary directory, which the file tracker gives every call and ignores. */
const temporary = (path: string) => /^\$(TMPDIR|\{TMPDIR\})(\/|$)/.test(path)

/** An absolute path with every existing component resolved, as the file tracker resolves
 * it: an existing path whole, an absent one through its nearest existing directory. */
async function canonical(path: string, directory: string) {
  let existing = resolve(directory, path)
  const absent: string[] = []
  for (;;) {
    try {
      return join(await realpath(existing), ...absent.reverse())
    } catch {
      if (dirname(existing) === existing) throw new Error(`[appencode] ${JSON.stringify(path)} names no file.`)
      absent.push(basename(existing))
      existing = dirname(existing)
    }
  }
}

/** An existing output file must be a singly linked regular file: through a symlink or a
 * second hard link, a write would change a path the file tracker does not record. */
async function writable(path: string, what: string) {
  const existing = await lstat(path).catch(error => {
    if (error?.code === 'ENOENT') return undefined
    throw error
  })
  if (existing && (!existing.isFile() || existing.nlink !== 1)) {
    throw new Error(`[appencode] ${what} ${JSON.stringify(path)} must be absent or a singly linked regular file; write a directory with a trailing /.`)
  }
}

/** Native write and edit with the absolute path the file tracker records. */
export async function checkedWrite(input: unknown, directory: string) {
  if (!input || typeof input !== 'object' || !('path' in input) || typeof input.path !== 'string') {
    throw new Error('[appencode] write and edit require a path.')
  }
  const parent = await realpath(dirname(resolve(directory, input.path))).catch(() => {
    throw new Error(`[appencode] path ${JSON.stringify(input.path)} has no existing parent directory.`)
  })
  const path = join(parent, basename(input.path))
  await writable(path, 'path')
  return { ...input, path }
}

/** Canonical arguments: absolute, resolved paths, a trailing / on every directory output,
 * and lower-case hosts. The proposal, the policy, the file tracker and the sandbox all
 * use these exact values. An input may be absent: a probe such as test -f reads nothing. */
export async function checkedSafeBash(input: unknown, directory: string): Promise<SafeBash> {
  if (!input || typeof input !== 'object' || !('command' in input) || typeof input.command !== 'string' || !input.command.trim()) {
    throw new Error('[appencode] appa_safebash requires a command.')
  }
  const fields = input as Record<string, unknown>
  const unknown = Object.keys(fields).filter(key => !['command', 'input_files', 'output_files', 'network_hosts'].includes(key))
  if (unknown.length) throw new Error(`[appencode] appa_safebash does not accept ${unknown.join(', ')}.`)
  const inputs: string[] = []
  for (const path of strings(fields.input_files, 'input_files')) {
    inputs.push(temporary(path) ? path : await canonical(path, directory))
  }
  const outputs: string[] = []
  for (const path of strings(fields.output_files, 'output_files')) {
    if (temporary(path)) { outputs.push(path); continue }
    const resolved = await canonical(path, directory)
    const folder = path.endsWith('/') || (await lstat(resolved).catch(() => undefined))?.isDirectory()
    if (!folder) await writable(resolved, 'appa_safebash output')
    outputs.push(folder && resolved !== '/' ? `${resolved}/` : resolved)
  }
  const hosts = strings(fields.network_hosts, 'network_hosts').map(host => host.toLowerCase())
  const invalid = hosts.find(host => !HOST.test(host))
  if (invalid !== undefined) throw new Error(`[appencode] appa_safebash network host ${JSON.stringify(invalid)} is not a hostname, *.suffix, or host:port.`)
  return { command: input.command, input_files: [...new Set(inputs)], output_files: [...new Set(outputs)], network_hosts: [...new Set(hosts)] }
}

/** The srt executable and its package, for a typed command: the one named, else the one on
 * PATH, else the one `appa` installs into its data directory, where `appa file-tracker run`
 * finds it too. */
async function sandboxRuntime(configured: string | undefined) {
  const data = process.env.APPA_DATA_DIR || join(process.env.XDG_DATA_HOME || join(process.env.HOME || '/', '.local/share'), 'appa')
  const installed = join(data, 'sandbox-runtime/node_modules/.bin/srt')
  const executable = configured || Bun.which('srt') || await lstat(installed).then(() => installed, () => undefined)
  if (!executable) {
    throw new Error('[appencode] Shell commands run in the Anthropic Sandbox Runtime, which is not installed. Install it with npm install -g @anthropic-ai/sandbox-runtime, or set APPA_OPENCODE_SRT.')
  }
  const real = await realpath(executable)
  const root = resolve(dirname(real), '..')
  const manifest = JSON.parse(await readFile(join(root, 'package.json'), 'utf8').catch(() => '{}'))
  if (manifest.name !== '@anthropic-ai/sandbox-runtime') {
    throw new Error('[appencode] APPA_OPENCODE_SRT must name the srt executable of @anthropic-ai/sandbox-runtime.')
  }
  return { executable: real, package: root }
}

/** Where to run: the APPA executable that serves `appa file-tracker`, and the srt a typed
 * command runs in. */
export type Runtime = { readonly appa?: string; readonly srt?: string }

/** The file tracker's confinement for `request`, computed in `cwd`: the toolchains it opens
 * and the repository a typed command works on depend on the working directory. */
function confinement(request: object, cwd: string, appa: string | undefined): Confinement {
  const answer = spawnSync(appa || 'appa', ['file-tracker', 'sandbox'], { cwd, input: JSON.stringify(request), encoding: 'utf8', timeout: 30_000 })
  if (answer.error || answer.status !== 0) {
    const reason = String(answer.stderr || answer.error?.message || 'no answer').trim().replace(/^appa file-tracker: /, '')
    throw new Error(`[appencode] The command was not run: ${reason}`)
  }
  return JSON.parse(answer.stdout)
}

/** The srt settings for `confined`: the tracker's filesystem and network, plus what srt
 * itself needs inside the sandbox: its package, which holds its helpers and which no
 * command writes, and the proxy sockets in `sockets`. */
function settings(confined: Confinement, srt: { package: string }, sockets: string) {
  return JSON.stringify({
    filesystem: { ...confined.filesystem, allowRead: [...confined.filesystem.allowRead, srt.package, sockets],
      denyWrite: [...confined.filesystem.denyWrite, srt.package] },
    network: { allowedDomains: confined.network.allowedDomains, deniedDomains: [] },
  })
}

/** The server's environment without the variables the tracker withholds by name, since
 * they hold credentials. srt places its proxy sockets in TMPDIR; the prelude points the
 * command's own TMPDIR at the call's private directory. */
function environment(confined: Confinement, sockets: string) {
  const env: Record<string, string> = {}
  for (const [name, value] of Object.entries(process.env)) {
    if (value !== undefined && !confined.hiddenEnvironment.includes(name)) env[name] = value
  }
  return { ...env, TMPDIR: sockets }
}

// What a confined command's failure means, said beside its result, so the model declares
// what it lacked instead of concluding that the file is gone or the tool cannot work here.
const HINTS: { command?: RegExp; output?: RegExp; partial?: true; text: string }[] = [
  {
    partial: true,
    command: /(^|[\s;&|(`])(ls|find|tree|fd|du|git\s+(status|diff|ls-files|add|clean|stash))\b|\*/,
    text: '[appa] This command saw only its declared input_files: undeclared files were hidden, so a listing or git status here is incomplete. To see a directory, declare it in input_files.',
  },
  {
    output: /^(?!.*config file).*(read-only file system|EROFS)/im,
    text: '[appa] A write to a path not in output_files fails as a read-only file system. Declare the path in output_files, or its directory with a trailing /, such as node_modules/.vite/ or .git/.',
  },
  {
    output: /could not (lock|write) config file/,
    text: '[appa] The sandbox keeps .git/config read-only, even in a declared directory: a setting there, such as core.hooksPath or core.fsmonitor, would run code outside the sandbox. So git config and git remote add fail, and git push -u pushes but sets no upstream. Tell the person the git command to run in their own terminal.',
  },
  {
    output: /^(?!.*config file).*(Device or resource busy|EBUSY)/m,
    text: `[appa] A declared output file can be rewritten but not replaced, renamed or removed, so a tool that writes a temporary file and renames it over the output fails. Write through the file instead: sed 's/a/b/' f > "$TMPDIR/f" && cat "$TMPDIR/f" > f in place of sed -i. For a tool that always replaces the file, such as cargo add, declare the file's directory with a trailing / in output_files.`,
  },
  {
    output: /not a git repository/,
    text: '[appa] Git found no repository because the repository\'s root, the directory that holds .git, is not in input_files. Declare the root in input_files, and its .git/ in output_files if git writes.',
  },
  {
    output: /cannot lock ref|unable to create directory for .*\.git\/|Unable to create '[^']*\.lock'|unable to (create temporary|write new index)/,
    text: '[appa] Git writes branches, its index and new objects under .git. When the repository\'s .git/ is not in output_files, it is read-only to this command, so git commit, git checkout -b and git switch fail. Declare it with a trailing /, such as <repository>/.git/, beside the files the command changes.',
  },
  {
    output: /^c\S{9}\s.*\s1,\s+3\s/m,
    text: '[appa] An entry listed as character device 1, 3, such as .bashrc or .vscode, is /dev/null, which the sandbox mounts where a protected file does not exist so that the command cannot create it. There is no such file.',
  },
]

/** The hints `call` and what it printed call for. A call that declares the working
 * directory, or a directory that holds it, sees all of it but its credentials. */
function hints(call: SafeBash, cwd: string, printed: string) {
  const whole = call.input_files.some(declared => {
    const within = relative(declared, cwd)
    return !within.startsWith('..') && !isAbsolute(within)
  })
  return HINTS.filter(hint => hint.partial && whole ? false
    : hint.command ? hint.command.test(call.command) : hint.output!.test(printed)).map(hint => hint.text)
}

/** Run one released call with `appa file-tracker run`, the runner amppa and clappa's SafeBash
 * use: it confines the command in srt to what the call declares, withholds credential
 * variables, gives it a private $TMPDIR, and removes the absent outputs it did not write. A
 * command that ran is a result whatever its exit code, never a tool error: the engine applies
 * a call's Label only on success, and the output reaches the model either way. */
export async function runSafeBash(call: SafeBash, cwd: string, runtime: Runtime, signal: AbortSignal) {
  const report = await new Promise<string>((done, fail) => {
    // Its own process group, so an abort stops srt and the command with it.
    const child = spawn(runtime.appa || 'appa', ['file-tracker', 'run'], { cwd, detached: true, stdio: ['pipe', 'pipe', 'pipe'] })
    let stdout = ''
    let stderr = ''
    child.stdout.on('data', chunk => { stdout += chunk })
    child.stderr.on('data', chunk => { stderr += chunk })
    const stop = () => { try { process.kill(-child.pid!, 'SIGKILL') } catch {} }
    signal.addEventListener('abort', stop, { once: true })
    child.on('error', () => fail(new Error('[appencode] appa_safebash could not start appa file-tracker run. Put appa on PATH, or set APPA_FILE_TRACKER.')))
    child.on('close', code => {
      signal.removeEventListener('abort', stop)
      if (signal.aborted) return fail(new Error('[appencode] appa_safebash was cancelled.'))
      if (code !== 0) return fail(new Error(`[appencode] The command was not run: ${stderr.trim().replace(/^appa file-tracker: /, '')}`))
      try { done(JSON.parse(stdout)) } catch { fail(new Error('[appencode] appa file-tracker run sent an unreadable result.')) }
    })
    child.stdin.end(JSON.stringify(call))
  })
  return [report, ...hints(call, cwd, report)].join('\n')
}

/** A command the person types at the prompt (`!`), rewritten to run confined: the tracker
 * gives it the working directory, or the git repository that holds it, no host, and no
 * file whose Label restricts its audience, so its result needs no Label. The script
 * removes its settings and temporary directory when the command ends. */
export async function typedShell(command: string, cwd: string, runtime: Runtime) {
  if (process.platform !== 'linux') throw new Error('[appencode] Typed shell commands run sandboxed on Linux only.')
  const srt = await sandboxRuntime(runtime.srt)
  const confined = confinement({ command, typed: true }, cwd, runtime.appa)
  const run = await mkdtemp(join(tmpdir(), 'appencode-typed-'))
  const sockets = join(run, 'tmp')
  await mkdir(sockets, { mode: 0o700 })
  await writeFile(join(run, 'settings.json'), settings(confined, srt, sockets), { mode: 0o600 })
  const quoted = (text: string) => `'${text.replace(/'/g, `'\\''`)}'`
  return {
    shell: '/bin/sh',
    env: environment(confined, sockets),
    command: `${quoted(srt.executable)} --settings ${quoted(join(run, 'settings.json'))} -c ${quoted(`${confined.prelude}${command}`)}; status=$?; rm -rf ${quoted(run)} ${quoted(confined.tmpdir)}; exit $status`,
  }
}

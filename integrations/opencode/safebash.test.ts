import { afterAll, beforeAll, describe, expect, test } from 'bun:test'
import { link, mkdir, mkdtemp, readFile, readdir, realpath, rm, symlink, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { resolve } from 'node:path'
import { checkedSafeBash, runSafeBash, typedShell } from './safebash'

const runtime = {
  appa: resolve(process.env.APPA_TEST_BINARY || resolve(import.meta.dir, '../../target/debug/appa')),
  srt: resolve(import.meta.dir, 'node_modules/.bin/srt'),
}
let directory: string
const saved = { XDG_CONFIG_HOME: process.env.XDG_CONFIG_HOME, XDG_STATE_HOME: process.env.XDG_STATE_HOME, PATH: process.env.PATH }

beforeAll(async () => {
  directory = await realpath(await mkdtemp(`${tmpdir()}/appencode-safebash-`))
  // The file tracker the sandbox asks reads the person's configuration and store: keep both private.
  process.env.XDG_CONFIG_HOME = `${directory}/config`
  process.env.XDG_STATE_HOME = `${directory}/state`
  // `appa file-tracker run` finds srt on PATH: here the locked devDependency.
  process.env.PATH = `${resolve(import.meta.dir, 'node_modules/.bin')}:${process.env.PATH}`
  await mkdir(`${directory}/project/out`, { recursive: true })
  directory = `${directory}/project`
  await writeFile(`${directory}/in.txt`, 'declared-input')
  await writeFile(`${directory}/.env`, 'UNDECLARED-SECRET')
  await writeFile(`${directory}/out/old.txt`, 'PREVIOUS-OUTPUT')
  await symlink(`${directory}/.env`, `${directory}/link`)
  await writeFile(`${directory}/shared.txt`, 'shared')
  await link(`${directory}/shared.txt`, `${directory}/shared-link.txt`)
})
afterAll(async () => {
  for (const [name, value] of Object.entries(saved)) {
    if (value === undefined) delete process.env[name]
    else process.env[name] = value
  }
  await rm(resolve(directory, '..'), { recursive: true, force: true })
})

const call = (fields: object) => ({ command: 'true', input_files: [], output_files: [], network_hosts: [], ...fields })

describe('canonical SafeBash arguments', () => {
  test('resolves paths and hosts as the file tracker does, so policy, tracker and sandbox see the same values', async () => {
    expect(await checkedSafeBash(call({
      input_files: ['link', './in.txt', 'missing.txt', 'in.txt', '.'],
      output_files: ['out/new.txt', 'out', 'build/cache/', '$TMPDIR/x'],
      network_hosts: ['API.GitHub.com', '*.example.com:443', 'api.github.com'],
    }), directory)).toEqual({
      command: 'true',
      // An absent input reads nothing; a directory input reads what it holds.
      input_files: [`${directory}/.env`, `${directory}/in.txt`, `${directory}/missing.txt`, directory],
      // A directory output, existing or written with a trailing /, keeps the /.
      output_files: [`${directory}/out/new.txt`, `${directory}/out/`, `${directory}/build/cache/`, '$TMPDIR/x'],
      network_hosts: ['api.github.com', '*.example.com:443'],
    })
  })

  test('refuses ambiguous or unenforceable declarations', async () => {
    for (const [fields, message] of [
      // A write through a second hard link would change a path the tracker does not record.
      [{ output_files: ['shared-link.txt'] }, 'singly linked regular file'],
      [{ network_hosts: ['*'] }, 'not a hostname'],
      [{ network_hosts: ['10.0.0.0/8'] }, 'not a hostname'],
      [{ input_files: 'in.txt' }, 'array'],
      [{ cwd: '/' }, 'does not accept cwd'],
      [{ command: ' ' }, 'requires a command'],
    ] as const) {
      await expect(checkedSafeBash(call(fields), directory)).rejects.toThrow(message)
    }
  })
})

// The sandbox needs bubblewrap and user namespaces. Run where they exist.
describe.skipIf(process.platform !== 'linux' || !Bun.which('bwrap'))('srt enforcement of the file tracker\'s confinement', () => {
  const run = async (fields: object) =>
    runSafeBash(await checkedSafeBash(call(fields), directory), directory, runtime, new AbortController().signal)

  test('reads only declared inputs, and writes only declared outputs, which it reads as well', async () => {
    const output = await run({ input_files: ['in.txt'], output_files: ['out/old.txt', 'out/new.txt'],
      command: 'cat in.txt; cat .env; cat out/old.txt; echo written > out/new.txt; echo rewritten >> out/old.txt; echo extra > out/extra.txt; echo x >> in.txt; exit 3' })
    // A command that ran is a result, never a tool error: the error path would skip the inputs' Label.
    expect(output).toContain('exit: 3')
    expect(output).toContain('declared-input')
    expect(output).not.toContain('UNDECLARED-SECRET')
    expect(output).toContain('PREVIOUS-OUTPUT')
    expect(output).toContain('[appa] A write to a path not in output_files fails as a read-only file system.')
    expect(await readFile(`${directory}/out/new.txt`, 'utf8')).toBe('written\n')
    expect(await readFile(`${directory}/out/old.txt`, 'utf8')).toBe('PREVIOUS-OUTPUTrewritten\n')
    expect(await readFile(`${directory}/in.txt`, 'utf8')).toBe('declared-input')
    expect(await Bun.file(`${directory}/out/extra.txt`).exists()).toBe(false)
  }, 60_000)

  test('a declared directory hides the credentials it holds, and a directory output takes new files', async () => {
    const output = await run({ input_files: ['.'], output_files: ['build/'],
      command: 'ls -a; cat in.txt; cat .env; echo built > build/a.txt; echo "tmp=$TMPDIR" > "$TMPDIR/t"; cat "$TMPDIR/t"' })
    expect(output).toContain('declared-input')
    expect(output).not.toContain('UNDECLARED-SECRET')
    expect(output).toContain('exit: 0')
    // The call that declares the working directory sees all of it, so no listing hint.
    expect(output).not.toContain('saw only its declared input_files')
    expect(await readFile(`${directory}/build/a.txt`, 'utf8')).toBe('built\n')
    // $TMPDIR is the call's own directory, removed when it ends.
    const tmp = output.match(/tmp=(\S+)/)?.[1]
    expect(tmp).toBeDefined()
    expect(await Bun.file(`${tmp}/t`).exists()).toBe(false)
  }, 60_000)

  test('an output the failed command did not write is removed again, with a note', async () => {
    const output = await run({ input_files: [], output_files: ['never.txt'], command: 'ls; exit 1' })
    expect(output).toContain('exit: 1')
    expect(output).toContain('did not exist, so the command found an empty file in its place')
    expect(output).toContain('saw only its declared input_files')
    expect(await Bun.file(`${directory}/never.txt`).exists()).toBe(false)
  }, 60_000)

  test('reaches only declared hosts through the proxy', async () => {
    const receiver = Bun.serve({ hostname: '127.0.0.1', port: 0, fetch: () => new Response('LOCAL-RECEIVER') })
    try {
      const output = await run({ network_hosts: [],
        command: `curl -s --max-time 5 http://127.0.0.1:${receiver.port}/ || echo no-local; curl -s --max-time 5 -o /dev/null -w '%{http_code}' https://example.com || echo no-network` })
      expect(output).not.toContain('LOCAL-RECEIVER')
      expect(output).toContain('no-local')
      expect(output).toContain('no-network')
    } finally { receiver.stop(true) }
  }, 60_000)

  test('a declared host is reachable and an undeclared one is not', async () => {
    const output = await run({ network_hosts: ['example.com'],
      command: `curl -s --max-time 10 -o /dev/null -w 'declared=%{http_code}\\n' https://example.com; curl -s --max-time 10 -o /dev/null -w 'undeclared=%{http_code}\\n' https://github.com` })
    expect(output).toContain('undeclared=000')
    // The orb may lack outbound network; the declared check holds only where it exists.
    if (!output.includes('declared=000')) expect(output).toContain('declared=200')
  }, 60_000)

  test('a typed command works on the project, reaches no host and sees no credential', async () => {
    const before = await readdir(tmpdir())
    const shell = await typedShell(
      'cat in.txt; cat .env || echo no-env; echo typed > typed.txt; curl -s --max-time 5 https://example.com || echo no-network',
      directory, runtime)
    const child = Bun.spawn([shell.shell, '-c', shell.command], { cwd: directory, env: shell.env, stdout: 'pipe', stderr: 'pipe' })
    const output = `${await new Response(child.stdout).text()}${await new Response(child.stderr).text()}`
    expect(await child.exited).toBe(0)
    expect(output).toContain('declared-input')
    expect(output).not.toContain('UNDECLARED-SECRET')
    expect(output).toContain('no-network')
    expect(await readFile(`${directory}/typed.txt`, 'utf8')).toBe('typed\n')
    // The script removes its settings and the command's temporary directory.
    expect((await readdir(tmpdir())).filter(name => !before.includes(name) && /^(appencode-typed|appa-bash)-/.test(name))).toEqual([])
  }, 60_000)
})

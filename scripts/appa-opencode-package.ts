// Run with Bun 1.3.10 from any directory. Dependencies use integrations/opencode/bun.lock.
import { mkdir, readdir, readFile, writeFile } from 'node:fs/promises'
import { dirname, join, resolve } from 'node:path'

const root = resolve(import.meta.dir, '..')
process.chdir(root)
if (Bun.version !== '1.3.10') throw new Error('Reproducible payloads require Bun 1.3.10.')
if (Bun.argv.slice(2).some(arg => arg !== '--check')) throw new Error('Usage: bun scripts/appa-opencode-package.ts [--check]')
const check = Bun.argv.includes('--check')
const destination = join(root, 'marketplace/plugins/opencode')
const files = new Map<string, string>()
const result = await Bun.build({
  entrypoints: ['integrations/opencode/appencode.ts'],
  root,
  target: 'bun',
  format: 'esm',
  external: ['effect', '@opencode/plugin'],
  // The source-only operator launcher must never run when OpenCode imports the plugin.
  define: { 'import.meta.main': 'false' },
  minify: { syntax: true, whitespace: false, identifiers: false },
})
if (!result.success) throw new AggregateError(result.logs, 'AppenCode build failed')
if (result.outputs.length !== 1) throw new Error('Expected one self-contained JavaScript bundle')
// OpenCode's local-directory resolver tries server, then index (not package exports).
files.set('index.js', await result.outputs[0].text())
files.set('package.json', JSON.stringify({
  name: 'appa-appencode', version: '0.1.0', private: true, type: 'module',
  exports: { '.': './index.js' },
  // OpenCode supplies these public modules; no package installation is needed.
  dependencies: { '@opencode/plugin': '2.0.28', effect: '4.0.1' },
}, null, 2) + '\n')
files.set('appa-package.toml', `schema = 1
name = "opencode"
description = "AppenCode gates OpenCode V2 tool calls and emissions through the APPA runtime."

[plugin]
host = "opencode"
protocol = 1
default_policy = "default.appa.toml"
batteries = ["opencode"]
`)
files.set('default.appa.toml', `include = ["batteries/opencode/appa.toml"]

[policy]
version = 2

[policy.deployment]
context_control = true

[externals]
timeout_ms = 2000
max_body_bytes = 65536
`)
for (const path of ['SKILL.md', 'references/core.md', 'references/opencode.md']) {
  files.set(`guide/${path}`, await readFile(join(root, 'integrations/appa-guide', path), 'utf8'))
}
files.set('contracts.md', await readFile(join(root, 'website/content/docs/contracts.md'), 'utf8'))
let stale = false
for (const [path, text] of files) {
  const target = join(destination, path)
  if (check) {
    if (await Bun.file(target).text().catch(() => undefined) !== text) {
      console.error(`Stale or missing payload: ${path}`)
      stale = true
    }
  } else {
    await mkdir(dirname(target), { recursive: true })
    await writeFile(target, text)
  }
}
for (const path of await readdir(destination, { recursive: true }).catch(() => [])) {
  if ((await Bun.file(join(destination, path)).exists()) && !files.has(path)) {
    console.error(`Unexpected payload file: ${path}`)
    stale = true
  }
}
if (stale) process.exit(1)
console.log(`${check ? 'Checked' : 'Generated'} AppenCode: ${files.size} files, ${[...files.values()].reduce((size, text) => size + Buffer.byteLength(text), 0)} bytes`)

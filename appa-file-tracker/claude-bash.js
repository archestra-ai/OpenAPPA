// Confines Claude Code's own Bash tool per call, in Claude Code's built-in sandbox.
//
// `clappa` loads this file into Claude Code through BUN_OPTIONS="--preload <file>". It adds
// `input_files`, `output_files` and `network_hosts` to Bash's arguments, so the hooks and
// the file-tracker see them, and runs each call in Claude Code's sandbox confined to exactly
// those: `appa file-tracker sandbox` answers the call's filesystem settings, and the sandbox
// proxy admits the call's hosts alone. A call that does not run sandboxed is refused by
// Claude Code itself (`sandbox: "required"`). A command that ran is a success whatever its
// exit code, so the engine applies its label: its output reaches the model either way.
//
// With APPA_CLAUDE_BASH_SELFTEST set, this file checks every part of that on this Claude
// Code build, prints `appa-claude-bash: ok` or what failed, and exits before Claude Code
// starts. `clappa` uses native Bash only after the self-test passes.
//
// Everything here reaches into Claude Code's internals by property name, never by chunk
// hash or minified name. A build that changes them fails the self-test, and `clappa` falls
// back to SafeBash.

// The person's own BUN_OPTIONS, which `clappa` keeps after this preload, still reach the
// programs a command runs; this preload does not.
{
  const own = `--preload ${import.meta.path}`;
  const rest = (process.env.BUN_OPTIONS ?? "").replace(own, "").trim();
  if (rest) process.env.BUN_OPTIONS = rest;
  else delete process.env.BUN_OPTIONS;
}

import fs from "fs";
import os from "os";
import path from "path";
import http from "http";
import net from "net";
import crypto from "crypto";
import { execFileSync, spawnSync } from "child_process";
import { AsyncLocalStorage } from "async_hooks";

const ROOT = "/$bunfs/root";
const TRACKER = process.env.APPA_FILE_TRACKER ?? "appa";
const SELFTEST = Boolean(process.env.APPA_CLAUDE_BASH_SELFTEST);
delete process.env.APPA_CLAUDE_BASH_SELFTEST;

const GUIDANCE = `

Every Bash call runs in a sandbox confined to what it declares. Pass absolute paths in input_files for every file, directory or Unix socket the command reads, absolute paths in output_files for every file it writes, and hosts (example.com, *.example.com, host:port) in network_hosts for every host it connects to. Name only the hosts the command needs: a host other than a package registry's can lower the session's trust, go get through the Go proxy needs proxy.golang.org and sum.golang.org, not github.com, and cargo needs index.crates.io and static.crates.io, not crates.io. An output written with a trailing / is a directory: the command reads what it holds and writes under it, as a build directory, node_modules/ or a cache needs (for example ~/.cache/go-build/). A call that names a Go, cargo or npm registry host keeps what it downloads in that package manager's cache (~/go/pkg/mod, ~/.cargo/registry, ~/.npm/_cacache), and every later call reads the cache with no host and without declaring it; if a build still reports a missing download, declare the cache in input_files. A credential file such as .env inside a declared directory stays hidden unless the call names it or git tracks it. Declare a socket such as /var/run/docker.sock or $SSH_AUTH_SOCK in input_files to reach the service behind it. $TMPDIR is a private directory for this call alone, removed when it ends. System directories, the directories on PATH and installed toolchains are readable. Every other file in the workspace, the home directory and /tmp is hidden from the command, so \`ls\`, \`find\` and globs there show only what the call declares, and an empty listing does not mean a file is missing; a write to an undeclared path does not reach the disk. To list a directory, declare it in input_files. An output file that exists is read as well, so a command may rewrite it, as npm install does package.json, but not replace, rename or remove it: write through it, as sed 's/a/b/' f > "$TMPDIR/f" && cat "$TMPDIR/f" > f does in place of sed -i, and declare the file's directory with a trailing / only for a tool that always replaces the file, such as cargo add. The sandbox keeps a repository's config and hooks read-only, in .git, a bare <name>.git or .husky/, even in a declared directory, so git config, git remote add and the upstream that git push -u sets fail: leave those to the person, in their own terminal.`;

/** The exports of every embedded chunk whose source contains all of `needles`. */
async function modulesWith(...needles) {
  const found = [];
  for (const name of fs.readdirSync(ROOT)) {
    if (!name.endsWith(".js")) continue;
    let text;
    try {
      text = fs.readFileSync(`${ROOT}/${name}`, "utf8");
    } catch {
      continue;
    }
    if (needles.every((needle) => text.includes(needle))) found.push(await import(`${ROOT}/${name}`));
  }
  return found;
}

function exported(modules, test, what) {
  for (const module of modules) for (const value of Object.values(module)) if (value && test(value)) return value;
  throw new Error(`this Claude Code build has no ${what}`);
}

const shared = await modulesWith("wrapWithSandbox", "getLinuxHttpSocketPath");
const sandbox = exported(
  shared,
  (v) =>
    typeof v === "object" &&
    ["wrapWithSandbox", "ensureInitializedForWrap", "getConfig", "getProxyPort"].every((k) => typeof v[k] === "function"),
  "sandbox manager",
);
// Bash lives beside the sandbox manager in some builds and in a chunk of its own in others.
const bash = exported(
  [...shared, ...(await modulesWith("wrapWithSandbox", "coerceInput"))],
  (v) =>
    typeof v === "object" &&
    v.name === "Bash" &&
    typeof v.call === "function" &&
    typeof v.prompt === "function" &&
    typeof v.inputSchema?.safeExtend === "function",
  "Bash tool",
);

// --- the three lists, on Bash's schema and through Claude Code's input normalizer ---

const base = bash.inputSchema;
const text = base.shape?.description?._def?.innerType;
if (typeof text?.array !== "function") throw new Error("this Claude Code build's Bash schema has no plain string field");
const list = (description) => text.array().describe(description);
// The lists come ahead of `command` in the schema the model reads: a model writes the
// arguments in that order, and one that has written the command tends to end the call there.
const lists = {
  input_files: list("Absolute paths of every file the command reads. Required, even when empty."),
  output_files: list("Absolute paths of every file the command writes. Required, even when empty."),
  network_hosts: list("Hosts the command connects to: example.com, *.example.com, or host:port. Required, even when empty."),
};
const extended = base.safeExtend(lists);
try {
  // Before anything reads the shape, put the lists first; a build that refuses keeps them last.
  Object.defineProperty(extended._zod.def, "shape", { value: { ...lists, ...base.shape }, configurable: true, enumerable: true });
} catch {}

// Claude Code parses a streamed Bash call against the schema, then rebuilds the input from
// the fields it knows and hands it to `coerceInput`, once or more. The lists cross that gap
// here, keyed by the command they were declared with; the latest few hundred are kept.
const declared = new Map();
const parse = extended.parse.bind(extended);
extended.parse = function (value, ...rest) {
  const parsed = parse(value, ...rest);
  if (parsed && Array.isArray(parsed.input_files)) {
    declared.set(parsed.command, {
      input_files: parsed.input_files,
      output_files: parsed.output_files,
      network_hosts: parsed.network_hosts,
    });
    if (declared.size > 256) declared.delete(declared.keys().next().value);
  }
  return parsed;
};
Object.defineProperty(bash, "inputSchema", { get: () => extended, configurable: true });

// Claude Code rewrites a command before `coerceInput` sees it: it drops a leading
// `cd <working directory> && ` and unescapes `\\;`. The lists are found under the command
// as the model wrote it.
function declaredFor(command) {
  if (typeof command !== "string") return undefined;
  for (const [written, lists] of declared) {
    const unescaped = written.replaceAll("\\\\;", "\\;");
    const stripped = /^cd \S.* && /.test(unescaped) && unescaped.endsWith(` && ${command}`);
    if (written === command || unescaped === command || stripped) return lists;
  }
  return undefined;
}

// A model's call that names none of the lists reads the working directory and nothing else:
// no output and no host, the least a command that looks at the project needs. Rejecting it
// instead leaves a call without lists in the conversation, which the model copies.
const DEFAULTED = new Set();
const defaults = () => ({ input_files: [process.cwd()], output_files: [], network_hosts: [] });

// Only the model's calls pass here; a command the person types reaches `call` directly.
const coerce = bash.coerceInput;
bash.coerceInput = function (input, ...rest) {
  const coerced = coerce ? coerce.call(this, input, ...rest) : undefined;
  const target = coerced?.input ?? { ...input };
  if (Array.isArray(target.input_files)) return coerced;
  const named = [input?.input_files, input?.output_files, input?.network_hosts].some((list) => list !== undefined);
  let lists = declaredFor(target.command);
  if (!lists && named) return coerced;
  if (!lists) {
    lists = defaults();
    DEFAULTED.add(target.command);
  }
  Object.assign(target, lists);
  return { ...(coerced ?? {}), input: target, shapeClass: coerced?.shapeClass ?? "appa_declared_lists" };
};

const prompt = bash.prompt;
bash.prompt = async function (...args) {
  return (await prompt.apply(this, args)) + GUIDANCE;
};

// --- one call: its confinement, required sandboxing, and its exit code as a result ---

// What a confined command's failure means, said beside its result, so the model declares what
// it lacked instead of concluding that the file is gone or the tool cannot work here.
const HINTS = [
  {
    // A listing or a comparison against git shows only what the call declared; a call that
    // declares the working directory sees all of it but its credentials.
    partial: true,
    command: /(^|[\s;&|(`])(ls|find|tree|fd|du|git\s+(status|diff|ls-files|add|clean|stash))\b|\*/,
    text: "[appa] This command saw only its declared input_files: undeclared files were hidden, so a listing or git status here is incomplete. To see a directory, declare it in input_files.",
  },
  {
    // Go and others print it in lower case.
    output: /^(?!.*config file).*(read-only file system|EROFS)/im,
    text: "[appa] A write to a path not in output_files fails as a read-only file system. Declare the path in output_files, or its directory with a trailing /, such as node_modules/.vite/ or .git/.",
  },
  {
    // A repository's config stays read-only in a directory output: Claude Code's sandbox keeps
    // it so below its working directory whatever its settings say, and the file-tracker's
    // write guards everywhere else. A setting there such as core.hooksPath or core.fsmonitor
    // runs code the next time the person runs git, outside any sandbox.
    output: /could not (lock|write) config file/,
    text: "[appa] The sandbox keeps .git/config read-only, even in a declared directory: a setting there, such as core.hooksPath or core.fsmonitor, would run code outside the sandbox. So git config and git remote add fail, and git push -u pushes but sets no upstream, whatever its \"set up to track\" line says. Tell the person the git command to run in their own terminal, such as git branch --set-upstream-to=origin/<branch>; a ! command at the prompt runs in the same sandbox.",
  },
  {
    // Each declared output file is a mount of its own, so a rename over it or an unlink of
    // it fails. Replacing it would need its directory writable, which lets the command
    // create any other file there.
    output: /^(?!.*config file).*(Device or resource busy|EBUSY)/m,
    text: `[appa] A declared output file can be rewritten but not replaced, renamed or removed, so a tool that writes a temporary file and renames it over the output fails. Write through the file instead: sed 's/a/b/' f > "$TMPDIR/f" && cat "$TMPDIR/f" > f in place of sed -i, and git show HEAD:./f > f in place of git checkout -- f. For a tool that always replaces the file, such as cargo add, declare the file's directory with a trailing / in output_files.`,
  },
  {
    // A call that declares a directory below the repository's root leaves the root, and .git
    // in it, out of the command's view.
    output: /not a git repository/,
    text: "[appa] Git found no repository because the repository's root, the directory that holds .git, is not in input_files. Declare the root in input_files, and its .git/ in output_files if git writes.",
  },
  {
    // Git writes a branch, its index and new objects under .git, which a call that names only
    // the work tree, or nothing, leaves read-only; the failure names a lock, not the file system.
    output: /cannot lock ref|unable to create directory for .*\.git\/|Unable to create '[^']*\.lock'|unable to (create temporary|write new index)/,
    text: "[appa] Git writes branches, its index and new objects under .git. When the repository's .git/ is not in output_files, it is read-only to this command, so git commit, git checkout -b and git switch fail. Declare it with a trailing /, such as <repository>/.git/, beside the files the command changes.",
  },
  {
    // What `ls -l` shows of /dev/null mounted where a protected file is absent (`PROTECTED`).
    output: /^c\S{9}\s.*\s1,\s+3\s/m,
    text: "[appa] An entry listed as character device 1, 3, such as .bashrc or .vscode, is /dev/null, which the sandbox mounts where a protected file does not exist so that the command cannot create it. There is no such file.",
  },
];

/**
 * Remove each output the call's command was to create and did not write, so it is absent again
 * as before the call rather than an empty file a tool refuses to read, and say so when the
 * command `failed`: every one still empty when it failed, which a tool such as uv may have
 * opened and refused, and otherwise each left empty and untouched, so a file the command made
 * empty on purpose stays.
 */
function unwritten(confined, failed) {
  const hidden = new Set((confined.hiddenOutputs ?? []).map(({ path: output }) => output));
  const removed = (confined.placeholders ?? []).filter(({ path: placeholder, modified }) => {
    try {
      const stat = fs.lstatSync(placeholder, { bigint: true });
      if (!stat.isFile() || stat.size !== 0n || (!failed && String(stat.mtimeNs) !== modified)) return false;
      fs.unlinkSync(placeholder);
      return true;
    } catch {
      return false;
    }
  });
  // A hidden output's placeholder is removed too; its own note says why it stayed unwritten.
  const shown = removed.filter(({ path: placeholder }) => !hidden.has(placeholder));
  if (!failed || shown.length === 0) return "";
  return `[appa] ${shown.map(({ path: placeholder }) => placeholder).join(", ")} did not exist, so the command found an empty file in its place; it is removed again. A tool that refuses an empty file, such as uv with uv.lock, needs the file's directory declared with a trailing / in output_files instead.`;
}

/**
 * What the model reads about each file output the call's confinement hides: it does not exist
 * but carries a label an earlier session recorded, so the command cannot create it, whatever
 * its exit code says.
 */
function hiddenOutputs(confined) {
  return (confined.hiddenOutputs ?? []).map(({ note }) => note).join("\n");
}

/** `shown` with the hints that `input` and everything it printed, `printed`, call for. */
function hinted(input, shown, printed) {
  const command = input?.command;
  const whole = (input?.input_files ?? []).some((declared) => {
    const relative = typeof declared === "string" ? path.relative(declared, process.cwd()) : "..";
    return !relative.startsWith("..") && !path.isAbsolute(relative);
  });
  // A command the person typed declares nothing: it sees the repository or the working directory.
  const typed = ![input?.input_files, input?.output_files, input?.network_hosts].every(Array.isArray);
  const hints = HINTS.filter((hint) => (hint.partial && (whole || typed) ? false : hint.command ? hint.command.test(command ?? "") : hint.output.test(printed)));
  const ran = JSON.stringify([input?.input_files, input?.output_files, input?.network_hosts]);
  const fallback = defaults();
  if (DEFAULTED.delete(command) && ran === JSON.stringify([fallback.input_files, fallback.output_files, fallback.network_hosts])) {
    hints.unshift({
      text: `[appa] This call named no lists, so it ran as ${JSON.stringify(defaults())}: it read the working directory, wrote nothing and reached no host. Name all three lists in every Bash call.`,
    });
  }
  return [shown, ...hints.map((hint) => hint.text)].filter(Boolean).join("\n");
}

const calls = new AsyncLocalStorage();
const wrapping = new AsyncLocalStorage();
const proxied = new AsyncLocalStorage();
const hostsByNonce = new Map();

// The schema requires the three lists, so a call without them did not come from the model:
// it is a command the person typed at the prompt, which reaches no hook. It runs in the
// working directory, with no host, and sees no file whose label restricts its audience.
function confinement(input) {
  const lists = [input?.input_files, input?.output_files, input?.network_hosts];
  const request = lists.every(Array.isArray) ? input : { command: input?.command, typed: true };
  try {
    return JSON.parse(execFileSync(TRACKER, ["file-tracker", "sandbox"], { input: JSON.stringify(request), encoding: "utf8" }));
  } catch (error) {
    throw new Error(`The command was not run: ${String(error.stderr || error.message).trim()}`);
  }
}

// Claude Code keeps a Bash call in the conversation rebuilt from the fields it knows, so the
// model would see its earlier calls without the lists and copy them that way. The lists go
// back onto the stored call, found by its tool_use id, before the next request carries it.
function remember(input, context, rest) {
  if (![input?.input_files, input?.output_files, input?.network_hosts].every(Array.isArray)) return;
  const id = context?.toolUseId;
  if (typeof id !== "string") return;
  const messages = [...(Array.isArray(context?.messages) ? context.messages : []), ...rest];
  for (const message of messages) {
    const content = message?.message?.content;
    if (!Array.isArray(content)) continue;
    const block = content.find((block) => block?.type === "tool_use" && block.id === id);
    if (!block || typeof block.input !== "object" || block.input === null) continue;
    if (!Array.isArray(block.input.input_files)) {
      // The lists first, as the schema orders them.
      block.input = {
        input_files: input.input_files,
        output_files: input.output_files,
        network_hosts: input.network_hosts,
        ...block.input,
      };
    }
    return;
  }
}

const call = bash.call;
bash.call = async function (input, context, ...rest) {
  remember(input, context, rest);
  const confined = confinement(input);
  const nonce = crypto.randomBytes(16).toString("hex");
  hostsByNonce.set(nonce, confined.network.allowedDomains);
  // Claude Code refuses to run a call that must be sandboxed anywhere but in a confining
  // sandbox: no excluded command, no `dangerouslyDisableSandbox`, no simulated sed edit.
  const required = Object.create(context, {
    remoteCall: {
      value: {
        // Later builds report a confined command's outcome to the remote caller; there is none.
        onNote() {},
        ...(context?.remoteCall ?? {}),
        constraints: { ...(context?.remoteCall?.constraints ?? {}), sandbox: "required" },
      },
    },
  });
  let backgrounded = false;
  try {
    const result = await calls.run({ confined, nonce }, () => call.call(this, input, required, ...rest));
    backgrounded = result?.data?.backgroundTaskId !== undefined;
    if (!backgrounded) unwritten(confined, false);
    if (!backgrounded && typeof result?.data?.stdout === "string") {
      const shown = hinted(input, result.data.stdout, `${result.data.stdout}\n${result.data.stderr ?? ""}`);
      result.data.stdout = [shown, hiddenOutputs(confined)].filter(Boolean).join("\n");
    }
    return result;
  } catch (error) {
    if (error?.name !== "ShellError") throw error;
    const note = unwritten(confined, true);
    return {
      data: {
        stdout: [hinted(input, [error.stderr, `Exit code ${error.code}`].filter(Boolean).join("\n"), String(error.stderr ?? "")), note, hiddenOutputs(confined)]
          .filter(Boolean)
          .join("\n"),
        stderr: "",
        interrupted: Boolean(error.interrupted),
        isImage: false,
      },
    };
  } finally {
    if (!backgrounded) {
      hostsByNonce.delete(nonce);
      fs.rmSync(confined.tmpdir, { recursive: true, force: true });
    }
  }
};

// --- the sandbox: each command gets its own call's filesystem and hosts ---

// Claude Code's sandbox keeps a command from writing these, below its working directory, in
// every call. One that does not exist there is kept from being created by /dev/null mounted
// read-only on an empty entry, and git lists that entry as an untracked file, which
// `git add -A` refuses to add. Git in the command leaves out those the call starts without.
const PROTECTED = [
  ...[".gitconfig", ".gitmodules", ".bashrc", ".bash_profile", ".zshrc", ".zprofile", ".profile"],
  ...[".ripgreprc", ".mcp.json", ".vscode", ".idea", ".claude/commands", ".claude/agents"],
];

/**
 * The prelude line that keeps git in a command from listing what the sandbox mounts /dev/null
 * on: the protected paths absent here, and the files the call does not see, such as an
 * untracked .env, which `git add -A` would refuse to add and so add nothing.
 */
function unlisted({ tmpdir, filesystem }) {
  const cwd = process.cwd();
  // The repository the working directory is in, or the one `git init` would make there. A
  // worktree's .git is a file naming its git directory.
  let top = cwd;
  while (!fs.existsSync(path.join(top, ".git")) && path.dirname(top) !== top) top = path.dirname(top);
  if (!fs.existsSync(path.join(top, ".git"))) top = cwd;
  const absent = PROTECTED.map((name) => path.join(cwd, name)).filter((entry) => {
    let found;
    try {
      found = fs.lstatSync(entry);
    } catch {
      return true;
    }
    // The mount point the sandbox makes for a command still running, such as one in the
    // background, is an empty read-only file on the disk until that command ends.
    return found.isFile() && found.size === 0 && (found.mode & 0o777) === 0o444;
  });
  const hidden = (filesystem?.denyRead ?? []).filter((entry) => {
    const relative = path.relative(top, entry);
    return !relative.startsWith("..") && !path.isAbsolute(relative) && fs.lstatSync(entry, { throwIfNoEntry: false })?.isFile();
  });
  if (absent.length === 0 && hidden.length === 0) return "";
  let gitdir = path.join(top, ".git");
  try {
    const named = /^gitdir: (.+)$/m.exec(fs.readFileSync(gitdir, "utf8"))?.[1];
    if (named) gitdir = path.resolve(top, named.trim());
  } catch {}
  const glob = (text) => text.replace(/[\\*?[ ]/g, "\\$&");
  const excludes = path.join(tmpdir, "appa-git-excludes");
  const config = path.join(tmpdir, "appa-git-config");
  fs.writeFileSync(excludes, [...absent, ...hidden].map((entry) => `/${glob(path.relative(top, entry))}\n`).join(""));
  fs.writeFileSync(config, `[core]\n\texcludesFile = "${excludes.replace(/[\\"]/g, "\\$&")}"\n`);
  // Only in that repository: the same name at the top of another one is a file of its own.
  const quoted = (text) => `'${text.replace(/'/g, `'\\''`)}'`;
  const at = "${GIT_CONFIG_COUNT:-0}";
  return `export GIT_CONFIG_KEY_${at}=${quoted(`includeIf.gitdir:${glob(gitdir)}.path`)} GIT_CONFIG_VALUE_${at}=${quoted(config)} GIT_CONFIG_COUNT=$((${at}+1))\n`;
}

// Before every sandboxed command Claude Code makes .claude/.cc-writes in its working
// directory, where it stages its own writes of the settings, and leaves it there. What it
// made for this session goes when Claude Code ends, if nothing was put in it.
const staging = [".claude/.cc-writes", ".claude"].map((name) => path.join(process.cwd(), name)).filter((dir) => !fs.existsSync(dir));
process.on("exit", () => {
  for (const dir of staging) {
    try {
      fs.rmdirSync(dir);
    } catch {}
  }
});

const wrap = sandbox.wrapWithSandbox;
sandbox.wrapWithSandbox = async function (command, shell, custom, signal, options) {
  const current = calls.getStore();
  if (!current) throw new Error("A sandboxed command outside a declared Bash call was not run.");
  await sandbox.ensureInitializedForWrap();
  hookNetwork();
  const ours = current.confined.filesystem;
  const filesystem = {
    ...(custom?.filesystem ?? {}),
    denyRead: [...(custom?.filesystem?.denyRead ?? []), ...ours.denyRead],
    allowRead: [
      ...ours.allowRead,
      ...[sandbox.getLinuxHttpSocketPath?.(), sandbox.getLinuxSocksSocketPath?.()].filter(Boolean),
    ],
    allowWrite: [...ours.allowWrite],
    denyWrite: [...(custom?.filesystem?.denyWrite ?? []), ...ours.denyWrite],
  };
  // Claude Code's shell writes its working directory to a file of its own after the command,
  // so its suffix is the last match: an earlier one is the command's own text.
  const cwdFile = [...command.matchAll(/pwd -P >\| (\/tmp\/claude-[^\s'"]+)/g)].at(-1)?.[1];
  if (cwdFile && !cwdFile.split("/").includes("..")) {
    fs.mkdirSync(path.dirname(cwdFile), { recursive: true });
    fs.writeFileSync(cwdFile, "");
    filesystem.allowWrite.push(cwdFile);
  }
  const perCall = { ...(custom ?? {}), filesystem, network: { ...(custom?.network ?? {}), ...current.confined.network } };
  // The prelude points TMPDIR at the call's own directory and gives git the person's identity.
  const confinedCommand = (current.confined.prelude ?? "") + unlisted(current.confined) + command;
  const wrapped = await wrapping.run(current, () =>
    wrap.call(this, confinedCommand, shell, perCall, signal, { ...(options ?? {}), commandId: current.nonce }),
  );
  hookNetwork();
  // The sandbox starts without the variables that hold credentials, so neither the command
  // nor its shell's /proc/<pid>/environ carries them.
  const hidden = current.confined.hiddenEnvironment ?? [];
  if (hidden.length === 0) return wrapped;
  const quoted = (text) => `'${String(text).replace(/'/g, `'\\''`)}'`;
  return `exec env ${hidden.map((name) => `-u ${quoted(name)}`).join(" ")} ${quoted(shell || "/bin/bash")} -c ${quoted(wrapped)}`;
};

// The proxy names the command each connection comes from by the `commandId` in its proxy
// credentials: here, the call's own nonce, which no other command can read. The allowlist
// the proxy checks is that call's hosts and nothing else.
const emit = http.Server.prototype.emit;
http.Server.prototype.emit = function (event, request, ...rest) {
  if ((event === "connect" || event === "request") && request?.headers) {
    hookNetwork();
    const credentials = /^basic\s+([a-z0-9+/=]+)\s*$/i.exec(request.headers["proxy-authorization"] ?? "")?.[1];
    if (credentials) {
      const user = Buffer.from(credentials, "base64").toString("utf8").split(":")[0];
      if (user.startsWith("srt.")) {
        const nonce = Buffer.from(user.slice(4), "base64").toString("utf8");
        return proxied.run(nonce, () => emit.call(this, event, request, ...rest));
      }
    }
  }
  return emit.call(this, event, request, ...rest);
};

// A settings reload replaces the sandbox's configuration, so the allowlist is attached again
// on every wrap and every proxy request: whichever configuration the proxy reads holds it.
const HOOKED = Symbol("appa");
function hookNetwork() {
  const network = sandbox.getConfig()?.network;
  if (!network || network[HOOKED]) return;
  Object.defineProperty(network, "allowedDomains", {
    get() {
      const nonce = proxied.getStore();
      return (nonce && hostsByNonce.get(nonce)) ?? [];
    },
    set() {},
    configurable: true,
    enumerable: true,
  });
  // The sockets a command may connect to are its own call's, read only while its command is
  // wrapped. Claude Code counts a sandbox that admits a socket as not confining, since the
  // service behind it is outside: the file-tracker labels the call by the socket it declares
  // instead, and Claude Code's other refusals of unconfined calls still hold.
  for (const key of ["allowUnixSockets", "allowAllUnixSockets"]) {
    Object.defineProperty(network, key, {
      get() {
        return wrapping.getStore()?.confined.network[key];
      },
      set() {},
      configurable: true,
      enumerable: true,
    });
  }
  network.strictAllowlist = true;
  network[HOOKED] = true;
}

// --- the self-test ---

if (SELFTEST) {
  try {
    await selfTest();
    console.log("appa-claude-bash: ok");
    process.exit(0);
  } catch (error) {
    console.log(`appa-claude-bash: ${error.message}`);
    process.exit(1);
  }
}

function check(condition, what) {
  if (!condition) throw new Error(what);
}

// One command, confined as a Bash call declaring `lists` would be, run in `cwd`. `widen`
// changes the confinement first.
async function confinedRun(lists, command, cwd, widen = () => {}) {
  const confined = confinement(lists);
  widen(confined.filesystem);
  const wrapped = await calls.run({ confined, nonce: "selftest" }, () =>
    sandbox.wrapWithSandbox(command, "/bin/bash", undefined, undefined, { commandId: "selftest" }),
  );
  // Claude Code hands the wrapped command its own executable on descriptor 3, for the
  // sandbox's syscall filter.
  const executable = fs.openSync(process.execPath, "r");
  try {
    return spawnSync("/bin/bash", ["-c", wrapped], {
      encoding: "utf8",
      cwd,
      timeout: 20000,
      stdio: ["ignore", "pipe", "pipe", executable],
    });
  } finally {
    fs.closeSync(executable);
    fs.rmSync(confined.tmpdir, { recursive: true, force: true });
  }
}

async function selfTest() {
  extended.parse({ command: "true", input_files: ["/a"], output_files: ["/b"], network_hosts: ["example.com"] });
  const coerced = bash.coerceInput({ command: "true" });
  check(coerced?.input?.input_files?.[0] === "/a", "the declared lists do not survive Claude Code's input normalizer");
  extended.parse({ command: "cd /p && make", input_files: ["/p"], output_files: [], network_hosts: [] });
  check(bash.coerceInput({ command: "make" })?.input?.input_files?.[0] === "/p", "the lists of a call that starts with cd are lost");
  const bare = bash.coerceInput({ command: "ls" })?.input;
  check(bare?.input_files?.[0] === process.cwd() && bare.network_hosts.length === 0, "a call without lists does not read the working directory alone");
  DEFAULTED.delete("ls");
  check(!bash.coerceInput({ command: "ls", input_files: [] })?.input?.output_files, "a call naming some lists is completed");
  const declared = { command: "go mod tidy", input_files: ["/p"], output_files: [], network_hosts: [] };
  check(hinted(declared, "", "open /p/go.mod: read-only file system").includes("output_files"), "a lower-case read-only failure gets no hint");
  check(!hinted({ command: "git status" }, "", "").includes("saw only"), "a typed command gets the note for a partial listing");

  // Outside a session the settings are not loaded yet; the sandbox reads them.
  const loader = (await modulesWith("enableConfigs")).find((module) => typeof module.enableConfigs === "function");
  check(loader, "this Claude Code build has no settings loader");
  await loader.enableConfigs(undefined);
  await sandbox.ensureInitializedForWrap();

  // Filesystem: a declared input is read, an undeclared file is hidden, the declared output
  // is written, and an undeclared write never reaches the disk.
  const dir = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "appa-claude-bash-")));
  try {
    const input = path.join(dir, "in.txt");
    const secret = path.join(dir, "secret.txt");
    const output = path.join(dir, "out.txt");
    const stray = path.join(dir, "stray.txt");
    fs.writeFileSync(input, "DECLARED\n");
    fs.writeFileSync(secret, "UNDECLARED\n");
    process.env.APPA_SELFTEST_TOKEN = "CANARY-ENVIRONMENT";
    const lists = { command: "true", input_files: [input], output_files: [output], network_hosts: [] };
    const command = `cat ${input}; cat ${secret}; tr A-Z a-z < ${input} > ${output}; echo x > ${stray}; printenv APPA_SELFTEST_TOKEN; tr '\\0' '\\n' < /proc/$$/environ`;
    const ran = await confinedRun(lists, command, dir);
    check(ran.stdout.includes("DECLARED"), `a declared input is not readable: ${ran.stdout}${ran.stderr}`);
    check(!ran.stdout.includes("UNDECLARED"), "an undeclared file is readable");
    check(fs.readFileSync(output, "utf8") === "declared\n", `a declared output is not written: ${ran.stderr}`);
    check(!fs.existsSync(stray), "an undeclared write reached the disk");
    check(!ran.stdout.includes("CANARY-ENVIRONMENT"), "a credential variable reaches the command");

    // A declared directory: what it holds is read but a credential in it, which the call
    // does not name, and an output inside it is written.
    const project = path.join(dir, "project");
    fs.mkdirSync(path.join(project, "src"), { recursive: true });
    fs.writeFileSync(path.join(project, "src", "main.txt"), "SOURCE\n");
    fs.writeFileSync(path.join(project, ".env"), "TOKEN=CANARY-DOTENV\n");
    const build = path.join(project, "build");
    const built = await confinedRun(
      { command: "true", input_files: [project], output_files: [`${build}/`], network_hosts: [] },
      `cat ${project}/src/main.txt; cat ${project}/.env; mkdir -p ${build}/sub && echo BUILT > ${build}/sub/out.txt`,
      project,
    );
    check(built.stdout.includes("SOURCE"), `a file in a declared directory is not readable: ${built.stdout}${built.stderr}`);
    check(!built.stdout.includes("CANARY-DOTENV"), "a credential in a declared directory is readable");
    check(
      fs.existsSync(path.join(build, "sub", "out.txt")),
      `an output inside a declared directory is not written: ${built.stdout}${built.stderr}`,
    );

    // A directory output, as a command the person types gets the working directory: what
    // it writes reaches the disk, a credential in it stays unreadable, and a file a rule
    // guards for writing stays as it was.
    fs.writeFileSync(path.join(project, "CLAUDE.md"), "ORIGINAL\n");
    const typed = await confinedRun(
      { command: "true", input_files: [], output_files: [`${project}/`], network_hosts: [] },
      `cat ${project}/.env; echo NEW > ${project}/new.txt; echo CHANGED > ${project}/CLAUDE.md`,
      project,
    );
    check(!typed.stdout.includes("CANARY-DOTENV"), "a credential in a directory output is readable");
    check(fs.existsSync(path.join(project, "new.txt")), `a directory output is not written: ${typed.stdout}${typed.stderr}`);
    check(
      fs.readFileSync(path.join(project, "CLAUDE.md"), "utf8") === "ORIGINAL\n",
      "a file guarded for writing in a directory output is written",
    );

    // In a working directory the call writes, git lists the real untracked files alone, not
    // what the sandbox mounts where a protected file is absent, and the protection holds.
    const repo = path.join(dir, "repo");
    if (spawnSync("git", ["init", "-q", repo]).status === 0) {
      fs.writeFileSync(path.join(repo, "real.txt"), "REAL\n");
      const home = process.cwd();
      process.chdir(repo);
      try {
        // Twice: the second runs while the first one's mount points are still on the disk.
        for (const _ of [1, 2]) {
          const listed = await confinedRun(
            { command: "true", input_files: [repo], output_files: [`${repo}/`], network_hosts: [] },
            "git status --porcelain --untracked-files=all; echo x > .bashrc",
            repo,
          );
          check(listed.stdout === "?? real.txt\n", `git lists files that do not exist: ${listed.stdout}${listed.stderr}`);
        }
      } finally {
        process.chdir(home);
      }
      check(
        !fs.statSync(path.join(repo, ".bashrc"), { throwIfNoEntry: false })?.size,
        "a protected file absent from a directory output is written",
      );

      // A working directory below the repository's root, as in a monorepo: a call that
      // declares the repository finds it from there, and a credential there stays hidden:
      // `git add -A` adds the rest and leaves it out.
      const sub = path.join(repo, "sub");
      fs.mkdirSync(sub);
      fs.writeFileSync(path.join(sub, ".env"), "TOKEN=CANARY-SUBDIRECTORY\n");
      process.chdir(sub);
      try {
        const below = await confinedRun(
          { command: "true", input_files: [repo], output_files: [`${repo}/`], network_hosts: [] },
          "git rev-parse --show-toplevel; cat .env; git add -A && git diff --cached --name-only",
          sub,
        );
        check(below.stdout.startsWith(`${repo}\n`), `git finds no repository from a subdirectory: ${below.stdout}${below.stderr}`);
        check(!below.stdout.includes("CANARY-SUBDIRECTORY"), "a credential in the working directory is readable");
        check(
          below.stdout.includes("\nreal.txt\n") && !below.stdout.includes("sub/.env"),
          `git add -A fails on a hidden credential: ${below.stdout}${below.stderr}`,
        );
      } finally {
        process.chdir(home);
      }
    }

    // A toolchain or a download cache opens whole, read-only or written as a registry call
    // writes its cache, but for what the store recorded there with a label: the file-tracker
    // hides each such file or directory with a denyRead entry inside it.
    const cache = path.join(dir, "cache");
    fs.mkdirSync(path.join(cache, "labeled"), { recursive: true });
    fs.writeFileSync(path.join(cache, "open.txt"), "OPEN\n");
    fs.writeFileSync(path.join(cache, "recorded.txt"), "CANARY-RECORDED\n");
    fs.writeFileSync(path.join(cache, "labeled", "file.txt"), "CANARY-RECORDED\n");
    for (const list of ["allowRead", "allowWrite"]) {
      const opened = await confinedRun(
        { command: "true", input_files: [], output_files: [], network_hosts: [] },
        `cat ${cache}/open.txt ${cache}/recorded.txt ${cache}/labeled/file.txt`,
        dir,
        (filesystem) => {
          filesystem[list].push(cache);
          filesystem.denyRead.push(path.join(cache, "recorded.txt"), path.join(cache, "labeled"));
        },
      );
      check(opened.stdout.includes("OPEN"), `a cache in ${list} is not readable: ${opened.stdout}${opened.stderr}`);
      check(!opened.stdout.includes("CANARY-RECORDED"), `a recorded file in a cache in ${list} is readable`);
    }
  } finally {
    delete process.env.APPA_SELFTEST_TOKEN;
    fs.rmSync(dir, { recursive: true, force: true });
  }

  // Network: each call reaches its own hosts only, and a forged command id reaches nothing.
  hookNetwork();
  const echo = net.createServer((socket) => socket.end("ok"));
  await new Promise((resolve) => echo.listen(0, "127.0.0.1", resolve));
  const port = echo.address().port;
  const a = crypto.randomBytes(16).toString("hex");
  const b = crypto.randomBytes(16).toString("hex");
  hostsByNonce.set(a, ["127.0.0.1"]);
  hostsByNonce.set(b, ["other.invalid"]);
  try {
    const [own, others, crossed, forged] = await Promise.all([
      probe(a, "127.0.0.1", port),
      probe(a, "other.invalid", 443),
      probe(b, "127.0.0.1", port),
      probe("toolu_forged", "127.0.0.1", port),
    ]);
    check(own === 200, `a call cannot reach its own host (${own})`);
    check(others !== 200 && crossed !== 200, "a call reaches another call's host");
    check(forged !== 200, "a forged command id reaches a host");
  } finally {
    hostsByNonce.delete(a);
    hostsByNonce.delete(b);
    echo.close();
  }
}

function probe(nonce, host, port) {
  const user = "srt." + Buffer.from(nonce).toString("base64");
  const auth = Buffer.from(`${user}:${sandbox.getProxyAuthToken?.()}`).toString("base64");
  return new Promise((resolve, reject) => {
    const request = http.request({
      host: "127.0.0.1",
      port: sandbox.getProxyPort(),
      method: "CONNECT",
      path: `${host}:${port}`,
      headers: { "Proxy-Authorization": `Basic ${auth}` },
    });
    request.on("connect", (response, socket) => {
      socket.destroy();
      resolve(response.statusCode);
    });
    request.on("response", (response) => resolve(response.statusCode));
    request.on("error", reject);
    request.end();
  });
}

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

delete process.env.BUN_OPTIONS;

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

Every Bash call runs in a sandbox confined to what it declares. Pass absolute paths in input_files for every file the command reads, absolute paths in output_files for every file it writes, and hosts (example.com, *.example.com, host:port) in network_hosts for every host it connects to. Every other file in the workspace, the home directory and /tmp is hidden from the command, so \`ls\`, \`find\` and globs there show only what the call declares, and an empty listing does not mean a file is missing; a write to an undeclared path does not reach the disk. To find files, use the Glob tool, then declare the ones the command needs. Each output file is emptied before the command runs.`;

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
const bash = exported(
  shared,
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
const extended = base.safeExtend({
  input_files: list("Absolute paths of every file the command reads."),
  output_files: list("Absolute paths of every file the command writes."),
  network_hosts: list("Hosts the command connects to: example.com, *.example.com, or host:port."),
});

// Claude Code parses a streamed Bash call against the schema, then rebuilds the input from
// the fields it knows and hands it to `coerceInput`. The lists cross that gap here, keyed by
// the command they were declared with.
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
  }
  return parsed;
};
Object.defineProperty(bash, "inputSchema", { get: () => extended, configurable: true });

const coerce = bash.coerceInput;
bash.coerceInput = function (input, ...rest) {
  const coerced = coerce ? coerce.call(this, input, ...rest) : undefined;
  const target = coerced?.input ?? { ...input };
  const lists = declared.get(target.command);
  if (!lists || Array.isArray(target.input_files)) return coerced;
  declared.delete(target.command);
  Object.assign(target, lists);
  return { ...(coerced ?? {}), input: target, shapeClass: coerced?.shapeClass ?? "appa_declared_lists" };
};

const prompt = bash.prompt;
bash.prompt = async function (...args) {
  return (await prompt.apply(this, args)) + GUIDANCE;
};

// --- one call: its confinement, required sandboxing, and its exit code as a result ---

// The model reads a listing of a confined directory as the truth; say on every result that it is not.
const HIDDEN =
  "[appa] This command saw only its declared input_files: undeclared files were hidden, so a listing here is incomplete. Use the Glob tool to list files.";

const calls = new AsyncLocalStorage();
const proxied = new AsyncLocalStorage();
const hostsByNonce = new Map();

function confinement(input) {
  const lists = [input?.input_files, input?.output_files, input?.network_hosts];
  if (!lists.every(Array.isArray)) {
    throw new Error("This Bash call declares no input_files, output_files and network_hosts, so it cannot be confined. The command was not run.");
  }
  try {
    return JSON.parse(execFileSync(TRACKER, ["file-tracker", "sandbox"], { input: JSON.stringify(input), encoding: "utf8" }));
  } catch (error) {
    throw new Error(`The command was not run: ${String(error.stderr || error.message).trim()}`);
  }
}

const call = bash.call;
bash.call = async function (input, context, ...rest) {
  const confined = confinement(input);
  const nonce = crypto.randomBytes(16).toString("hex");
  hostsByNonce.set(nonce, confined.network.allowedDomains);
  // Claude Code refuses to run a call that must be sandboxed anywhere but in a confining
  // sandbox: no excluded command, no `dangerouslyDisableSandbox`, no simulated sed edit.
  const required = Object.create(context, {
    remoteCall: {
      value: { ...(context?.remoteCall ?? {}), constraints: { ...(context?.remoteCall?.constraints ?? {}), sandbox: "required" } },
    },
  });
  let backgrounded = false;
  try {
    const result = await calls.run({ confined, nonce }, () => call.call(this, input, required, ...rest));
    backgrounded = result?.data?.backgroundTaskId !== undefined;
    if (!backgrounded && typeof result?.data?.stdout === "string") {
      result.data.stdout = [result.data.stdout, HIDDEN].filter(Boolean).join("\n");
    }
    return result;
  } catch (error) {
    if (error?.name !== "ShellError") throw error;
    return {
      data: {
        stdout: [error.stderr, `Exit code ${error.code}`, HIDDEN].filter(Boolean).join("\n"),
        stderr: "",
        interrupted: Boolean(error.interrupted),
        isImage: false,
      },
    };
  } finally {
    if (!backgrounded) hostsByNonce.delete(nonce);
  }
};

// --- the sandbox: each command gets its own call's filesystem and hosts ---

const wrap = sandbox.wrapWithSandbox;
sandbox.wrapWithSandbox = async function (command, shell, custom, signal, options) {
  const current = calls.getStore();
  if (!current) throw new Error("A sandboxed command outside a declared Bash call was not run.");
  await sandbox.ensureInitializedForWrap();
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
  // The outputs are emptied inside the sandbox, once the command starts.
  const confinedCommand = (current.confined.prelude ?? "") + command;
  const wrapped = await wrap.call(this, confinedCommand, shell, perCall, signal, { ...(options ?? {}), commandId: current.nonce });
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

async function selfTest() {
  extended.parse({ command: "true", input_files: ["/a"], output_files: ["/b"], network_hosts: ["example.com"] });
  const coerced = bash.coerceInput({ command: "true" });
  check(coerced?.input?.input_files?.[0] === "/a", "the declared lists do not survive Claude Code's input normalizer");

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
    const confined = confinement(lists);
    const command = `cat ${input}; cat ${secret}; tr A-Z a-z < ${input} > ${output}; echo x > ${stray}; printenv APPA_SELFTEST_TOKEN; tr '\\0' '\\n' < /proc/$$/environ`;
    const wrapped = await calls.run({ confined, nonce: "selftest" }, () =>
      sandbox.wrapWithSandbox(command, "/bin/bash", undefined, undefined, { commandId: "selftest" }),
    );
    // Claude Code hands the wrapped command its own executable on descriptor 3, for the
    // sandbox's syscall filter.
    const executable = fs.openSync(process.execPath, "r");
    const ran = spawnSync("/bin/bash", ["-c", wrapped], {
      encoding: "utf8",
      cwd: dir,
      timeout: 20000,
      stdio: ["ignore", "pipe", "pipe", executable],
    });
    fs.closeSync(executable);
    check(ran.stdout.includes("DECLARED"), `a declared input is not readable: ${ran.stdout}${ran.stderr}`);
    check(!ran.stdout.includes("UNDECLARED"), "an undeclared file is readable");
    check(fs.readFileSync(output, "utf8") === "declared\n", `a declared output is not written: ${ran.stderr}`);
    check(!fs.existsSync(stray), "an undeclared write reached the disk");
    check(!ran.stdout.includes("CANARY-ENVIRONMENT"), "a credential variable reaches the command");
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

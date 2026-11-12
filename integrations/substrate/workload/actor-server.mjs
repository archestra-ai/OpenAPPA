// The actor's HTTP front door. Substrate's router delivers requests here and
// wakes the actor first when it is suspended.
//
//   GET  /          the console page (ui/index.html)
//   GET  /healthz   the wakeup probe
//   GET  /actor     what this actor is and what it can reach
//   POST /run       {"prompt": "..."} runs one stock Claude Code turn and
//                   streams its events as NDJSON
//   POST /curl      {"tool": "...", "arguments": {...}} makes one MCP tool
//                   call with curl and returns what came back
//
// Nothing here knows about OpenAPPA. Claude Code calls GitHub's MCP server
// directly; the egress gateway checks each call on its way out.

import { execFile, spawn } from "node:child_process";
import { readFileSync } from "node:fs";
import { createServer } from "node:http";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const HERE = dirname(fileURLToPath(import.meta.url));
const PORT = Number(process.env.PORT ?? 8080);
const MCP_CONFIG = process.env.AGENT_MCP_CONFIG ?? "/home/agent/mcp.json";
const MODEL = process.env.AGENT_MODEL ?? "sonnet";
const RUN_TIMEOUT_MS = Number(process.env.AGENT_RUN_TIMEOUT_MS ?? 240_000);
// The GitHub tools the session may call without a prompt. A headless session
// refuses any other tool before it is sent.
const ALLOWED_TOOLS = (
  process.env.AGENT_ALLOWED_TOOLS ?? "mcp__github__get_file_contents,mcp__github__add_issue_comment"
).split(",");

const server = createServer((request, response) => {
  route(request, response).catch((error) => {
    console.error("[actor] request failed:", error);
    if (!response.headersSent) response.writeHead(500, { "content-type": "text/plain" });
    response.end(String(error));
  });
});

server.listen(PORT, "0.0.0.0", () => console.log(`[actor] listening on :${PORT}`));

let running = false;

async function route(request, response) {
  const url = new URL(request.url ?? "/", "http://actor");
  if (request.method === "GET" && url.pathname === "/") {
    response.writeHead(200, { "content-type": "text/html; charset=utf-8" });
    response.end(readFileSync(join(HERE, "ui", "index.html")));
    return;
  }
  if (request.method === "GET" && url.pathname === "/healthz") {
    response.writeHead(200, { "content-type": "text/plain" });
    response.end("ok");
    return;
  }
  if (request.method === "GET" && url.pathname === "/actor") {
    json(response, 200, {
      actor: process.env.ACTOR_NAME ?? null,
      atespace: process.env.ACTOR_ATESPACE ?? null,
      egress: (process.env.AGENT_EGRESS_HOSTS ?? "").split(",").filter(Boolean),
      // What the sandbox holds in place of each credential.
      credentials: {
        CLAUDE_CODE_OAUTH_TOKEN: process.env.CLAUDE_CODE_OAUTH_TOKEN ?? null,
        GITHUB_TOKEN: process.env.GITHUB_TOKEN ?? null,
      },
      mcpServer: process.env.GITHUB_MCP_URL ?? "https://api.githubcopilot.com/mcp/",
    });
    return;
  }
  if (request.method === "POST" && (url.pathname === "/run" || url.pathname === "/curl")) {
    if (running) return json(response, 409, { error: "a run is already in progress" });
    const input = JSON.parse((await readBody(request)) || "{}");
    running = true;
    try {
      if (url.pathname === "/run") {
        if (typeof input.prompt !== "string" || !input.prompt.trim()) {
          return json(response, 400, { error: "prompt is required" });
        }
        await streamRun(input.prompt, response);
      } else {
        json(response, 200, await curlCall(input.tool, input.arguments ?? {}));
      }
    } finally {
      running = false;
    }
    return;
  }
  json(response, 404, { error: "not found" });
}

async function streamRun(prompt, response) {
  response.writeHead(200, { "content-type": "application/x-ndjson", "cache-control": "no-store" });
  const emit = (event) => response.write(`${JSON.stringify(event)}\n`);
  const toolNames = new Map();
  const child = spawn(
    "claude",
    [
      "-p", prompt,
      "--output-format", "stream-json",
      "--verbose",
      "--model", MODEL,
      "--mcp-config", MCP_CONFIG,
      "--strict-mcp-config",
      // No built-in tools: this actor reaches the outside through MCP only.
      "--tools", "",
      "--allowedTools", ALLOWED_TOOLS.join(","),
      "--permission-mode", "default",
      "--no-session-persistence",
    ],
    { cwd: "/home/agent/work", env: process.env, stdio: ["ignore", "pipe", "pipe"] },
  );
  const timer = setTimeout(() => child.kill("SIGTERM"), RUN_TIMEOUT_MS);
  let buffered = "";
  child.stdout.on("data", (chunk) => {
    buffered += chunk;
    let newline;
    while ((newline = buffered.indexOf("\n")) !== -1) {
      const line = buffered.slice(0, newline).trim();
      buffered = buffered.slice(newline + 1);
      if (!line) continue;
      try {
        for (const event of translate(JSON.parse(line), toolNames)) emit(event);
      } catch {}
    }
  });
  child.stderr.on("data", (chunk) => process.stderr.write(chunk));
  const code = await new Promise((resolve) => child.on("close", resolve));
  clearTimeout(timer);
  emit({ type: "exit", code });
  response.end();
}

// Claude Code stream-json messages in, console events out.
function translate(message, toolNames) {
  const events = [];
  if (message.type === "system" && message.subtype === "init") {
    events.push({ type: "session", tools: (message.tools ?? []).filter((name) => name.startsWith("mcp__")) });
  }
  if (message.type === "assistant") {
    for (const block of message.message?.content ?? []) {
      if (block.type === "text" && block.text.trim()) events.push({ type: "text", text: block.text });
      if (block.type === "tool_use") {
        toolNames.set(block.id, block.name);
        events.push({ type: "tool_call", tool: block.name, input: block.input });
      }
    }
  }
  if (message.type === "user") {
    for (const block of message.message?.content ?? []) {
      if (block.type !== "tool_result") continue;
      events.push({
        type: "tool_result",
        tool: toolNames.get(block.tool_use_id) ?? "",
        error: Boolean(block.is_error),
        text: resultText(block.content),
      });
    }
  }
  if (message.type === "result") events.push({ type: "result", text: message.result ?? "" });
  return events;
}

function curlCall(tool, args) {
  return new Promise((resolve) => {
    execFile(join(HERE, "mcp-call.sh"), [String(tool), JSON.stringify(args)], { timeout: 60_000 },
      (error, stdout, stderr) => {
        const data = stdout.split("\n").filter((line) => line.startsWith("data:")).map((line) => line.slice(5).trim());
        let result = null;
        for (const item of data.length ? data : [stdout]) {
          try {
            const parsed = JSON.parse(item);
            if (parsed.id === 2) result = parsed;
          } catch {}
        }
        const content = result?.result?.content?.map((part) => part.text).join("\n") ?? stdout;
        resolve({ ok: !error, isError: Boolean(result?.result?.isError || result?.error), text: content || stderr });
      });
  });
}

function resultText(content) {
  if (typeof content === "string") return content;
  if (Array.isArray(content)) return content.filter((part) => part.type === "text").map((part) => part.text).join("\n");
  return "";
}

function json(response, status, value) {
  response.writeHead(status, { "content-type": "application/json" });
  response.end(JSON.stringify(value));
}

function readBody(request) {
  return new Promise((resolve, reject) => {
    let body = "";
    request.on("data", (chunk) => {
      body += chunk;
      if (body.length > 64 * 1024) reject(new Error("request body too large"));
    });
    request.on("end", () => resolve(body));
    request.on("error", reject);
  });
}

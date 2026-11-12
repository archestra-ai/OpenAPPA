// The actor's HTTP front door. Substrate's router delivers requests here and
// wakes the actor first when it is suspended.
//
//   GET  /          the console page (ui/index.html)
//   GET  /healthz   the wakeup probe: answers once the OpenAPPA runtime does
//   GET  /actor     what this actor is and what it can reach
//   POST /run       {"prompt": "..."} runs one protected Claude Code turn and
//                   streams its events as NDJSON
//
// Each run is `clappa -p`: Claude Code with OpenAPPA's hooks, gated by the
// runtime that `appa plugin install claude-code` deployed in this image. The
// runtime binds loopback inside the sandbox, so a suspend snapshots its
// trajectory store with the agent and a resume restores both.

import { spawn } from "node:child_process";
import { readFileSync } from "node:fs";
import { createServer } from "node:http";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const HERE = dirname(fileURLToPath(import.meta.url));
const PORT = Number(process.env.PORT ?? 8080);
const RUNTIME_URL = process.env.APPA_RUNTIME_URL ?? "http://127.0.0.1:8787";
const MCP_CONFIG = process.env.APPA_DEMO_MCP_CONFIG ?? "/home/agent/mcp.json";
const MODEL = process.env.APPA_DEMO_MODEL ?? "sonnet";
const RUN_TIMEOUT_MS = Number(process.env.APPA_DEMO_RUN_TIMEOUT_MS ?? 240_000);

// Tools the session may call without a prompt. The GitHub write is absent on
// purpose: in a headless session Claude Code refuses an unlisted tool at its
// permission step, so a comment that OpenAPPA admitted by mistake still could
// not post. OpenAPPA's PreToolUse decision comes first, so when the policy
// holds, its refusal is the one the model sees.
const ALLOWED_TOOLS = (
  process.env.APPA_DEMO_ALLOWED_TOOLS ??
  "mcp__archestra__grain__list_meetings,mcp__archestra__grain__fetch_meeting,mcp__appa"
).split(",");

// Tool results whose content stays out of the console. A meeting record
// names its participants, so the page shows only that the read happened.
const WITHHELD_RESULT_PREFIXES = ["mcp__archestra__grain__"];

const server = createServer((request, response) => {
  route(request, response).catch((error) => {
    console.error("[actor] request failed:", error);
    if (!response.headersSent) {
      response.writeHead(500, { "content-type": "text/plain" });
    }
    response.end(String(error));
  });
});

server.listen(PORT, "0.0.0.0", () => {
  console.log(`[actor] listening on :${PORT}`);
});

let running = false;

async function route(request, response) {
  const url = new URL(request.url ?? "/", "http://actor");
  if (request.method === "GET" && url.pathname === "/") {
    const page = readFileSync(join(HERE, "ui", "index.html"));
    response.writeHead(200, { "content-type": "text/html; charset=utf-8" });
    response.end(page);
    return;
  }
  if (request.method === "GET" && url.pathname === "/healthz") {
    const healthy = await runtimeHealthy();
    response.writeHead(healthy ? 200 : 503, { "content-type": "text/plain" });
    response.end(healthy ? "ok" : "appa runtime not answering");
    return;
  }
  if (request.method === "GET" && url.pathname === "/actor") {
    response.writeHead(200, { "content-type": "application/json" });
    response.end(JSON.stringify(await describeActor()));
    return;
  }
  if (request.method === "POST" && url.pathname === "/run") {
    if (running) {
      response.writeHead(409, { "content-type": "text/plain" });
      response.end("a run is already in progress");
      return;
    }
    const { prompt } = JSON.parse((await readBody(request)) || "{}");
    if (typeof prompt !== "string" || prompt.trim() === "") {
      response.writeHead(400, { "content-type": "text/plain" });
      response.end("prompt is required");
      return;
    }
    running = true;
    try {
      await streamRun(prompt, response);
    } finally {
      running = false;
    }
    return;
  }
  response.writeHead(404, { "content-type": "text/plain" });
  response.end("not found");
}

async function streamRun(prompt, response) {
  response.writeHead(200, {
    "content-type": "application/x-ndjson",
    "cache-control": "no-store",
  });
  const emit = (event) => response.write(`${JSON.stringify(event)}\n`);
  const toolNames = new Map();
  let sessionId = null;

  const child = spawn(
    "clappa",
    [
      "-p",
      prompt,
      "--output-format",
      "stream-json",
      "--verbose",
      "--model",
      MODEL,
      "--mcp-config",
      MCP_CONFIG,
      "--strict-mcp-config",
      // No built-in tools: this actor exists to call MCP tools.
      "--tools",
      "",
      "--allowedTools",
      ALLOWED_TOOLS.join(","),
      "--permission-mode",
      "default",
      "--no-session-persistence",
    ],
    { cwd: "/home/agent/work", env: process.env, stdio: ["ignore", "pipe", "pipe"] },
  );
  const timer = setTimeout(() => child.kill("SIGTERM"), RUN_TIMEOUT_MS);

  let buffered = "";
  child.stdout.on("data", (chunk) => {
    buffered += chunk;
    let newline = buffered.indexOf("\n");
    while (newline !== -1) {
      const line = buffered.slice(0, newline).trim();
      buffered = buffered.slice(newline + 1);
      newline = buffered.indexOf("\n");
      if (line === "") continue;
      let message;
      try {
        message = JSON.parse(line);
      } catch {
        continue;
      }
      for (const event of translate(message, toolNames)) {
        if (event.type === "session") sessionId = event.sessionId;
        emit(event);
      }
      if (message.type === "user" && sessionId) {
        // Tool results change the trajectory label; report the new one.
        trajectoryLabel(sessionId).then((label) => label && emit({ type: "label", label }));
      }
    }
  });
  child.stderr.on("data", (chunk) => process.stderr.write(chunk));

  const code = await new Promise((resolve) => child.on("close", resolve));
  clearTimeout(timer);
  if (sessionId) {
    const label = await trajectoryLabel(sessionId);
    if (label) emit({ type: "label", label });
  }
  emit({ type: "exit", code });
  response.end();
}

// Claude Code stream-json messages in, console events out.
function translate(message, toolNames) {
  const events = [];
  if (message.type === "system" && message.subtype === "init") {
    events.push({
      type: "session",
      sessionId: message.session_id,
      tools: (message.tools ?? []).filter((name) => name.startsWith("mcp__")),
    });
  }
  if (message.type === "assistant") {
    for (const block of message.message?.content ?? []) {
      if (block.type === "text" && block.text.trim()) {
        events.push({ type: "text", text: block.text });
      }
      if (block.type === "tool_use") {
        toolNames.set(block.id, block.name);
        events.push({ type: "tool_call", id: block.id, tool: block.name, input: block.input });
      }
    }
  }
  if (message.type === "user") {
    for (const block of message.message?.content ?? []) {
      if (block.type !== "tool_result") continue;
      const tool = toolNames.get(block.tool_use_id) ?? "";
      const text = resultText(block.content);
      const withheld = WITHHELD_RESULT_PREFIXES.some((prefix) => tool.startsWith(prefix));
      events.push({
        type: "tool_result",
        id: block.tool_use_id,
        tool,
        error: Boolean(block.is_error),
        text: withheld && !block.is_error ? null : text,
        bytes: text.length,
      });
    }
  }
  if (message.type === "result") {
    events.push({
      type: "result",
      text: message.result ?? "",
      permissionDenials: (message.permission_denials ?? []).map((denial) => denial.tool_name),
      costUsd: message.total_cost_usd ?? null,
      durationMs: message.duration_ms ?? null,
    });
  }
  return events;
}

function resultText(content) {
  if (typeof content === "string") return content;
  if (Array.isArray(content)) {
    return content
      .filter((part) => part.type === "text")
      .map((part) => part.text)
      .join("\n");
  }
  return "";
}

async function trajectoryLabel(sessionId) {
  try {
    const query = new URLSearchParams({ trajectory: `cc:${sessionId}` });
    const answer = await fetch(`${RUNTIME_URL}/status?${query}`, {
      signal: AbortSignal.timeout(2000),
    });
    return answer.ok ? await answer.json() : null;
  } catch {
    return null;
  }
}

async function runtimeHealthy() {
  try {
    const answer = await fetch(`${RUNTIME_URL}/health`, { signal: AbortSignal.timeout(2000) });
    return answer.ok && (await answer.text()).trim() === "ok";
  } catch {
    return false;
  }
}

async function describeActor() {
  return {
    actor: process.env.ACTOR_NAME ?? null,
    atespace: process.env.ACTOR_ATESPACE ?? null,
    egress: (process.env.APPA_DEMO_EGRESS_HOSTS ?? "").split(",").filter(Boolean),
    // What the sandbox holds in place of each credential. The egress gateway
    // swaps the real value in after the request leaves the sandbox.
    credentials: {
      CLAUDE_CODE_OAUTH_TOKEN: process.env.CLAUDE_CODE_OAUTH_TOKEN ?? null,
      ARCHESTRA_TOKEN: process.env.ARCHESTRA_TOKEN ?? null,
    },
    mcpServer: process.env.ARCHESTRA_MCP_URL ?? null,
    appaRuntime: (await runtimeHealthy()) ? "ok" : "down",
  };
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

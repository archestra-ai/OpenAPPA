"use client";

import { useEffect, useState } from "react";

/* "OpenAPPA docs as MCP server" panel: a card under the table of contents
   that opens a modal of per-agent install commands for the auth-less MCP
   server at <origin>/mcp (app/mcp/route.ts). The origin is taken from the
   page URL so commands stay correct on previews and future domains. */

interface AgentEntry {
  name: string;
  hint?: string;
  command: (origin: string) => string;
}

const AGENTS: AgentEntry[] = [
  {
    name: "Claude Code",
    command: (o) => `claude mcp add --transport http openappa-docs ${o}/mcp`,
  },
  {
    name: "Codex",
    hint: "via the mcp-remote stdio bridge",
    command: (o) => `codex mcp add openappa-docs -- npx -y mcp-remote ${o}/mcp`,
  },
  {
    name: "OpenCode",
    hint: "add to opencode.json",
    command: (o) =>
      `{\n  "mcp": {\n    "openappa-docs": {\n      "type": "remote",\n      "url": "${o}/mcp",\n      "enabled": true\n    }\n  }\n}`,
  },
  {
    name: "Gemini CLI",
    command: (o) => `gemini mcp add --transport http openappa-docs ${o}/mcp`,
  },
  {
    name: "Cursor",
    hint: "add to .cursor/mcp.json",
    command: (o) => `{\n  "mcpServers": {\n    "openappa-docs": {\n      "url": "${o}/mcp"\n    }\n  }\n}`,
  },
  {
    name: "VS Code",
    command: (o) => `code --add-mcp '{"name":"openappa-docs","type":"http","url":"${o}/mcp"}'`,
  },
  {
    name: "Anything else",
    hint: "streamable HTTP, no auth",
    command: (o) => `${o}/mcp`,
  },
];

function CopyButton({ value }: { value: string }) {
  const [copied, setCopied] = useState(false);
  return (
    <button
      type="button"
      className="mcp-copy-btn"
      aria-label="Copy command"
      onClick={() => {
        navigator.clipboard?.writeText(value);
        setCopied(true);
        setTimeout(() => setCopied(false), 1500);
      }}
    >
      {copied ? "copied" : "copy"}
    </button>
  );
}

export function McpPanel() {
  const [open, setOpen] = useState(false);
  const [origin, setOrigin] = useState("https://openappa.dev");

  useEffect(() => {
    if (typeof window !== "undefined") setOrigin(window.location.origin);
  }, []);

  useEffect(() => {
    if (!open) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") setOpen(false);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [open]);

  return (
    <>
      <button type="button" className="mcp-panel-card" onClick={() => setOpen(true)}>
        <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" aria-hidden>
          <rect x="2" y="4" width="20" height="7" rx="1.5" />
          <rect x="2" y="13" width="20" height="7" rx="1.5" />
          <circle cx="6.2" cy="7.5" r="0.8" fill="currentColor" stroke="none" />
          <circle cx="6.2" cy="16.5" r="0.8" fill="currentColor" stroke="none" />
        </svg>
        <span>
          OpenAPPA docs
          <br />
          as MCP server
        </span>
      </button>

      {open && (
        <div
          className="search-overlay"
          onClick={(e) => {
            if (e.target === e.currentTarget) setOpen(false);
          }}
        >
          <div className="search-dialog mcp-dialog" role="dialog" aria-modal="true" aria-label="OpenAPPA docs as MCP server">
            <div className="mcp-dialog-header">
              <div>
                <div className="mcp-dialog-title">OpenAPPA docs as MCP server</div>
                <div className="mcp-dialog-subtitle">
                  This documentation is served as an auth-less MCP server from <code>{origin}/mcp</code> — search, page
                  reads, glossary, and spec-rule lookup, straight from your coding agent.
                </div>
              </div>
              <span className="search-kbd">ESC</span>
            </div>
            <div className="mcp-agent-list">
              {AGENTS.map((agent) => {
                const cmd = agent.command(origin);
                return (
                  <div key={agent.name} className="mcp-agent-row">
                    <div className="mcp-agent-name">
                      {agent.name}
                      {agent.hint && <span className="mcp-agent-hint"> — {agent.hint}</span>}
                    </div>
                    <div className={cmd.includes("\n") ? "mcp-command multiline" : "mcp-command"}>
                      <pre>{cmd}</pre>
                      <CopyButton value={cmd} />
                    </div>
                  </div>
                );
              })}
            </div>
          </div>
        </div>
      )}
    </>
  );
}

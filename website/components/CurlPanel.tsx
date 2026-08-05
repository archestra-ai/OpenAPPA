"use client";

import { useEffect, useState } from "react";

/* "All the docs as curl" panel: a card under the MCP panel that opens a modal
   with curl commands for the plain-text doc endpoints (app/llms.txt and
   app/llms-full.txt). Shares the MCP panel's card and dialog styling. */

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

export function CurlPanel() {
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

  const entries = [
    {
      name: "Everything in one file",
      hint: "llms.txt — index up top, then every page in full",
      command: `curl -s ${origin}/llms.txt`,
    },
  ];

  return (
    <>
      <button type="button" className="mcp-panel-card" onClick={() => setOpen(true)}>
        <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" aria-hidden>
          <polyline points="4 17 10 11 4 5" />
          <line x1="12" y1="19" x2="20" y2="19" />
        </svg>
        <span>
          All the docs
          <br />
          as curl
        </span>
      </button>

      {open && (
        <div
          className="search-overlay"
          onClick={(e) => {
            if (e.target === e.currentTarget) setOpen(false);
          }}
        >
          <div className="search-dialog mcp-dialog" role="dialog" aria-modal="true" aria-label="All the docs as curl">
            <div className="mcp-dialog-header">
              <div>
                <div className="mcp-dialog-title">All the docs as curl</div>
                <div className="mcp-dialog-subtitle">
                  The documentation is also served as plain markdown — one file, no HTML — for piping into anything.
                </div>
              </div>
              <span className="search-kbd">ESC</span>
            </div>
            <div className="mcp-agent-list">
              {entries.map((entry) => (
                <div key={entry.name} className="mcp-agent-row">
                  <div className="mcp-agent-name">
                    {entry.name}
                    <span className="mcp-agent-hint"> — {entry.hint}</span>
                  </div>
                  <div className={entry.command.includes("\n") ? "mcp-command multiline" : "mcp-command"}>
                    <pre>{entry.command}</pre>
                    <CopyButton value={entry.command} />
                  </div>
                </div>
              ))}
            </div>
          </div>
        </div>
      )}
    </>
  );
}

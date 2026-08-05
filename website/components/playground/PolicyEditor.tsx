"use client";

import React, { useEffect, useRef } from "react";

// A TOML editor without an editor library: a syntax-colored <pre> sits behind
// a transparent-text <textarea>, kept aligned by sharing every metric that
// affects layout and mirroring the scroll position. The caret and selection
// belong to the real textarea, so typing behaves exactly like a plain field.

type Token = { text: string; color?: string };

const COMMENT = "var(--text-weak)";
const SECTION = "var(--accent)";
const KEY = "var(--text-strong)";
const STRING = "var(--warn)";
const LITERAL = "var(--accent)";

/** Strings, comments, inline keys, booleans and numbers inside a value. */
function tokenizeValue(value: string, out: Token[]) {
  const pattern = /("(?:[^"\\]|\\.)*"?)|(#.*$)|([A-Za-z_][\w-]*)(?=\s*=)|\b(true|false)\b|(-?\d[\w.:-]*)/g;
  let last = 0;
  for (let match = pattern.exec(value); match; match = pattern.exec(value)) {
    if (match.index > last) out.push({ text: value.slice(last, match.index) });
    const color = match[1] ? STRING : match[2] ? COMMENT : match[3] ? KEY : LITERAL;
    out.push({ text: match[0], color });
    last = match.index + match[0].length;
  }
  if (last < value.length) out.push({ text: value.slice(last) });
}

function tokenizeLine(line: string): Token[] {
  if (line.trimStart().startsWith("#")) return [{ text: line, color: COMMENT }];
  const section = line.match(/^(\s*)(\[+[^\]]*\]+)(.*)$/);
  if (section) {
    const out: Token[] = [{ text: section[1] }, { text: section[2], color: SECTION }];
    tokenizeValue(section[3], out);
    return out;
  }
  const eq = line.indexOf("=");
  if (eq !== -1) {
    const out: Token[] = [{ text: line.slice(0, eq), color: KEY }, { text: "=" }];
    tokenizeValue(line.slice(eq + 1), out);
    return out;
  }
  const out: Token[] = [];
  tokenizeValue(line, out);
  return out;
}

// Every class that affects text layout, identical on both layers.
const METRICS = "m-0 whitespace-pre-wrap break-words p-3 font-mono text-[13px] leading-relaxed";

/** An inclusive line range to light up — the block the engine is acting on. */
export type Highlight = { start: number; end: number };

/**
 * Find the contract or authority block that declares `name`, as line indices:
 * from its `[[...]]` opener down to the line before the next section.
 */
export function findBlock(policy: string, name: string): Highlight | null {
  const lines = policy.split("\n");
  const escaped = name.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  const declares = new RegExp(`^\\s*name\\s*=\\s*"${escaped}"`);
  const at = lines.findIndex((line) => declares.test(line));
  if (at === -1) return null;
  let start = at;
  while (start > 0 && !lines[start].trimStart().startsWith("[[")) start -= 1;
  let end = at;
  while (end + 1 < lines.length && !lines[end + 1].startsWith("[")) end += 1;
  // Trailing blanks and comments belong to the next block, not this one.
  while (end > at && (lines[end].trim() === "" || lines[end].trimStart().startsWith("#"))) end -= 1;
  return { start, end };
}

export function PolicyEditor({
  value,
  onChange,
  autoFocus,
  className,
  highlight,
}: {
  value: string;
  onChange: (next: string) => void;
  autoFocus?: boolean;
  className?: string;
  /** Lines the engine is currently acting on, marker-penned in the layer. */
  highlight?: Highlight | null;
}) {
  const preRef = useRef<HTMLPreElement>(null);
  const textareaRef = useRef<HTMLTextAreaElement>(null);
  const lines = value.split("\n");

  // Bring the lit block into view. Programmatic scrollTop fires the scroll
  // event, so the pre follows through the same sync as user scrolling.
  // Assignment, not scrollTo({behavior:"smooth"}): Chrome silently ignores
  // smooth scrolling on textareas.
  useEffect(() => {
    const textarea = textareaRef.current;
    if (!highlight || !textarea) return;
    const lineHeight = parseFloat(getComputedStyle(textarea).lineHeight) || 20;
    textarea.scrollTop = Math.max(0, highlight.start * lineHeight - lineHeight * 1.5);
  }, [highlight]);

  return (
    <div
      className={`relative overflow-hidden rounded-md border border-[var(--border-weak)] bg-[var(--bg-weak)] focus-within:border-[var(--accent)] ${className ?? ""}`}
    >
      {/* The zero-width space keeps the pre's first child from being a bare
          newline: HTML parsing eats a newline right after <pre>, which would
          make the server snapshot differ from the client render. */}
      <pre
        aria-hidden
        className={`${METRICS} pointer-events-none absolute inset-0 overflow-hidden text-[var(--text)]`}
        ref={preRef}
      >
        {"​"}
        {lines.map((line, index) => {
          const lit = highlight && index >= highlight.start && index <= highlight.end;
          const tokens = tokenizeLine(line).map((token, at) =>
            token.color ? (
              <span key={at} style={{ color: token.color }}>
                {token.text}
              </span>
            ) : (
              token.text
            ),
          );
          return (
            <React.Fragment key={index}>
              {index > 0 && "\n"}
              {lit ? (
                <span style={{ background: "var(--warn-bg)", borderRadius: 2, boxShadow: "0 0 0 3px var(--warn-bg)" }}>
                  {tokens}
                </span>
              ) : (
                tokens
              )}
            </React.Fragment>
          );
        })}
        {"\n"}
      </pre>
      <textarea
        autoFocus={autoFocus}
        className={`${METRICS} relative block h-full w-full resize-none bg-transparent text-transparent caret-[var(--text)] outline-none`}
        onChange={(event) => onChange(event.currentTarget.value)}
        onScroll={(event) => {
          const pre = preRef.current;
          if (pre) {
            pre.scrollTop = event.currentTarget.scrollTop;
            pre.scrollLeft = event.currentTarget.scrollLeft;
          }
        }}
        ref={textareaRef}
        spellCheck={false}
        value={value}
      />
    </div>
  );
}

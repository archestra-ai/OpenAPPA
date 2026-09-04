import fs from "fs";
import path from "path";

import GithubSlugger from "github-slugger";

import { getAllDocs, type DocPage } from "@/lib/docs";

/* Content backing the MCP server (app/mcp/route.ts) and /llms.txt: read from
   the content/docs *.md files on every call — nothing is cached or prebaked —
   with visual directives replaced by text stand-ins and pages sliced
   into sections for targeted reads and full-text search. The docs menu is the
   catalog: only pages that appear in it (title + category frontmatter) are
   served. */

const DIRECTIVE_DESCRIPTIONS: Record<string, string> = {
  "battery-catalog":
    "- [Slack](/battery-slack) — rules for 19 Slack tools, with audiences from Slack users and groups.\n- [Claude Code tools](/battery-claude-code) — rules for Claude Code's Bash and Read tools.\n- [GitHub](/battery-github) — rules for 44 repository, issue, pull request, and user tools.\n- [Grain](/battery-grain) — rules for 49 meeting, transcript, deal, and admin tools.\n- [Google Workspace](/battery-google-workspace) — uses your Workspace directory and groups to build audiences.\n- [Add your own](/write-a-battery) — create and submit policy for an MCP server.",
  "fig-claude-code-hooks":
    "[Animated figure: a protected Claude Code session sends each hook event to OpenAPPA; one tool call comes back allowed, one comes back blocked with safer options.]",
  "fig-connected-agent":
    "[Animated figure: an agent connected to Jira, Salesforce, GitHub, and Granola composes a client update from all four sources.]",
  "fig-exfiltration":
    "[Animated figure: the same agent, on another run, pulls another client's call notes into the update — data exfiltration without any attacker.]",
  "fig-guardrail":
    "[Animated figure: the agent runs inside a policy boundary; labeled data crosses in, and outbound flows are checked against contracts before dispatch.]",
  "fig-kagent":
    "[Animated figure: a gated kagent agent on Kubernetes sends tool calls through the ADK plugin to OpenAPPA, which answers each of the eight hook events; one call is allowed, one confidential read is denied and then authorized by the remedy the agent runs, and one destructive call waits on an operator.]",
  "fig-label-fold":
    "[Animated figure: labels fold as the agent reads — audience intersects, trust takes the minimum.]",
  "fig-negotiation":
    "[Animated figure: a blocked flow comes back with remedy plans; the agent picks one and completes the task.]",
  "fig-remedy-plan":
    "[Animated figure: the engine enumerates remedy plans — approval, sanitization, narrowing — for a blocked call.]",
  "fig-two-endings":
    "[Animated figure: two runs of the same trajectory reach the same verdict — determinism across runs.]",
};

function stripDirectives(markdown: string): string {
  return markdown.replace(/^:::([a-z-]+):::$/gm, (_, name: string) => DIRECTIVE_DESCRIPTIONS[name] ?? "");
}

export interface DocSection {
  /** GitHub-style anchor of the `##`/`###` heading, e.g. "labels-only-move-one-way". */
  anchor: string;
  heading: string;
  /** Markdown of the section body, heading included. */
  markdown: string;
}

export interface McpDoc {
  slug: string;
  title: string;
  description: string;
  category: string;
  url: string;
  markdown: string;
  sections: DocSection[];
}

function docUrl(slug: string): string {
  return slug === "index" ? "/" : `/${slug}`;
}

function splitSections(markdown: string): { intro: string; sections: DocSection[] } {
  const slugger = new GithubSlugger();
  const lines = markdown.split("\n");
  const sections: DocSection[] = [];
  let intro: string[] = [];
  let current: { heading: string; anchor: string; lines: string[] } | null = null;
  let inCode = false;

  for (const line of lines) {
    if (line.trimStart().startsWith("```")) inCode = !inCode;
    const match = !inCode && line.match(/^(##|###)\s+(.*)$/);
    if (match) {
      if (current) sections.push({ anchor: current.anchor, heading: current.heading, markdown: current.lines.join("\n").trim() });
      const heading = match[2].trim();
      current = { heading, anchor: slugger.slug(heading), lines: [line] };
    } else if (current) {
      current.lines.push(line);
    } else {
      intro.push(line);
    }
  }
  if (current) sections.push({ anchor: current.anchor, heading: current.heading, markdown: current.lines.join("\n").trim() });
  return { intro: intro.join("\n").trim(), sections };
}

export function getMcpDocs(): McpDoc[] {
  return getAllDocs()
    .filter((doc: DocPage) => Boolean(doc.title) && Boolean(doc.category))
    .map((doc: DocPage) => {
      const markdown =
        stripDirectives(doc.content).trim() || "*This page is under construction; its content has not been written yet.*";
      const { sections } = splitSections(markdown);
      return {
        slug: doc.slug,
        title: doc.title,
        description: doc.description,
        category: doc.category,
        url: docUrl(doc.slug),
        markdown,
        sections,
      };
    });
}

export function getMcpDoc(slug: string): McpDoc | undefined {
  return getMcpDocs().find((d) => d.slug === slug);
}

/* ——— full-text search ——— */

export interface McpSearchHit {
  slug: string;
  title: string;
  anchor: string | null;
  heading: string | null;
  snippet: string;
}

function snippetAround(text: string, index: number, radius = 220): string {
  const start = Math.max(0, index - radius);
  const end = Math.min(text.length, index + radius);
  return `${start > 0 ? "…" : ""}${text.slice(start, end).replace(/\s+/g, " ").trim()}${end < text.length ? "…" : ""}`;
}

export function searchMcpDocs(query: string, limit = 10): McpSearchHit[] {
  const terms = query
    .toLowerCase()
    .split(/\s+/)
    .filter((t) => t.length > 1);
  if (terms.length === 0) return [];

  const hits: (McpSearchHit & { score: number })[] = [];
  for (const doc of getMcpDocs()) {
    const units: { anchor: string | null; heading: string | null; text: string }[] = [
      { anchor: null, heading: null, text: doc.markdown },
    ];
    for (const s of doc.sections) units.push({ anchor: s.anchor, heading: s.heading, text: s.markdown });

    for (const unit of units) {
      const lower = unit.text.toLowerCase();
      let score = 0;
      let firstIndex = -1;
      for (const term of terms) {
        const idx = lower.indexOf(term);
        if (idx === -1) {
          score = 0;
          break;
        }
        score += 1 + (unit.heading?.toLowerCase().includes(term) ? 2 : 0);
        if (firstIndex === -1 || idx < firstIndex) firstIndex = idx;
      }
      // whole-doc units only count when no section matched better; sections are preferred
      if (score > 0) {
        hits.push({
          slug: doc.slug,
          title: doc.title,
          anchor: unit.anchor,
          heading: unit.heading,
          snippet: snippetAround(unit.text, firstIndex),
          score: score + (unit.anchor ? 1 : 0),
        });
      }
    }
  }

  hits.sort((a, b) => b.score - a.score);
  // Drop a doc-level hit when one of its sections also matched.
  const seenDocWithSection = new Set(hits.filter((h) => h.anchor).map((h) => h.slug));
  return hits
    .filter((h) => h.anchor !== null || !seenDocWithSection.has(h.slug))
    .slice(0, limit)
    .map(({ score: _score, ...hit }) => hit);
}

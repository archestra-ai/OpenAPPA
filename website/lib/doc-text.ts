import {
  BATTERY_REVIEW_CHECKLIST,
  BENCHMARK_HIGHLIGHT,
  CLAUDE_POLICY_TIMING,
  CLAUDE_SESSION_CHOICE,
  DIRECTIVE_LINE,
  INTEGRATION_PATHS,
  SPONSOR_NOTE,
  type DirectiveName,
} from "@/lib/directive-content";
import { generateSections, getAllDocs, getBatteryCards, type DocPage } from "@/lib/docs";

/* The docs as text, backing /llms.txt, the MCP server (app/mcp/route.ts), and
   the site search index (lib/search-index.ts): read from the content/docs
   *.md files on every call, with directives replaced by text renderings and
   pages sliced into sections for targeted reads and full-text search. Nothing
   here keeps a copy of the docs. The docs menu is the catalog: only pages
   that appear in it (title + category frontmatter) are served. */

interface DirectiveContext {
  docs: DocPage[];
  /** URL of the page that hosts the directive, for its in-page anchors. */
  url: string;
  /** Heading anchors on that page. */
  anchors: Set<string>;
}

/* One text rendering per directive, keyed by DirectiveName so a new
   directive does not compile until it has one. Content-bearing directives
   render from the same data the site's components use
   (lib/directive-content.ts) or from the docs themselves; figures get a
   one-line description; interactive or purely visual blocks render as
   nothing. */
const DIRECTIVE_TEXT: Record<DirectiveName, (ctx: DirectiveContext) => string> = {
  "advisory-signup": () => "",
  "battery-catalog": ({ docs }) =>
    [
      ...getBatteryCards(docs).map((card) => `- [${card.name}](${card.url}) — ${card.description}`),
      "- [Add your own](/write-a-battery) — create and submit policy for an MCP server.",
    ].join("\n"),
  "battery-review-checklist": () =>
    [
      `**${BATTERY_REVIEW_CHECKLIST.summary}** ${BATTERY_REVIEW_CHECKLIST.intro}`,
      "",
      ...BATTERY_REVIEW_CHECKLIST.items.map((item) => `- ${item}`),
    ].join("\n"),
  "battery-rule-order": () =>
    "[Figure: root rules run first, followed by each included battery. Rules in every file run from top to bottom.]",
  "benchmark-highlight": () =>
    [
      ...BENCHMARK_HIGHLIGHT.charts.flatMap((chart) => [
        `**${chart.title}**`,
        "",
        ...chart.rows.map((row) => `- ${row.name}: ${row.pct}%`),
        "",
      ]),
      `[${BENCHMARK_HIGHLIGHT.link.label}](${BENCHMARK_HIGHLIGHT.link.href})`,
    ].join("\n"),
  "brand-assets": () => "",
  "claude-policy-timing": () =>
    `${CLAUDE_POLICY_TIMING.duration} ${CLAUDE_POLICY_TIMING.durationNote}. ${CLAUDE_POLICY_TIMING.lead} **${CLAUDE_POLICY_TIMING.emphasis}** ${CLAUDE_POLICY_TIMING.rest}`,
  "claude-session-choice": () =>
    CLAUDE_SESSION_CHOICE.map((option) => `- \`${option.command}\` — ${option.title}. ${option.note}`).join("\n"),
  "fig-claude-code-hooks": () =>
    "[Animated figure: a protected Claude Code session sends each hook event to OpenAPPA; one tool call comes back allowed, one comes back blocked with safer options.]",
  "fig-connected-agent": () =>
    "[Animated figure: an agent connected to Jira, Salesforce, GitHub, and Granola composes a client update from all four sources.]",
  "fig-exfiltration": () =>
    "[Animated figure: the same agent, on another run, pulls another client's call notes into the update — data exfiltration without any attacker.]",
  "fig-guardrail": () =>
    "[Animated figure: the agent runs inside a policy boundary; labeled data crosses in, and outbound flows are checked against contracts before dispatch.]",
  "fig-kagent": () =>
    "[Animated figure: a gated kagent agent on Kubernetes sends tool calls through the ADK plugin to OpenAPPA, which answers each of the eight hook events; one call is allowed, one confidential read is denied and then authorized by the remedy the agent runs, and one destructive call waits on an operator.]",
  "fig-label-fold": () =>
    "[Animated figure: labels fold as the agent reads — audience intersects, trust takes the minimum.]",
  "fig-negotiation": () =>
    "[Animated figure: a blocked flow comes back with remedy plans; the agent picks one and completes the task.]",
  "fig-policy-stack": () =>
    "[Figure: a stack of OpenAPPA policy TOML files applied to coding agents, LLM proxies, MCP gateways, MCP servers, and agents in production.]",
  "fig-remedy-plan": () =>
    "[Animated figure: the engine enumerates remedy plans — approval, sanitization, narrowing — for a blocked call.]",
  "fig-runtime-overview": () =>
    "[Figure: the agent harness intercepts lifecycle events via middleware, callbacks, or plugins and sends them to the OpenAPPA runtime at POST /hook. Inside the runtime, an adapter decodes the event and the policy engine evaluates security rules to return a decision. Remediation runs through the runtime MCP service.]",
  "fig-two-endings": () =>
    "[Animated figure: two runs of the same trajectory reach the same verdict — determinism across runs.]",
  "integration-paths": ({ url, anchors }) =>
    INTEGRATION_PATHS.map((p) => {
      if (!anchors.has(p.href.slice(1))) throw new Error(`integration-paths: ${url} has no heading ${p.href}`);
      return `- [${p.method}](${url}${p.href}) — ${p.detail}`;
    }).join("\n"),
  "mascot-board": () => "",
  "sponsor-note": () => `${SPONSOR_NOTE.lead} [${SPONSOR_NOTE.link.label}](${SPONSOR_NOTE.link.href})${SPONSOR_NOTE.rest}`,
  "video-how-it-works": () => "[Video: How OpenAPPA works — https://www.youtube.com/watch?v=XKdN90IYy0Y]",
};

/* lib/docs.ts has already refused any page naming an unknown directive. */
function renderDirectives(markdown: string, ctx: DirectiveContext): string {
  return markdown.replace(DIRECTIVE_LINE, (_, indent: string, name: DirectiveName) =>
    DIRECTIVE_TEXT[name](ctx)
      .split("\n")
      .map((line) => (line ? indent + line : line))
      .join("\n"),
  );
}

export interface TextSection {
  /** The heading's anchor on the rendered page, e.g. "labels-only-move-one-way". */
  anchor: string;
  heading: string;
  /** Section text below the heading, directives rendered. */
  body: string;
  /** The same section as markdown, heading included. */
  markdown: string;
}

export interface TextDoc {
  slug: string;
  title: string;
  description: string;
  category: string;
  url: string;
  markdown: string;
  /** Text above the first heading, directives rendered. */
  intro: string;
  sections: TextSection[];
}

export function docUrl(slug: string): string {
  return slug === "index" ? "/" : `/${slug}`;
}

export function getTextDocs(): TextDoc[] {
  const docs = getAllDocs().filter((doc) => Boolean(doc.title) && Boolean(doc.category));
  return docs.map((doc) => {
    const url = docUrl(doc.slug);
    const markdown =
      renderDirectives(doc.content, { docs, url, anchors: new Set(generateSections(doc.content).map((s) => s.id)) }).trim() ||
      "*This page is under construction; its content has not been written yet.*";
    /* The site's own section walker, so every anchor is a real anchor on the
       rendered page. Directives never produce headings, so rendering them
       first leaves the sections unchanged. */
    const sections = generateSections(markdown).map((section) => ({
      anchor: section.id,
      heading: section.text,
      body: section.body.trim(),
      markdown: `${"#".repeat(section.level)} ${section.text}\n\n${section.body.trim()}`,
    }));
    const firstHeading = markdown.search(/^#{2,3}\s/m);
    const intro = (firstHeading === -1 ? markdown : markdown.slice(0, firstHeading)).trim();
    return { slug: doc.slug, title: doc.title, description: doc.description, category: doc.category, url, markdown, intro, sections };
  });
}

export function getTextDoc(slug: string): TextDoc | undefined {
  return getTextDocs().find((d) => d.slug === slug);
}

/* ——— full-text search ——— */

export interface TextSearchHit {
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

export function searchTextDocs(query: string, limit = 10): TextSearchHit[] {
  const terms = query
    .toLowerCase()
    .split(/\s+/)
    .filter((t) => t.length > 1);
  if (terms.length === 0) return [];

  const hits: (TextSearchHit & { score: number })[] = [];
  for (const doc of getTextDocs()) {
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

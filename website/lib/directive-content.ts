/* Every :::name::: block directive a page may use. The site's renderers
   (components/DocContent.tsx) and the text renderers behind /llms.txt, the
   MCP server, and search (lib/doc-text.ts) are both keyed by this type, so a
   directive cannot exist in one and be missing from the other; lib/docs.ts
   refuses a page that names a directive not listed here. */
export const DIRECTIVE_NAMES = [
  "advisory-signup",
  "battery-catalog",
  "battery-review-checklist",
  "battery-rule-order",
  "benchmark-highlight",
  "brand-assets",
  "claude-policy-timing",
  "claude-session-choice",
  "fig-claude-code-hooks",
  "fig-connected-agent",
  "fig-exfiltration",
  "fig-guardrail",
  "fig-kagent",
  "fig-label-fold",
  "fig-negotiation",
  "fig-policy-stack",
  "fig-remedy-plan",
  "fig-runtime-overview",
  "fig-two-endings",
  "integration-paths",
  "mascot-board",
  "sponsor-note",
  "video-how-it-works",
] as const;

export type DirectiveName = (typeof DIRECTIVE_NAMES)[number];

export function isDirectiveName(name: string): name is DirectiveName {
  return (DIRECTIVE_NAMES as readonly string[]).includes(name);
}

/** A directive on a line of its own; list items may indent it. */
export const DIRECTIVE_LINE = /^([ \t]*):::([a-z0-9-]+):::[ \t]*$/gm;

/* Copy behind the directives that carry content rather than decoration. The
   site renders it through its components; the text renderers render the same
   data, so the two cannot drift. Inline code is written as `backticks` and
   each renderer turns it into its own form. */

export const BATTERY_REVIEW_CHECKLIST = {
  summary: "What should I review?",
  intro: "Open the links next to each rule and check:",
  items: [
    "The battery includes every tool in the server version it names.",
    "It identifies every tool that sends data or changes something.",
    "Each action sends data only to the people or services you expect.",
    "Only the right people can see each result.",
    "Data from sources you have not checked is `suspicious`, not `trusted`.",
    "The battery asks a person before every action that needs approval.",
    "The agent lists every question it could not answer from the server code or docs.",
  ],
} as const;

export const BENCHMARK_HIGHLIGHT = {
  charts: [
    {
      title: "Task completion",
      rows: [
        { name: "OpenAPPA", pct: 89, subject: true },
        { name: "Claude Auto mode", pct: 90 },
        { name: "FIDES (Microsoft)", pct: 41 },
      ],
    },
    {
      title: "Attacks that succeeded",
      rows: [
        { name: "OpenAPPA", pct: 0, subject: true },
        { name: "Claude Auto mode", pct: 10 },
        { name: "FIDES (Microsoft)", pct: 31 },
      ],
    },
  ],
  link: { href: "/evaluation", label: "Read the full benchmark results" },
} as const;

export const CLAUDE_POLICY_TIMING = {
  duration: "~10 min",
  durationNote: "for the demo policy",
  lead: "Claude typically needs about ten minutes to inspect your tools, ask any necessary questions, and generate the initial policy.",
  emphasis: "In a corporate deployment, this is a one-time governance step.",
  rest: "The policy is generated, reviewed, and approved once, then shared across the protected agent surfaces.",
} as const;

export const CLAUDE_SESSION_CHOICE = [
  { command: "$ claude", title: "Standard Claude Code", note: "The plugin leaves this path unchanged.", protected: false },
  { command: "$ clappa", title: "Claude Code + OpenAPPA", note: "Tool flows are checked against your policy.", protected: true },
] as const;

/** Hrefs are anchors on the page that hosts the directive. */
export const INTEGRATION_PATHS = [
  { href: "#embed-the-appa-runtime-in-your-agents-code", method: "Your own agent", detail: "Any language. You own the agent loop and tool execution." },
  { href: "#connect-a-coding-agent-through-hooks", method: "Agents through hooks", detail: "Claude Code, or harnesses such as omp and Hermes with a custom adapter." },
  { href: "#use-appa-at-the-llm-proxy", method: "LLM proxy", detail: "Apply policies centrally through Archestra." },
] as const;

export const SPONSOR_NOTE = {
  lead: "The development of OpenAPPA is sponsored by Archestra. We intentionally made OpenAPPA vendor-agnostic: the engine, the batteries, and this website belong to no single product. If you are looking to ship OpenAPPA as part of your product, don't hesitate to",
  link: {
    href: "https://github.com/archestra-ai/openappa/tree/main/website/content/docs",
    label: "contribute a page to this website's repository",
  },
  rest: ", and your product will be listed here 👋. Let's promote deterministic guardrails together!",
} as const;

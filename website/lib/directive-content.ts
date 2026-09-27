/* Copy behind the :::name::: directives that carry content rather than
   decoration. The site renders it through the components in
   components/DocContent.tsx; /llms.txt and the MCP server render the same
   data as text through lib/mcp-content.ts, so the two cannot drift. Inline
   code is written as `backticks` and each renderer turns it into its own
   form. */

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

// Content for the /landing page, verbatim from the design project's
// Landing.dc.html script block.

export type Syntax = { id: string; label: string };

export const SYNTAXES: Syntax[] = [
  { id: "toml", label: "TOML" },
  { id: "mcp", label: "JSON / MCP" },
  { id: "yaml", label: "YAML / Kyverno" },
  { id: "rego", label: "OPA / Rego" },
  { id: "cedar", label: "Cedar" },
  { id: "langchain", label: "LangChain" },
];

// Lines beginning with '~' are the contract rows and render highlighted.
export function lines(src: string): { text: string; hl: boolean }[] {
  return src
    .replace(/^\n|\n$/g, "")
    .split("\n")
    .map((l) => {
      const hl = l.startsWith("~");
      return { text: hl ? l.slice(1) : l, hl };
    });
}

export type Source = {
  id: string;
  name: string;
  kicker: string;
  blurb: string;
  code: Record<string, string>;
};

export const SOURCES: Source[] = [
  {
    id: "arg",
    name: "Contract from tool arguments",
    kicker: "01",
    blurb: "Derived from the tool-call argument itself.",
    code: {
      toml: `
# Contract derived from the tool-call argument
[[tool]]
name     = "gmail__send_email"          # gmail__send_email(body, to)
~requires = { trust = "trusted", audience = { contains = ["$to"] } }
~effects  = ["egress"]
~delta    = {}   # a delivery receipt carries nothing`,
      mcp: `
// tools/list — the contract rides on the tool itself
{
  "name": "gmail__send_email",
  "_meta": {
~    "appa/contract": {
~      "requires": { "trust": "trusted", "audience": { "contains": ["$to"] } },
~      "effects": ["egress"],
~      "delta": {}
~    }
  }
}`,
      yaml: `
apiVersion: appa.dev/v1
kind: ToolContract
metadata:
  name: gmail-send-email
spec:
  tool: gmail__send_email
~  requires:
~    trust: trusted
~    audience: { contains: ["$to"] }
~  effects: [egress]
~  delta: {}`,
      rego: `
package appa.gmail

# contract for gmail__send_email(body, to)
~contract[c] {
~  input.tool == "gmail__send_email"
~  c := {
~    "requires": {"trust": "trusted",
~                 "audience": {"contains": [input.arguments.to]}},
~    "effects": ["egress"],
~    "delta": {}
~  }
~}`,
      cedar: `
// contract for gmail__send_email(body, to)
~permit (
~  principal,
~  action == Action::"gmail__send_email",
~  resource
~)
~when { context.trust == "trusted" &&
~       resource.audience.contains(context.arguments.to) };
// effects: ["egress"]  delta: {}`,
      langchain: `
# contract for gmail__send_email(body, to)
@tool
def gmail__send_email(body: str, to: str):
~    """Send an email."""
~    appa.contract(
~        requires={"trust": "trusted", "audience": {"contains": ["$to"]}},
~        effects=["egress"],
~        delta={},   # a delivery receipt carries nothing
~    )`,
    },
  },
  {
    id: "enum",
    name: "Contract enumeration",
    kicker: "02",
    blurb: "Defined in the policy, inline per argument. First matching row wins.",
    code: {
      toml: `
# Contract defined in the policy, inline per argument
# (proposed matcher rows — not in the dialect yet; first matching row wins)
[[tool]]
name     = "github__create_issue"
when     = { repo = "archestra/*-private" }   # private repos: no audience gate
~requires = {}
~effects  = ["egress", "mutation"]
~delta    = {}

[[tool]]
name     = "github__create_issue"          # everywhere else: public only
~requires = { trust = "trusted", audience = { contains = ["public"] } }
~effects  = ["egress", "mutation"]
~delta    = {}`,
      mcp: `
// first matching row wins
{
  "name": "github__create_issue",
  "_meta": {
    "appa/contracts": [
~      { "when": { "repo": "archestra/*-private" },
~        "requires": {}, "effects": ["egress", "mutation"], "delta": {} },
~      { "requires": { "trust": "trusted",
~                      "audience": { "contains": ["public"] } },
~        "effects": ["egress", "mutation"], "delta": {} }
    ]
  }
}`,
      yaml: `
apiVersion: appa.dev/v1
kind: ToolContract
metadata:
  name: github-create-issue
spec:
  tool: github__create_issue
  rules:                        # first matching rule wins
~    - when: { repo: "archestra/*-private" }
~      requires: {}
~      effects: [egress, mutation]
~    - requires:
~        trust: trusted
~        audience: { contains: ["public"] }
~      effects: [egress, mutation]`,
      rego: `
package appa.github

# private repos: no audience gate
~contract[c] {
~  glob.match("archestra/*-private", [], input.arguments.repo)
~  c := {"requires": {}, "effects": ["egress", "mutation"], "delta": {}}
~}

# everywhere else: public only
~contract[c] {
~  not glob.match("archestra/*-private", [], input.arguments.repo)
~  c := {"requires": {"trust": "trusted",
~                     "audience": {"contains": ["public"]}},
~        "effects": ["egress", "mutation"], "delta": {}}
~}`,
      cedar: `
// private repos: no audience gate
~permit (principal, action == Action::"github__create_issue", resource)
~when { resource.repo like "archestra/*-private" };

// everywhere else: public only
~permit (principal, action == Action::"github__create_issue", resource)
~when { context.trust == "trusted" &&
~       resource.audience.contains("public") };
// effects: ["egress", "mutation"]`,
      langchain: `
@tool
def github__create_issue(repo: str, title: str):
    """Open an issue."""
~    appa.contracts(
~        # private repos: no audience gate
~        when(repo="archestra/*-private",
~             requires={}, effects=["egress", "mutation"]),
~        # everywhere else: public only
~        default(requires={"trust": "trusted",
~                          "audience": {"contains": ["public"]}},
~                effects=["egress", "mutation"]),
~    )`,
    },
  },
  {
    id: "dynamic",
    name: "Dynamically resolved",
    kicker: "03",
    blurb: "Arrives per call from the MCP server, out of its own ACLs.",
    code: {
      toml: `
# Contract dynamically resolved from the MCP server
[[tool]]
name = "confluence__get_issue"
# arrives per call from the resolver, out of Confluence's own ACLs —
# may land in MCP as SEP-1862 (tools/resolve), until then implement
# the resolve endpoint yourself.
#
#   → tools/resolve
#     {
#       "name": "confluence__get_issue",
#       "arguments": { "issue_key": "LEGAL-123" }
#     }
#   ← {
#       "_meta": {
~#         "appa/contract": {
~#           "delta": { "audience": ["legal"] }
~#         }
#       }
#     }`,
      mcp: `
// → tools/resolve  (proposed as SEP-1862)
{
  "name": "confluence__get_issue",
  "arguments": { "issue_key": "LEGAL-123" }
}

// ← resolved out of Confluence's own ACLs
{
  "_meta": {
~    "appa/contract": {
~      "delta": { "audience": ["legal"] }
~    }
  }
}`,
      yaml: `
apiVersion: appa.dev/v1
kind: ToolContract
metadata:
  name: confluence-get-issue
spec:
  tool: confluence__get_issue
~  resolver:                     # contract fetched per call
~    endpoint: https://confluence.internal/appa/resolve
~    method: tools/resolve
~  # ← delta: { audience: [legal] }`,
      rego: `
package appa.confluence

# contract fetched per call from the resolver
~contract[c] {
~  resp := http.send({
~    "method": "POST",
~    "url": "https://confluence.internal/appa/resolve",
~    "body": {"name": input.tool, "arguments": input.arguments},
~  })
~  c := resp.body._meta["appa/contract"]
~}`,
      cedar: `
// Cedar has no fetch — the resolver supplies the entity
// before evaluation, out of Confluence's own ACLs.
~permit (principal, action == Action::"confluence__get_issue", resource)
~when {
~  // resource.audience arrives from tools/resolve
~  resource.audience == ["legal"]
~};`,
      langchain: `
@tool
def confluence__get_issue(issue_key: str):
    """Read a Confluence issue."""
~    appa.resolve(
~        # out of Confluence's own ACLs, per call
~        endpoint="https://confluence.internal/appa/resolve",
~        method="tools/resolve",
~    )   # ← delta: {"audience": ["legal"]}`,
    },
  },
];

export type Installer = { id: string; label: string; cmd: string };

export const INSTALLERS: Installer[] = [
  { id: "brew", label: "Homebrew", cmd: "brew install openappa" },
  { id: "cargo", label: "Cargo", cmd: "cargo install openappa" },
  { id: "npm", label: "npm", cmd: "npm i -g openappa" },
  { id: "nix", label: "Nix", cmd: "nix profile install nixpkgs#openappa" },
  { id: "docker", label: "Docker", cmd: "docker run --rm -it ghcr.io/openappa/appa:latest" },
  { id: "scoop", label: "Scoop", cmd: "scoop install openappa" },
  { id: "aur", label: "AUR", cmd: "yay -S openappa" },
  { id: "mise", label: "Mise", cmd: "mise use -g openappa@latest" },
  { id: "asdf", label: "asdf", cmd: "asdf plugin add openappa && asdf install openappa latest" },
  { id: "deb", label: "Debian", cmd: "sudo apt install openappa" },
  { id: "rpm", label: "Fedora / RHEL", cmd: "sudo dnf install openappa" },
  { id: "choco", label: "Chocolatey", cmd: "choco install openappa" },
];

export type DemoState = {
  v: "allowed" | "blocked" | "idle";
  flow: string;
  trust: string;
  audience: string;
};

export type ScenarioLineKind = "cmd" | "say" | "tool" | "dim" | "warn";

export type ScenarioLine = [string, ScenarioLineKind, DemoState | null];

export type Scenario = {
  id: string;
  label: string;
  lines: ScenarioLine[];
  note: string;
};

export const SCENARIOS: Scenario[] = [
  {
    id: "github",
    label: "Open an issue",
    lines: [
      ["> file the complaint from LEGAL-123 as a GitHub issue", "cmd", null],
      ["I need the ticket first — **reading it from Confluence**.", "say", null],
      [
        '● confluence__get_issue(issue_key: "LEGAL-123")',
        "tool",
        {
          v: "allowed",
          flow: "confluence__get_issue → context",
          trust: "trust: 0.41",
          audience: "audience: legal",
        },
      ],
      ['  ⎿  resolver returned delta.audience = ["legal"]', "dim", null],
      ["Now filing it on the **main tracker**, archestra/openappa.", "say", null],
      [
        '● github__create_issue(repo: "archestra/openappa", title: …)',
        "tool",
        {
          v: "blocked",
          flow: "github__create_issue → repo.public",
          trust: "trust: 0.41",
          audience: "audience: legal",
        },
      ],
      ['  ⎿  APPA: this value is legal-only; a public repo requires audience ["public"].', "warn", null],
      ["     Repos matching archestra/*-private accept it as is.", "warn", null],
      [
        "APPA **stopped sensitive information from leaking**: the ticket is **legal-only**, so a public repo is out. It also **named the sinks** that do accept it — **retrying on the private tracker**.",
        "say",
        null,
      ],
      [
        '● github__create_issue(repo: "archestra/legal-private", title: …)',
        "tool",
        {
          v: "allowed",
          flow: "github__create_issue → repo.private",
          trust: "trust: 0.41",
          audience: "audience: legal",
        },
      ],
      ["  ⎿  created issue #482", "dim", null],
      ["Filed as **archestra/legal-private#482** — **nothing legal-only left the audience**.", "say", null],
    ],
    note: "APPA does not just refuse. The ruling names the requirement that failed and the sinks that would accept the value, so the agent reroutes on its own instead of retrying blind.",
  },
];

export type Layer = {
  id: string;
  kicker: string;
  name: string;
  blurb: string;
  surfaces: string;
  explain: string;
  code: string;
};

export const LAYERS: Layer[] = [
  {
    id: "sdk",
    kicker: "01",
    name: "Natively, in the agent SDK",
    blurb: "The session wrapper labels every tool result and checks every call inside your own loop.",
    surfaces: "LangChain · LangGraph · Vercel AI SDK · Pydantic AI · custom loops",
    explain:
      "The mediator sits between your agent and its tools. It folds a label onto every result the model reads and evaluates the contract before dispatch, so no proxy or hook is in the path — a ruling is just a function call in your process.",
    code: `
from appa import Session

session = Session(policy="appa.toml")
~agent = session.wrap(my_agent)      # tools now flow through the mediator
~
~result = agent.invoke("email the board notes to dana@acme.com")
~# ← every tool call carried a contract; the trajectory is unchanged`,
  },
  {
    id: "hook",
    kicker: "02",
    name: "As a pre-tool-call hook",
    blurb: "One command registers APPA on the coding agent you already run.",
    surfaces: "Claude Code · Codex · Cursor · Gemini CLI · Pi CLI · VS Code · Windsurf · Hermes · OpenClaw",
    explain:
      "Coding agents expose a hook that fires before a tool runs. APPA answers it with a ruling: allow, or refuse with the requirement that failed and the sinks that would accept the value. Nothing about the agent changes.",
    code: `
$ appa setup claude-code
~✓ wrote .claude/settings.json  →  PreToolUse: appa hook
~
~# .claude/settings.json
~{ "hooks": { "PreToolUse": [{ "command": "appa hook" }] } }`,
  },
  {
    id: "gateway",
    kicker: "03",
    name: "At the LLM / MCP gateway",
    blurb: "Enforced on the wire, for agents you do not control.",
    surfaces: "Any agent · any MCP server · any model provider",
    explain:
      "Point the agent at the gateway instead of the provider. It reads contracts off the MCP servers it fronts, tracks labels across the whole trajectory and rules on each call in flight — the only option that covers agents you did not build.",
    code: `
$ appa gateway --policy appa.toml --listen :8080
~→ fronting 4 MCP servers · 61 tools · 12 contracts resolved
~
~export ANTHROPIC_BASE_URL=http://localhost:8080
~# every tool call from every agent now passes the mediator`,
  },
];

export const IDLE: DemoState = {
  v: "idle",
  flow: "waiting for a tool call",
  trust: "trust: —",
  audience: "audience: —",
};

/* Inline-code terms and their popover definitions, restating entries of
   the repository's docs/glossary.md. Golden: a glossary or spec change
   that touches a term here lands with the matching update in the same
   commit. Keys are the exact chip text as written in the markdown; a chip
   with no entry renders as plain code. */

const TERMS = {
  version: "The policy configuration dialect version.",
  trust_chain:
    "The ordered list of trust ranks, least-trusted first. Omitted, it defaults to suspicious < trusted.",

  /* Tool contracts */
  delta:
    "The label contribution of an admitted call result. A delta never expands permissions: it intersects reader sets, lowers the trust rank, or leaves the trajectory label unchanged.",
  requires:
    "The prerequisites for executing a tool call: rules on allowed readers, trust levels, and required history.",
  audience:
    "Who is allowed to see data. In a delta, it restricts who can receive the tool's result; in requires, it checks who the agent can send data to.",
  trust:
    "Whether data comes from a vetted source. In a delta, it marks tool output as trusted or suspicious; in requires, it sets the minimum trust level a tool demands.",
  trusted:
    "Data from a vetted source, or vouched to the rank by a mandated transition — a claim about the instruction channel, never about a value's honesty.",
  suspicious:
    "Data from an unvetted source, like external web content. Once ingested, the run stays suspicious.",
  public:
    "The reserved audience state meaning everyone: the complete reader universe, not a reader ID. An agent with public reach can send data to any outbound destination. Never a group member.",
  "@name":
    "A group: a directory-held reader set. The membership resolver turns the name into literal reader IDs when the engine reads it; the algebra never stores the name.",
  "@auditors":
    "A group: a directory-held reader set, resolved to literal reader IDs by the membership resolver when the engine reads it.",
  "[membership]":
    "The one registration every @name group resolves through. A group mention without it is a load error.",
  internal:
    "An example reader for restricted internal data. Reading internal data closes off public destinations.",
  "{public, trusted}":
    "The neutral starting label before reading any data: unrestricted outbound reach and trusted status.",
  egress:
    "A side effect where data leaves the system, recorded in the log on successful execution.",
  mutation:
    "A side effect where external state is modified, recorded in the log on successful execution.",
  effects:
    "What a successful tool call appends to the execution log, declared as effects = [...] in the contract.",
  "effects.has":
    "In a tool requires check: prior(k) — a matching effect must already exist in the log.",
  "effects.has_no":
    "In a tool requires check: no_prior(k) — no matching effect may exist, appended to the log or reserved by a dispatch still in flight.",
  emits:
    "What a successful tool call appends to the log, declared as effects = [...] in the contract.",
  exactly: "In an audience condition: the allowed reader set becomes precisely this list.",
  includes: "In a requires condition: the run's allowed readers must contain these.",
  cap: "In a requires condition: the run's allowed readers must stay within this set. In may_cast: the ceiling a resolved reader set must stay within.",
  tags: "Routing names with no algebraic life; the currency of authority, cast, and sanitizer scope.",

  /* Authorities */
  mandate:
    "The declared bound on a registered component's power: for an authority, what its rulings may cover; for a sanitizer, the one transition it may claim, on either dimension.",
  hint: "The deployer's own account of what an authority or sanitizer is for. Carried into every remedy plan naming it, so the agent chooses on stated purpose. Advisory: it grants nothing.",
  can_cover_trust_to: "The ceiling of a trust cover in an authority mandate.",
  can_cover_readers: "The ceiling of an audience cover in an authority mandate.",
  may_add:
    "The audience cover ceiling of an authority: the readers its rulings may cover.",
  can_waive: "The effect kinds an authority ruling may waive for one dispatch.",
  attends: "The attention marks an authority's rulings satisfy.",
  scope:
    "The tags a registered component (authority, cast, or sanitizer) has jurisdiction over.",
  attention: "Named marks demanding a fresh ruling on every dispatch.",
  'builtin = "hitl"':
    "The built-in human-in-the-loop authority: elicitation hosted by the harness rather than a remote resolver.",

  /* Sanitizers and Casts */
  on: "Where a sanitizer may apply: tool_output at an admission the host can withhold — a child return, or a tool result at a confined application point; tool_input as whole-argument substitution at dispatch.",
  tool_input:
    "A sanitizer application point: the sanitizer derives a replacement for the whole argument set of one call, and the harness dispatches exactly the substituted bytes.",
  tool_output:
    "A sanitizer application point: an admission the host can withhold — the child-return crossing, or a tool result at an application point the deployment confines. The derivation is admitted; the raw value is withheld.",
  from: "In a sanitizer mandate: the required source label state before the transition applies.",
  to: "In a sanitizer mandate: the exact target label state produced after the transition.",
  constant: "In a cast: every Unknown on the dimension resolves to one declared state.",
  resolver:
    "The dynamic implementation of a registered external: authority rulings, cast decisions, sanitizer derivations, or membership answers.",
  may_cast:
    "The ceiling on a cast resolver: the states it is allowed to resolve an Unknown value to.",
  "[child]": "Run configuration for child sub-executions.",
  return_sanitizer: "Configured default sanitizer for all child sub-execution returns.",
  "attest-schema":
    "The reserved builtin sanitizer of the quarantine exit: raises trust on a child return whose structure is shape-bounded (values the schema declares and bounds — no free text) and was bound at fork, up to the parent's fork-time rank. Claims instruction-cleanliness only.",

  /* Refusals & Model Terms */
  requirement_gaps: "Returned on a refusal: the unmet entries of requires.",
  narrowing: "The loss of reach a proposed flow would commit; returned on a refusal for the agent to accept, alone or inside a composed plan.",
  remedy_plans:
    "Returned on a refusal: exact valid paths forward to unblock execution.",
  unestablished:
    "Returned on a refusal: values whose needed dimension no registered cast could establish.",
  Unknown:
    "Unestablished label state on an unannotated or pending-cast value. Fails closed at requirement checks until resolved by a cast.",
  trajectory: "One agent run: its security label plus its append-only event log.",
} as const satisfies Record<string, string>;

export function termDefinition(chip: string): string | undefined {
  return (TERMS as Record<string, string>)[chip];
}

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
  trusted: "Data from a vetted source.",
  suspicious:
    "Data from an unvetted source, like external web content. Once ingested, the run stays suspicious.",
  public:
    "The reader representing everyone. An agent with public access can send data to any outbound destination.",
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
    "In a tool requires check: no_prior(k) — no matching effect may exist in the log.",
  emits:
    "What a successful tool call appends to the log, declared as effects = [...] in the contract.",
  exactly: "In an audience condition: the allowed reader set becomes precisely this list.",
  includes: "In a requires condition: the run's allowed readers must contain these.",
  cap: "In a requires condition: the run's allowed readers must stay within this set.",
  tags: "Routing names with no algebraic life; the currency of authority scope.",

  /* Authorities */
  mandate:
    "The declared bound on a registered component's power: for an authority, what its rulings may cover; for a sanitizer, the transition it may claim.",
  can_raise_trust_to: "The ceiling of a trust cover in an authority mandate.",
  can_add_readers: "The ceiling of an audience cover in an authority mandate.",
  may_add:
    "The audience cover ceiling of an authority: the readers its rulings may vouch for.",
  can_waive: "The effect kinds an authority ruling may waive for one dispatch.",
  attends: "The attention marks an authority's rulings satisfy.",
  scope: "The tags an authority has jurisdiction over.",
  attention: "Named marks demanding a fresh ruling on every dispatch.",
  'builtin = "hitl"':
    "The built-in human-in-the-loop authority: elicitation hosted by the harness rather than a remote resolver.",

  /* Sanitizers and Casts */
  from: "In a sanitizer mandate: the required source label state before the transition applies.",
  to: "In a sanitizer mandate: the exact target label state produced after the transition.",
  constant: "In a cast: every Unknown on the dimension resolves to one declared state.",
  resolver:
    "The dynamic implementation of a registered external: authority rulings, cast decisions, or sanitizer derivations.",
  may_cast:
    "The ceiling on a cast resolver: the states it is allowed to resolve an Unknown value to.",
  "[child]": "Run configuration for child sub-executions.",
  return_sanitizer: "Configured default sanitizer for all child sub-execution returns.",

  /* Refusals & Model Terms */
  requirement_gaps: "Returned on a refusal: the unmet entries of requires.",
  narrowing: "The loss of reach a proposed flow would commit; returned on a refusal as a soft-blocked choice.",
  remedy_plans:
    "Returned on a refusal: exact sound paths forward to unblock execution.",
  unestablished:
    "Returned on a refusal: values whose needed dimension no registered cast could establish.",
  Unknown:
    "Unestablished label state on an unannotated or pending-cast value. Fails closed at requirement checks until resolved by a cast.",
  trajectory: "One agent run: its security label plus its append-only event log.",
} as const satisfies Record<string, string>;

export function termDefinition(chip: string): string | undefined {
  return (TERMS as Record<string, string>)[chip];
}

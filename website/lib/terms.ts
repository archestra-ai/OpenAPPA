/* Inline-code terms and their popover definitions, restating entries of
   the model's vocabulary. Golden: a vocabulary change
   that touches a term here lands with the matching update in the same
   commit. Keys are the exact chip text as written in the markdown; a chip
   with no entry renders as plain code. */

const TERMS = {
  version: "The policy configuration dialect version.",
  trust_chain:
    "The ordered list of trust ranks, least-trusted first. Omitted, it defaults to suspicious < trusted.",

  /* Core Engine Concepts */
  label:
    "The security state attached to a trajectory, tracking allowed readers (audience) and trust rank.",
  log:
    "The append-only execution history recording tool dispatches, narrowing acceptances, authority approvals, and denials.",
  authority:
    "A registered external component (such as a human review prompt or approval service) empowered to approve specific out-of-bounds actions.",
  authorities:
    "Registered external components empowered to approve specific out-of-bounds actions under pre-configured mandates.",
  sanitizer:
    "A registered component (such as a PII scrubber or schema validator) that derives cleaner outputs to restore lost reach or raise trust.",
  sanitizers:
    "Registered components that derive cleaner outputs to restore lost reach or raise trust.",
  cast:
    "A registered component that resolves an Unknown label dimension to a concrete state under pre-configured ceilings.",
  casts:
    "Registered components that resolve Unknown label dimensions to concrete audience or trust states.",
  remedy:
    "An actionable path returned on a policy refusal explaining how to unblock execution safely.",
  remedies:
    "Actionable paths returned on a policy refusal explaining how to unblock execution safely (e.g. human approval, sanitizer, or narrowing acceptance).",

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
    "The reserved unrestricted audience state, not a reader ID: no audience restriction applies. An agent with public reach can send data to any outbound destination. As a placeholder argument it names the Public audience, which only a Public trajectory includes. Never a group member.",
  "@name":
    "A group: a directory-held reader set. The membership resolver turns the name into literal reader IDs when the engine reads it; the algebra never stores the name.",
  "@auditors":
    "A group: a directory-held reader set, resolved to literal reader IDs by the membership resolver when the engine reads it.",
  "[membership]":
    "The one registration every @name group resolves through. A group mention without it is a load error.",
  "[[dynamic_resolver]]":
    "A named external used by a dynamic audience binding or a tool-level resolver. A tool-level resolver reads all canonical arguments and returns its declared trust, audience, or attention fields.",
  argument:
    "In a dynamic audience form, the top-level string tool argument passed to the named dynamic resolver.",
  resolvers:
    "Tool-level dynamic resolver bindings. Each receives the complete canonical argument object before the call is checked.",
  returns:
    "The trust, audience, and attention fields a tool-level resolver must return. Trust values select from the policy trust chain; attention values select from marks named by authority mandates. Trust and audience have one owner; attention is additive.",
  "[externals.dynamic]":
    "The deployment binding shared by dynamic resolvers: either an HTTP endpoint or the tool-level-only Claude Code builtin.",
  internal:
    "An example reader for restricted internal data. Reading internal data closes off public destinations.",
  "{public, trusted}":
    "The neutral starting label before reading any data: unrestricted outbound reach and the trust chain's top rank — trusted under the default chain.",
  egress:
    "A side effect where data leaves the system, recorded in the log on successful execution.",
  mutation:
    "A side effect where external state is modified, recorded in the log on successful execution.",
  effects:
    "What a successful tool call appends to the execution log, declared as effects = [...] in the contract.",
  "effects.has":
    "In a tool requires check: prior(k) — a matching effect must already exist in the log.",
  "effects.has_no":
    "In a tool requires check: no_prior(k) — no matching effect may exist, appended to the log or under an unsettled reservation.",
  emits:
    "What a successful tool call appends to the log, declared as effects = [...] in the contract.",
  exactly: "In an audience condition: the allowed reader set becomes precisely this list.",
  includes: "In a requires condition: the run's allowed readers must contain these.",
  cap: "In a requires condition: the run's allowed readers must stay within this set. In may_cast: the ceiling a resolved audience must stay within; only a public cap admits a public resolution.",
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
  attends:
    "The attention marks an authority's rulings satisfy. Tool-level dynamic resolvers can demand only marks present in at least one authority mandate.",
  scope:
    "The tags a registered component (authority, cast, or sanitizer) has jurisdiction over.",
  attention: "Named marks demanding a fresh ruling on every dispatch.",
  'builtin = "hitl"':
    "The built-in human-in-the-loop authority: elicitation hosted by the harness rather than a remote resolver.",
  'builtin = "approve"':
    "The stock in-process authority that approves every consult within its mandate. An open gate the deployer chose deliberately — legitimate and visible in review.",

  /* Sanitizers and Casts */
  on: "Where a sanitizer may apply: tool_output at an admission the host can withhold — a child return, or a tool result at a confined application point; tool_input as whole-argument substitution at dispatch.",
  tool_input:
    "A sanitizer application point: the sanitizer derives a replacement for the whole argument set of one call, and the harness dispatches exactly the substituted bytes.",
  tool_output:
    "A sanitizer application point: an admission the host can withhold — the child-return crossing, or a tool result at an application point the deployment confines. The derivation is admitted; the raw value is withheld.",
  from: "In a sanitizer mandate: the required source label state before the transition applies.",
  to: "In a sanitizer mandate: the exact target label state produced after the transition.",
  constant: "In a cast: every covered Unknown value resolves to one declared complete label.",
  resolver:
    "The dynamic implementation of a registered external: authority rulings, cast decisions, sanitizer derivations, membership answers, audience bindings, or tool-contract fields.",
  may_cast:
    "The complete product ceiling on a cast resolver: the trust ranks and audience cap it may resolve an Unknown value within.",
  "[child]": "Run configuration for child sub-executions.",
  return_sanitizer: "Configured default sanitizer for all child sub-execution returns.",
  "attest-schema":
    "The reserved builtin sanitizer of the quarantine exit: raises trust on a child return whose structure is shape-bounded (values the schema declares and bounds — no free text) and was bound at fork, up to the parent's fork-time rank. Claims instruction-cleanliness only.",
  'builtin = "redact-email"':
    "The stock in-process sanitizer that replaces every email-like token in a value with a fixed placeholder, deriving under its declared mandate.",

  /* Refusals & Model Terms */
  requirement_gaps: "Returned on a refusal: the unmet entries of requires.",
  narrowing:
    "The loss of reach a proposed flow would commit. A raw path requires acceptance. An output-sanitizer path withholds the raw result; its derivation then admits, remains confined for another helpful sanitizer, or awaits acceptance of its exact residual.",
  remedy_plans:
    "Returned on a refusal: exact valid paths forward to unblock execution.",
  unestablished:
    "Returned on a refusal: values whose needed dimension no registered cast could establish.",
  Unknown:
    "Unestablished label state on an unannotated or pending-cast value. Fails closed at requirement checks until resolved by a cast.",
  trajectory: "One agent run: its security label plus its append-only event log.",
} as const satisfies Record<string, string>;

export function termDefinition(chip: string): string | undefined {
  const direct = (TERMS as Record<string, string>)[chip];
  if (direct) return direct;
  return (TERMS as Record<string, string>)[chip.toLowerCase()];
}

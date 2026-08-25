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
    "Registered external components empowered to approve specific out-of-bounds actions, each within what its permits table declares.",
  sanitizer:
    "A registered component (such as a PII scrubber or schema validator) that derives cleaner outputs to restore lost reach or raise trust.",
  sanitizers:
    "Registered components that derive cleaner outputs to restore lost reach or raise trust.",
  cast:
    "A registered component that resolves one whole Unknown value to one complete label, atomically, under pre-configured ceilings.",
  casts:
    "Registered components that resolve whole Unknown values to complete labels, atomically, under pre-configured ceilings.",
  "[[cast]]":
    "The registered resolution of an Unknown value: one complete label for the whole source, atomically.",
  remedy:
    "An actionable path returned on a policy refusal explaining how to unblock execution safely.",
  remedies:
    "Actionable paths returned on a policy refusal explaining how to unblock execution safely (e.g. human approval, sanitizer, or narrowing acceptance).",

  /* Tool contracts */
  "Tool(argument:pattern)":
    "An ordered tool contract selector. OpenAPPA uses the first contract whose top-level string argument matches the pattern. An asterisk matches any text; a bare tool name is the fallback. A sanitizer rewrite cannot move a call to another selector.",
  delta:
    "The label contribution of an admitted call result. A delta never expands permissions: it intersects reader sets, lowers the trust rank, or leaves the trajectory label unchanged.",
  requires:
    "The prerequisites for executing a tool call: rules on allowed readers, trust levels, and required history.",
  audience:
    "Who is allowed to see data. In a delta, it restricts who can receive the tool's result; in requires, it checks who the agent can send data to.",
  trust:
    "Whether data comes from a vetted source. In a delta, it marks tool output as trusted or suspicious; in requires, it sets the minimum trust level a tool demands.",
  trusted:
    "Data from a vetted source, or vouched to the rank by a sanitizer that permits the transition — a claim about the instruction channel, never about a value's honesty.",
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
    "A named external that classifies proposed tool calls. Its opaque non-empty name can contain dots. It declares the inputs a tool must supply and the contract destinations it owns through returns. It is distinct from @group membership resolution.",
  inputs:
    "The values a resolver reads. A tool maps each one from $tool_call. Without an explicit mapping, the resolver reads the complete tool call: name, description when declared, and arguments.",
  uses:
    "A tool's dynamic resolvers. Each entry names a registered resolver and maps every input that resolver declares. Omit it when the tool uses none.",
  returns:
    "The contract destinations an attached resolver owns and always answers with: delta.trust, delta.audience, requires.trust, requires.audience, requires.attention. Static values and other resolvers cannot own the same destinations. Trust values select from the policy trust chain; attention values select from marks authorities name under permits.attention.",
  "$tool_call":
    "The only source a resolver input reads. Its five forms are the complete call (name, description when declared, arguments), its name, its description, its arguments, and one top-level argument. Only $tool_call.description requires a declared description.",
  "[externals.dynamic]":
    "The shared HTTP endpoint for every dynamic resolver without an inline builtin. Requests carry the resolver name.",
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
  emits:
    "What a successful tool call appends to the log, declared as effects = [...] in the contract.",
  contains:
    "Under requires.audience: the current audience must include these readers; a $arg placeholder is allowed only here. Under requires.effects: the trajectory already recorded this effect.",
  within:
    "Under requires.audience: the current audience must be a subset of these readers. A tool_input rewrite cannot clear it.",
  excludes:
    "Under requires.effects: the effect is neither recorded in the trajectory nor reserved by an unsettled dispatch.",
  tags: "Routing names with no algebraic life. On a tool, the names that select it. On an authority, sanitizer, or cast, the tools it answers or the values it acts on; omitted, every tool. Attention routing ignores tags.",

  /* Authorities */
  permits:
    "What a registered component may do, declared in its own table. For an authority: which unmet requirements its rulings can clear, and how far. For a sanitizer: the one transition, on one dimension, its derivation can claim.",
  hint: "The deployer's own account of what an authority or sanitizer is for. Carried into every remedy plan naming it, so the agent chooses on stated purpose. Advisory: it grants nothing.",
  trust_below:
    "In an authority's permits: it can rule for a call whose trust requirement is unmet, for requirements up to this rank.",
  audience_missing:
    "In an authority's permits: it can rule for a call whose audience is missing required readers, up to these readers.",
  effects_containing:
    "In an authority's permits: it can rule for a call although the trajectory already contains one of these effects, so a failed excludes check is waived for that one dispatch.",
  attention:
    "In a tool's requires: named marks that demand a fresh ruling on every call; history never satisfies them. In an authority's permits: the marks its rulings satisfy. A mark routes to every authority that names it, whatever its tags.",
  "permits.attention":
    "In an authority's permits: the marks its rulings satisfy. The marks every authority lists form the set a resolver's attention answer selects from.",
  'builtin = "hitl"':
    "The built-in human-in-the-loop authority: elicitation hosted by the harness rather than a remote resolver.",
  'builtin = "approve"':
    "The stock in-process authority that approves every consult within what it permits. An open gate the deployer chose deliberately — legitimate and visible in review.",

  /* Sanitizers and Casts */
  on: "Where a sanitizer may apply: tool_output at an admission the host can withhold — a child return, or a tool result at a confined application point; tool_input as whole-argument substitution at dispatch.",
  tool_input:
    "A sanitizer application point: the sanitizer derives a replacement for one call's arguments, and the harness dispatches exactly those bytes. The rewrite keeps the proposal's resolver answers, does not consult again, and cannot select another ordered contract.",
  tool_output:
    "A sanitizer application point: an admission the host can withhold — the child-return crossing, or a tool result at an application point the deployment confines. The derivation is admitted; the raw value is withheld.",
  from: "In a sanitizer's permits: for audience, the readers the source audience must contain; for trust, the rank the source must meet or exceed.",
  to: "In a sanitizer's permits: the exact audience, or the exact trust rank, the derivation carries.",
  constant: "In a cast: every covered Unknown value resolves to one declared complete label.",
  resolver:
    "The dynamic implementation of a registered external: authority rulings, cast decisions, sanitizer derivations, membership answers, or tool-contract fields.",
  may_cast:
    "The complete ceiling on a cast resolver's answer: the trust ranks it must choose from, and the readers its audience must stay within. Only a ceiling of [\"public\"] admits a public answer.",
  "[child]": "Run configuration for child sub-executions.",
  return_sanitizer: "Configured default sanitizer for all child sub-execution returns.",
  "attest-schema":
    "The reserved builtin sanitizer of the quarantine exit: raises trust on a child return whose structure is shape-bounded (values the schema declares and bounds — no free text) and was bound at fork, up to the parent's fork-time rank. Claims instruction-cleanliness only.",
  'builtin = "redact-email"':
    "The stock in-process sanitizer that replaces every email-like token in a value with a fixed placeholder, deriving the label its permits table declares.",

  /* Refusals & Model Terms */
  requirement_gaps: "Returned on a refusal: the unmet entries of requires.",
  narrowing:
    "The loss of reach a proposed flow would commit. A raw path requires acceptance. An output-sanitizer path withholds the raw result; its derivation then admits, remains confined for another helpful sanitizer, or awaits acceptance of its exact residual.",
  remedy_plans:
    "Returned on a refusal: exact valid paths forward to unblock execution.",
  unestablished:
    "Returned on a refusal: each source, by value, whose needed dimension no registered cast reaches, with its unresolved dimensions. A registered cast that gives no answer decides nothing and lists nothing here.",
  Unknown:
    "Unestablished label state on an unannotated or pending-cast value. Not a rank: it is ordered against no rank. Fails closed at every consumer of the dimension — label requirements, sanitizer applicability and permits checks, pending-cast admission — until a cast resolves the value.",
  confined_results:
    "The deployment's list of result points the host withholds from the model. A pending-cast delta and output sanitization at a tool result both need the tool listed here.",
  trajectory: "One agent run: its security label plus its append-only event log.",
} as const satisfies Record<string, string>;

export function termDefinition(chip: string): string | undefined {
  const direct = (TERMS as Record<string, string>)[chip];
  if (direct) return direct;
  return (TERMS as Record<string, string>)[chip.toLowerCase()];
}

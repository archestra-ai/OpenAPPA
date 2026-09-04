/* Inline-code terms and their popover definitions, restating entries of
   the model's vocabulary. Golden: a vocabulary change
   that touches a term here lands with the matching update in the same
   commit. Keys are the exact chip text as written in the markdown; a chip
   with no entry renders as plain code. */

const TERMS = {
  version: "The policy configuration dialect version.",
  include:
    "Policy fragments composed by the root configuration. Root declarations run first, followed by included declarations in list order. Included files cannot include more files or replace root-wide settings. A root [[annotator]] replaces one included Annotator with the same name.",
  trust_chain:
    "The ordered list of trust ranks, least-trusted first. Omitted, it defaults to suspicious < trusted.",

  /* Core Engine Concepts */
  label:
    "The security state attached to a trajectory, tracking allowed readers (audience) and trust rank.",
  log:
    "The append-only execution history recording tool dispatches, narrowing acceptances, authority approvals, and denials.",
  authority:
    "A policy component empowered to approve specific out-of-bounds actions. A deployment binding provides its judgment; without one, it returns no answer and cannot release a call.",
  authorities:
    "Policy components empowered to approve specific out-of-bounds actions, each within what its permits table declares. An unbound authority returns no answer.",
  sanitizer:
    "A registered component (such as a PII scrubber or schema validator) that derives cleaner outputs to restore lost reach or raise trust.",
  sanitizers:
    "Registered components that derive cleaner outputs to restore lost reach or raise trust.",
  annotator:
    "A registered component that produces a tool call's complete contract — delta, requires, and effects — per call, inside its declared mandate. A tool routes through it with annotator = \"<name>\"; the wildcard tool routes the long tail through one.",
  annotators:
    "Registered components that produce complete per-call tool contracts inside their declared mandates.",
  annotation:
    "The complete concrete contract one released tool call carries: its delta, its requires, and the effects it emits. Written statically in the [[tool]] entry, or answered per call by an annotator; pinned to the exact call, so a rewrite is annotated afresh and replay never consults again.",
  "[[annotator]]":
    "A named producer of per-call tool contracts. It declares an optional policy-authored hint, an input mapping, and a mandate that bounds every answer. It may name builtin = \"claude-code\" or \"llm\"; otherwise the deployment binds it under [externals.annotators.<name>].",
  mandate:
    "The closed vocabulary an annotator's answers may use: ranks, audiences, marks, and effects. An omitted bound admits the whole policy vocabulary; public is always an admissible audience. Every transport's answer passes the same mandate validation.",
  remedy:
    "An actionable path returned on a policy refusal explaining how to unblock execution safely.",
  remedies:
    "Actionable paths returned on a policy refusal explaining how to unblock execution safely (e.g. human approval, sanitizer, or narrowing acceptance).",

  /* Tool contracts */
  "Tool(argument:pattern)":
    "An ordered tool contract selector. A selector holds one or more comma-separated argument:pattern clauses, and a contract matches only when every clause matches its own top-level string argument. OpenAPPA uses the first matching contract. An asterisk matches any text; a bare tool name is the fallback. A sanitizer rewrite that selects another contract is judged as a new call under it.",
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
    "A mention of a symbolic audience: @finance names a configured [[audience.group]], and @provider:selector reads a source collection directly. The mention stays symbolic in labels and the log; membership is read from the configured sources per act and pinned.",
  "@finance":
    "A mention of a configured named audience. It stays symbolic in labels and the log; its membership is read from the audience sources per act and pinned.",
  "[[audience.group]]":
    "One configured named audience: its bare name (mentioned as @name), an optional within assertion into a built-in audience, and the from selectors that supply its members. Multiple sources are unioned.",
  "[audience.self]":
    "The mapping of the built-in self audience: the viewer selectors of the configured sources. self is the deployment's configured operating principal — whoever the credentials represent.",
  "[audience.internal]":
    "The mapping of the built-in internal audience: full-membership collections, and for GitHub only explicitly selected organizations. Multiple sources are unioned.",
  self: "The innermost built-in audience: the deployment's configured operating principal — whoever the credentials represent, which need not be a person — extensionally the union of the configured viewer sources.",
  "[identity]":
    "The deployment's one identity implementation, canonicalizing each provider-reported member to one principal before exact reader comparison. The shipped verified-email is deterministic and network-free; a custom name binds under [externals.identity.<name>].",
  "verified-email":
    "The shipped identity implementation: a member with a verified email becomes that address under conservative normalization (domain case only); a member without one keeps its provider-qualified ID. The address is the principal, so a reader written as an address is the same reader the verified claim resolves to. Deterministic and network-free.",
  inputs:
    "The values an annotator reads, each mapped from $tool_call on its declaration. Without an explicit mapping, the annotator reads the complete tool call: name, description when declared, and arguments.",
  ranks:
    "In an annotator's mandate: the trust ranks its answers may write in delta.trust and requires.trust. Omitted, every rank in the trust chain.",
  audiences:
    "In an annotator's mandate: the literal readers a restricted audience answer may name. public is always admissible and is never listed as a reader; a symbolic audience is never admissible. Omitted, every reader the policy writes.",
  marks:
    "In an annotator's mandate: the attention marks its answers may require. Omitted, every mark an authority names under permits.attention.",
  "$tool_call":
    "The only source an annotator input reads. Its five forms are the complete call (name, description when declared, arguments), its name, its description, its arguments, and one top-level argument. Only $tool_call.description requires a declared description.",
  cwd: "The working directory the harness reported for a proposed call, carried on every annotation consult as artifact.cwd: the absolute path as written, or null when the harness reported none. Consult input only — never part of a label, a digest, or the annotation's identity.",
  "[externals.annotators.<name>]":
    "The deployment binding for one annotator that does not carry a builtin on its declaration: an HTTP endpoint or a local command. Every implementation receives the same consult and answers under the same mandate validation. Unsupported platforms reject command bindings when loading the configuration.",
  "[externals.<kind>.<name>]":
    "One deployment binding: a registered authority or sanitizer bound to exactly one of url, command, or builtin; an annotator without a declared builtin, an audience source, or a custom identity implementation, bound to url or command. A binding without a registration refuses the deployment, and so does an unbound sanitizer, annotator, referenced audience source, or custom identity implementation; an unbound authority returns no answer.",
  declaration:
    "The registered half of a consult: the component's hint and permits, an annotator's hint, input names, and mandate vocabulary, or an audience source's selector templates. The agent never writes it.",
  artifact:
    "The judged half of a consult: the call and its unmet requirements, the body to rewrite, an annotator's args and cwd, a selector or member to read, or the member claims to canonicalize. Never the trajectory.",
  internal:
    "The built-in organization audience, between self and public in the shipped chain. Symbolic in labels and the log; extensionally the union of the configured internal sources, the members of self, and every group declared within either. Reading internal data closes off public destinations.",
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
    "Under requires.audience: the current audience must sit within this audience; a tool_input rewrite cannot clear it. On an [[audience.group]]: the trusted policy assertion that the group sits within a built-in audience (self or internal).",
  excludes:
    "Under requires.effects: the effect is neither recorded in the trajectory nor reserved by an unsettled dispatch.",
  tags: "Routing names with no algebraic life. On a tool, the names that select it. On an authority or sanitizer, the tools it answers or the values it acts on; omitted, every tool. Attention routing ignores tags.",

  /* Authorities */
  permits:
    "What a registered component may do, declared in its own table. For an authority: which unmet requirements its rulings can clear, and how far. For a sanitizer: the one transition, on one dimension, its derivation can claim.",
  hint: "The deployer's trusted instruction for an authority, sanitizer, or annotator. It explains what the component covers, removes, or classifies. It enters the component's consult, and authority or sanitizer hints also enter remedy plans. Advisory: it grants nothing.",
  trust_below:
    "In an authority's permits: it can rule for a call whose trust requirement is unmet, for requirements up to this rank.",
  audience_missing:
    "In an authority's permits: it can rule for a call whose audience is missing required readers, up to these readers.",
  effects_containing:
    "In an authority's permits: it can rule for a call although the trajectory already contains one of these effects, so a failed excludes check is waived for that one dispatch.",
  attention:
    "In a tool's requires: named marks that demand a fresh ruling on every call; history never satisfies them. In an authority's permits: the marks its rulings satisfy. A mark routes to every authority that names it, whatever its tags.",
  "permits.attention":
    "In an authority's permits: the marks its rulings satisfy. The marks every authority lists form the set an annotation's attention answer selects from.",
  'builtin = "hitl"':
    "The built-in human-in-the-loop authority: elicitation hosted by the harness rather than a remote resolver.",
  'builtin = "approve"':
    "The stock in-process authority that approves every consult within what it permits. An open gate the deployer chose deliberately — legitimate and visible in review.",
  'builtin = "claude-code"':
    "The stock model transport on a Claude subscription: one isolated claude -p process per consult, given the component's declaration and the judged value, never the trajectory. An authority or sanitizer binds it under [externals]; an annotator names it on its declaration. The same caps apply as to any implementation.",
  'builtin = "llm"':
    "The stock model transport on an API key, through the deployment's [externals.llm] profile. Same consult rendering, same placement, and same per-kind caps as claude-code.",
  "[externals.llm]":
    "The deployment's one API-key model profile — provider (anthropic, openai, gemini, or ollama), model, optional url, token_env (required except for ollama), timeout, and concurrency — that every builtin = \"llm\" binding or declaration consults. A deployment without it refuses to open a policy that declares one.",

  /* Sanitizers */
  on: "Where a sanitizer may apply: tool_output at an admission the host can withhold — a child return, or a tool result at a confined application point; tool_input as whole-argument substitution at dispatch.",
  tool_input:
    "A sanitizer application point: the sanitizer derives a replacement for one call's arguments, and the harness dispatches exactly those bytes. The rewrite is judged by the ordered contract its arguments select; an annotation binds the exact call, so a rewrite of an annotator-backed tool is annotated afresh, whichever contract it selects.",
  tool_output:
    "A sanitizer application point: an admission the host can withhold — the child-return crossing, or a tool result at an application point the deployment confines. The derivation is admitted; the raw value is withheld.",
  from: "In a sanitizer's permits: for audience, the readers the source audience must contain; for trust, the rank the source must meet or exceed.",
  to: "In a sanitizer's permits: the exact audience, or the exact trust rank, the derivation carries.",
  resolver:
    "The implementation answering for one registered external: the endpoint, command, builtin, or model behind an authority, sanitizer, annotator, audience source, or identity binding.",
  return_schema:
    "Argument of the attest-schema plan in a spawn's return declaration: the shape-bounded JSON schema the child's return must match. The child is told the shape when it starts.",
  "attest-schema":
    "The reserved builtin sanitizer of the quarantine exit: raises trust on a child return whose structure is shape-bounded (values the schema declares and bounds — no free text) and was declared by the parent at the spawn, up to the parent's rank at that spawn. Claims instruction-cleanliness only.",
  'builtin = "redact-email"':
    "The stock in-process sanitizer that replaces every email-like token in a value with a fixed placeholder, deriving the label its permits table declares.",

  /* Refusals & Model Terms */
  requirement_gaps: "Returned on a refusal: the unmet entries of requires.",
  narrowing:
    "The loss of reach a proposed flow would commit. A raw path requires acceptance. An output-sanitizer path withholds the raw result; its derivation then admits, remains confined for another helpful sanitizer, or awaits acceptance of its exact residual.",
  remedy_plans:
    "Returned on a refusal: exact valid paths forward to unblock execution.",
  confined_results:
    "The deployment's list of result points the host withholds from the model. Output sanitization at a tool result needs the tool listed here; a provider-run tool cannot be listed, because its result reaches the model inside the inference call.",
  trajectory: "One agent run: its security label plus its append-only event log.",
} as const satisfies Record<string, string>;

export function termDefinition(chip: string): string | undefined {
  const direct = (TERMS as Record<string, string>)[chip];
  if (direct) return direct;
  return (TERMS as Record<string, string>)[chip.toLowerCase()];
}

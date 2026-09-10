/* Inline-code terms and their popover definitions, restating entries of
   the model's vocabulary. Golden: a vocabulary change
   that touches a term here lands with the matching update in the same
   commit. Keys are the exact chip text as written in the markdown; a chip
   with no entry renders as plain code. */

const TERMS = {
  context_control:
    "Declares that the integration can keep a child agent's data hidden from the parent and withhold its answer until OpenAPPA allows it. Setting this to true does not implement that behavior; the integration must already support it.",
  version: "The policy configuration dialect version.",
  include:
    "Policy fragments composed by the root configuration. Root declarations run first, followed by included declarations in list order. Included files cannot include more files or replace root-wide settings. A root [[policy.annotator]] replaces one included Annotator with the same name.",
  trust_chain:
    "The ordered list of trust ranks, least-trusted first. Omitted, it defaults to suspicious < trusted.",

  /* Core Engine Concepts */
  label:
    "The security state attached to a trajectory, tracking allowed readers (audience) and trust rank.",
  log:
    "The append-only execution history recording tool dispatches, narrowing acceptances, authority approvals, and denials.",
  authority:
    "Reviews a blocked tool call and can approve an exception within its permits. Approval applies to that call only.",
  authorities:
    "Components that review blocked tool calls. Each can approve only the requirements listed in its permits section.",
  sanitizer:
    "Cleans or validates data before an agent or tool receives it. Its permits specify the audience or trust rank allowed for the result.",
  sanitizers:
    "Components that clean or validate data before an agent or tool receives it, within the changes allowed by their permits.",
  annotator:
    "Determines a tool call's delta, requires (including attention), and effects. The tool selects it with annotator = \"<name>\". Its permits limit the values it can return.",
  annotators:
    "Components that determine each tool call's restrictions, requirements, and effects within their configured permits.",
  annotation:
    "The restrictions, requirements, and effects for one tool call. These come from a static tool contract or an annotator. Changing an annotated call requires a new annotation; replaying it uses the recorded answer.",
  "[[policy.annotator]]":
    "Declares an annotator, the call data it receives, and the values it may return. Select a built-in implementation here or configure its service under [externals.annotators.<name>].",
  remedy:
    "An actionable path returned on a policy refusal explaining how to unblock execution safely.",
  remedies:
    "Actionable paths returned on a policy refusal explaining how to unblock execution safely (e.g. human approval, sanitizer, or narrowing acceptance).",

  /* Tool identity */
  "canonical tool id":
    "The identity APPA records for a tool: <family>/<namespace>/<tool>, with families mcp, host, and agent. Policies accept canonical or host-native names. An unqualified kagent rule can span MCP servers; server restricts it to one connection. Discovery supplies evidence, not permission. A trajectory retains its opening policy and accepted tool identities.",
  "<family>/<namespace>/<tool>":
    "The shape of a canonical tool id. The family is mcp, host, or agent; each segment matches [A-Za-z0-9_.-]+; a namespace never contains __. The one id outside these families is appa/execute_remedy_plan.",
  "appa/execute_remedy_plan":
    "The runtime's own control tool, the one member of the appa family. A policy cannot declare it; the runtime recognizes it before any contract and runs the remedy plan the call quotes.",
  adapter:
    "The runtime's translation layer for host events and tool identities. It derives canonical tool ids, spawn-ness, and child names. The heavier host lifecycle belongs to a plugin; Claude Code and kagent are the initial hosts.",
  "raw tool spelling":
    "The host's spelling for a tool, such as mcp__github__create_issue in Claude Code. The agent keeps its normal names, and policies may use native names too. The adapter translates wire spellings to canonical identities for recording and back for model-facing suggestions.",

  /* Tool contracts */
  "Tool(argument:pattern)":
    "An ordered tool contract selector. Every argument:pattern clause must match its top-level string argument. OpenAPPA uses the first matching contract in authored order, including overlapping native and canonical names. An asterisk matches any argument text; a bare name is the fallback. A sanitizer rewrite selecting another contract is judged under that contract.",
  delta:
    "The label contribution of an admitted call result. A delta never expands permissions: it intersects reader sets, lowers the trust rank, or leaves the trajectory label unchanged. Its audience may be one selector placeholder such as @slack:channel/$channel_id, resolved from the call's arguments at check time.",
  requires:
    "The prerequisites for executing a tool call: rules on allowed readers, trust levels, and required history.",
  audience:
    "Who is allowed to see data. In a delta, it restricts who can receive the tool's result; in requires, it checks who the agent can send data to.",
  trust:
    "A rank that describes how much the data can be trusted. delta.trust can lower the trajectory's rank; requires.trust sets the minimum rank for a call. Allowed ranks come from trust_chain.",
  trusted:
    "The higher default trust rank. A tool can declare this rank for its result or require it before a call. A trusted rank does not guarantee that the data is factually correct.",
  suspicious:
    "Data from an unverified source, such as a web page. Reading it lowers the trajectory's trust and can block tools that require trusted input.",
  public:
    "The reserved unrestricted audience state, not a reader ID: no audience restriction applies. An agent with public reach can send data to any outbound destination. As a placeholder argument it names the Public audience, which only a Public trajectory includes. Never a group member.",
  "@name":
    "A mention of a symbolic audience: @finance names a configured [policy.audience.group.<name>], and @provider:selector reads a source collection directly. A selector segment written as $argument is a selector placeholder, filled from the call's argument at check time. The mention stays symbolic in labels and the log; membership is read from the configured sources per act and pinned.",
  "@finance":
    "A mention of a configured named audience. It stays symbolic in labels and the log; its membership is read from the audience sources per act and pinned.",
  "[policy.audience]":
    "Maps the built-in audiences to membership sources. self lists selectors of templates declared with feeds = self, the identity OpenAPPA acts for; internal lists selectors of templates declared with feeds = internal, and for GitHub only explicitly selected organizations. A provider exists in the policy when a selector here references it. Multiple sources are unioned.",
  "[policy.audience.group.<name>]":
    "One configured named audience, mentioned as @name: an optional within assertion into a built-in audience, and the from selectors that supply its members. Multiple sources are unioned.",
  self: "The identity OpenAPPA acts for. This can be a person or a service. The configured viewer sources supply its reader IDs, which are combined into the self audience.",
  lookup:
    "On [externals.audience.<provider>]: the name of another [externals.audience.<name>] entry that answers this provider's member lookups. OpenAPPA then also looks up every group member of that provider that is not an email address. A root entry whose only key is lookup routes a provider that an included battery binds.",
  selectors:
    "On [externals.audience.<provider>]: the selector templates the service understands, each with an optional feeds role, self or internal. Every selector or mention the policy writes must match one; a selector placeholder must match one with $argument only on <variable> segments. The declaration enters the policy identity, and every consult carries it as declaration.templates for the service to check.",
  readers:
    "On an [externals.audience.<name>] entry that a lookup names: an inline table from <provider>:<id> to reader ID. OpenAPPA answers member lookups from it without calling a service. A member absent from the table keeps its ID.",
  inputs:
    "The values an annotator reads, each mapped from $tool_call on its declaration. Without an explicit mapping, the annotator reads the complete tool call: name, description when declared, and arguments.",
  ranks:
    "Trust ranks an annotator may use in delta.trust and requires.trust. If omitted, it may use every rank in trust_chain.",
  audiences:
    "Audiences an annotator may use in its answer. public is always allowed and must not be listed here. An empty list allows only public; omitting the field allows audiences declared in the policy. A selector placeholder entry is instantiated per call, so the answer may name only the resource the call's arguments spell.",
  marks:
    "Attention marks an annotator may require. An empty list allows none. If omitted, it may use every mark declared in an authority's permits.attention.",
  "$tool_call":
    "The only source an annotator input reads. Its five forms are the complete call (name, description when declared, arguments), its name, its description, its arguments, and one top-level argument. Only $tool_call.description requires a declared description.",
  "[externals.annotators.<name>]":
    "Configures an annotator's HTTP service or local program. Local programs require Unix. Every implementation receives a consult request and must return values within the annotator's permits.",
  "[externals.<kind>.<name>]":
    "Configures how OpenAPPA calls a component. The kind identifies its role, such as sanitizers, and the name matches its policy declaration. Use url for a service or command for a local program. Authorities and sanitizers also accept builtin here.",
  declaration:
    "Instructions and limits that OpenAPPA includes in a consult request. These come from the policy, not from the agent. Their fields depend on the component receiving the request.",
  artifact:
    "The request data sent to a component: for example, a tool call to review, text to clean, or a member to look up.",
  internal:
    "Data for members of the organization, as defined by the policy. After reading it, the agent needs a permitted remedy to share data outside that audience.",
  "{public, trusted}":
    "The neutral starting label before reading any data: unrestricted outbound reach and the trust chain's top rank — trusted under the default chain.",
  egress:
    "A side effect where data leaves the system, recorded in the log on successful execution.",
  mutation:
    "A side effect where external state is modified, recorded in the log on successful execution.",
  effects:
    "What a successful tool call appends to the execution log, declared as effects = [...] in the contract.",
  emits:
    "The effects listed in an annotator's JSON answer. OpenAPPA records them when the tool succeeds. Policy TOML uses the field name effects.",
  contains:
    "Under requires.audience: the current audience must include these readers; a whole-entry $arg placeholder is allowed only here, and a selector placeholder such as @slack:channel/$channel_id is allowed here and in delta. Under requires.effects: the trajectory already recorded this effect.",
  within:
    "Under requires.audience: the current audience must sit within this audience; a tool_input rewrite cannot clear it. On a [policy.audience.group.<name>]: the trusted policy assertion that the group sits within a built-in audience (self or internal).",
  excludes:
    "Blocks a call if a listed effect is already recorded or declared by another call that has been allowed but has not finished.",
  tags: "Names that connect tools to authorities and sanitizers. One matching tag is enough. Without tags, an authority or sanitizer is not limited to particular tools. Attention approvals use permits.attention instead.",

  /* Authorities */
  permits:
    "Limits what a component may do. An authority can approve only the listed requirements. A sanitizer can make only the declared audience or trust change. An annotator can return only the permitted values.",
  hint: "Instructions from the policy for an authority, sanitizer, or annotator. Explains what to review, clean, or classify. It does not grant permission beyond permits.",
  trust_below:
    "Allows an authority to approve a call whose trust requirement is not met, up to the rank specified here.",
  audience_missing:
    "Allows an authority to approve sharing with readers outside the current audience, limited to the audience specified here.",
  effects_containing:
    "Allows an authority to approve a call blocked by excludes because one of these effects has already occurred.",
  attention:
    "Named approvals required for each tool call. An earlier approval does not satisfy a later call. Authorities can give the approvals listed in their permits.attention, regardless of their tags.",
  "permits.attention":
    "The named approvals an authority can give for a call. Annotators can require only marks declared by authorities in the policy.",
  'builtin = "hitl"':
    "Asks a person to approve or deny the call through the agent integration.",
  'builtin = "approve"':
    "Automatically approves every request within the authority's permits.",
  'builtin = "claude-code"':
    "Runs Claude Code locally to answer a component's request. Each request starts a new claude -p process with the policy instructions and request data. Requires Claude Code on the Unix machine running OpenAPPA.",
  'builtin = "llm"':
    "Uses the model configured under [externals.llm] to answer a component's request. The model receives the policy instructions and request data and must stay within the component's permits.",
  "[externals.llm]":
    "Selects the provider, model, authentication, and request limits shared by all builtin = \"llm\" components. This section is required when any component uses that implementation.",

  /* Sanitizers */
  on: "Selects the data a sanitizer can transform: tool_output for a tool result or child agent's answer, or tool_input for a tool call's arguments.",
  tool_input:
    "Lets a sanitizer replace a tool call's arguments. OpenAPPA checks the changed call before allowing it. The integration must use exactly the replacement arguments.",
  tool_output:
    "Lets a sanitizer transform a tool result or child agent's answer before the receiving agent reads it. The integration must keep the original data hidden.",
  from: "In a sanitizer's permits: for audience, the readers the source audience must contain; for trust, the rank the source must meet or exceed.",
  to: "The audience or trust rank assigned to a sanitizer's result.",
  resolver:
    "The implementation answering for one registered external: the endpoint, command, builtin, or model behind an authority, sanitizer, annotator, or audience source.",
  return_schema:
    "The JSON Schema a parent supplies when selecting an attest-schema plan. It specifies the fields and values the child may return. The child receives these requirements when it starts.",
  "attest-schema":
    "Checks a child agent's structured answer against a schema chosen before the child reads untrusted data. It can raise trust up to the parent's rank when the child started. The schema must restrict every field and exclude free text. It does not verify factual accuracy.",
  'builtin = "redact-email"':
    "Replaces email addresses with a fixed placeholder. It does not remove other private information. The result receives the audience or trust rank declared in permits.",

  /* Refusals & Model Terms */
  requirement_gaps: "Returned on a refusal: the unmet entries of requires.",
  narrowing:
    "The loss of reach a proposed flow would commit. A raw path requires acceptance. An output-sanitizer path withholds the raw result; its derivation then admits, remains confined for another helpful sanitizer, or awaits acceptance of its exact residual.",
  remedy_plans:
    "Returned on a refusal: exact valid paths forward to unblock execution.",
  confined_results:
    "Tools whose results the integration can keep hidden until OpenAPPA allows delivery. A tool must be listed here for its results to use an output sanitizer. Tools that run inside the model provider's service cannot be listed.",
  trajectory: "One agent run: its security label plus its append-only event log.",
} as const satisfies Record<string, string>;

export function termDefinition(chip: string): string | undefined {
  const direct = (TERMS as Record<string, string>)[chip];
  if (direct) return direct;
  return (TERMS as Record<string, string>)[chip.toLowerCase()];
}

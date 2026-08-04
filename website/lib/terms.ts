/* Inline-code terms and their popover definitions, restating entries of
   the repository's docs/glossary.md. Golden: a glossary or spec change
   that touches a term here lands with the matching update in the same
   commit. Keys are the exact chip text as written in the markdown; a chip
   with no entry renders as plain code. */

const TERMS = {
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
  emits:
    "What a successful tool call appends to the log, declared as effects = [...] in the contract.",
  exactly: "In an audience condition: the allowed reader set becomes precisely this list.",
  includes: "In a requires condition: the run's allowed readers must contain these.",
  cap: "In a requires condition: the run's allowed readers must stay within this set.",
  may_add:
    "The audience cover ceiling of an authority: the readers its rulings may vouch for.",
  may_cast:
    "The ceiling on a cast resolver: the states it is allowed to resolve an Unknown value to.",
  attention: "Named marks demanding a fresh ruling on every dispatch.",
  'builtin = "hitl"':
    "The built-in human-in-the-loop authority: elicitation hosted by the harness rather than a remote resolver.",
  requirement_gaps: "Returned on a refusal: the unmet entries of requires.",
  remedy_plans:
    "Returned on a refusal: exact sound paths forward to unblock execution.",
  unestablished:
    "Returned on a refusal: values whose needed dimension no registered cast could establish.",
} as const satisfies Record<string, string>;

export function termDefinition(chip: string): string | undefined {
  return (TERMS as Record<string, string>)[chip];
}

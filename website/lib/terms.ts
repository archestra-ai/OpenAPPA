/* Inline-code terms and their popover definitions, restating entries of
   the repository's docs/glossary.md. Golden: a glossary or spec change
   that touches a term here lands with the matching update in the same
   commit. Keys are the exact chip text as written in the markdown; a chip
   with no entry renders as plain code. */

const TERMS = {
  delta:
    "What a call's admitted result folds into the run's label. Always restrictive — it intersects the readers, lowers the trust, or leaves the label alone.",
  requires:
    "What must hold before a call may dispatch: conditions on the run's label, its history, and attention marks.",
  audience:
    "The readers dimension of a label. In a delta, the readers a result is limited to; in requires, a condition on the run's readers.",
  trust:
    "The trust dimension of a label. In a delta, the rank a result carries; in requires, the floor a call demands.",
  trusted: "A trust rank, above suspicious. The fold keeps the lower of two ranks.",
  suspicious: "A trust rank, below trusted. The fold keeps the lower of two ranks.",
  public: "The reader id meaning everyone. A run whose audience includes public can reach any recipient.",
  internal:
    "A reader id from the examples. A run whose audience is exactly internal can reach only that reader, and public sinks are closed to it.",
  "{public, trusted}":
    "A whole label: audience public — everyone may read — at the trusted rank. The neutral starting point when nothing has been read yet.",
  egress: "An effect kind: data left the system through this call. A successful call appends its effects to the log.",
  mutation:
    "An effect kind: the call changed something outside the run. A successful call appends its effects to the log.",
  effects: "The tool's declared emits: what a successful call appends to the run's log.",
  emits: "What a successful call appends to the log, declared as effects = [...] in the TOML.",
  exactly: "In an audience: the reader set becomes precisely this list.",
  includes: "In requires: the run's readers must contain these.",
  cap: "In requires: the run's readers must stay within this set.",
  may_add: "The ceiling of an authority's audience cover: the readers its rulings may vouch.",
  may_cast: "The ceiling on a cast resolver: the states it is allowed to resolve an Unknown value to.",
  attention: "Named marks demanding a fresh ruling on every dispatch.",
  'builtin = "hitl"':
    "The built-in human-in-the-loop authority: elicitation hosted by the harness rather than a remote resolver.",
  requirement_gaps: "Returned on a refusal: the unmet entries of requires.",
  remedy_plans: "Returned on a refusal: the sound ways out, most of them executable by id.",
  unestablished: "Returned on a refusal: values whose needed dimension no registered cast could establish.",
} as const satisfies Record<string, string>;

export function termDefinition(chip: string): string | undefined {
  return (TERMS as Record<string, string>)[chip];
}

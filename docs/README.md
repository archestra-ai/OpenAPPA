# OpenAPPA documentation

APPA is an information-flow policy engine for LLM agents. Before every tool
call it answers one question: may this data go there?

## What to read

| you are | read |
|---|---|
| deciding whether APPA fits | `guide.md` — one sitting |
| implementing the engine, or handing it to a model | `spec.md` with `glossary.md` |
| writing or reviewing a policy | `contracts.md` |
| asking why it works this way | `rationale.md` |
| after the formal treatment | `../paper/` |

## Precedence

`spec.md` is normative. Where the guide simplifies, the spec governs; where
the spec and an implementation disagree, the spec governs. `rationale.md`
explains decisions and settles none of them. The website is not a source of
truth and never feeds back into these files.

## Conventions

Spec rules carry stable ids by family: `POS` position and capability, `LBL`
labels, `CHK` the check, `RMD` remedies, `AUT` authorities, `RUL` rulings,
`SAN` sanitizers and casts, `LOG` effects and history, `BRN` branching,
`UNK` Unknown, `CFG` load-time rules, `EXT` external interfaces, `IMP`
implementation shape, `THR` threat model. Cite them from tests, issues and
the paper — ids outlive section numbers.

Sections that are not live carry a status: **design direction** for agreed
but unspecified, **deferred** for specified but unimplemented.

Prose in `docs/` follows the writing rules in the root `CLAUDE.md`.

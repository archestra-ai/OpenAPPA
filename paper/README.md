# Branch Before You Read (APPA) — AISec '26 draft

Draft submission to [AISec 2026](https://aisec.cc) (19th ACM Workshop on
Artificial Intelligence and Security, co-located with ACM CCS).

- Submission deadline: **July 24, 2026 (firm)**, https://aisec26.hotcrp.com/
- Format: ACM double-column (`acmart` sigconf), anonymized. Body ≤ 10 pages
  excluding bibliography and well-marked appendices (≤ 2 extra pages;
  12 pages total max).
- Required: an explicit Generative-AI-use declaration after the
  references/appendices (present in `appa.tex`; revise before submission).

## Layout

- `paper.md` — the source skeleton/outline the LaTeX draft was generated from
  (kept as the working design document).
- `appa.tex` — ACM `acmart` root file; draft-note macros `\todo`, `\fixme`,
  `\stub` render in color.
- `sections/*.tex` — one file per section; `sections/appendix.tex` holds the
  appendix stubs.
- `appa.bib` — bibliography (Zotero exports, hand-added classics, and resolved citations including Odersky et al. 2026).

## Build

```sh
make        # latexmk -pdf appa.tex
make watch  # rebuild on change
make clean
```

Requires TeX Live with `acmart`.

## Status

Draft with visible TODO/FIXME/STUB notes. Proofs are complete (Appendix A).
Notably outstanding: evaluation numbers (§8 is a protocol, not results), the
worked-example and rendered-call appendices, the GenAI declaration wording,
and CCS concepts regeneration.

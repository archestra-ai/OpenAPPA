# Branch Before You Read (APPA)

LaTeX source for *Agentic Permissions Policy Algebra for Taint Confinement in
LLM Agents*.

- Submitted to [AISec 2026](https://aisec.cc) (19th ACM Workshop on Artificial
  Intelligence and Security, co-located with ACM CCS), July 24 2026.
- `appa.tex` is now the **public, non-anonymized** version (arXiv preprint).
  Rebuild the anonymized submission version with
  `git show 6e27400:paper/appa.tex > appa-anon.tex`.
- Format: ACM double-column (`acmart` sigconf, `nonacm`). Body ≤ 10 pages
  excluding bibliography and appendices.

## Layout

- `appa.tex` — ACM `acmart` root file; draft-note macros `\todo`, `\fixme`,
  `\stub` render in color (none currently used).
- `sections/*.tex` — one file per section; `sections/appendix.tex` holds the
  proofs (Appendix A) and benchmark prompts (Appendix B).
- `appa.bib` — bibliography (Zotero exports, hand-added classics, and resolved
  citations including Odersky et al. 2026).

## Build

```sh
make        # latexmk -pdf appa.tex
make watch  # rebuild on change
make arxiv  # appa-arxiv.tar.gz, ready to upload
make clean
```

Requires TeX Live with `acmart`.

`make arxiv` bundles the source plus the generated `appa.bbl`: arXiv does not
run BibTeX, so the `.bbl` must ship with the upload.

## Camera-ready checklist

Deferred while the paper is a preprint:

- Restore the commented-out `\setcopyright` / `\acmConference` / `\acmDOI` /
  `\acmISBN` block in `appa.tex`.
- Fill `\city` / `\country` in the author affiliations.
- Regenerate the CCS concepts at <https://dl.acm.org/ccs> — the current
  `CCSXML` block lists a third concept (Network security) with no matching
  `\ccsdesc`.

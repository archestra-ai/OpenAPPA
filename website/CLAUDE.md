# CLAUDE.md — website

This directory is the marketing/docs website (Next.js, pnpm). Three files
in it are golden: `content/docs/how-it-works.md`, the reader-facing
introduction to the model, `content/docs/contracts.md`, the policy-review
guide, and `lib/terms.ts`, the term-popover definitions. A change that
touches one of those files' claims lands together with the matching change
in the others, and a commit that leaves two of them contradicting each
other is not allowed.

Everything else under `website/content/` drifts from the actual project
and is **not** a source of truth.

**When working on anything APPA-related (engine semantics, terminology,
contracts), do not take website content other than `how-it-works.md`,
`contracts.md` and `lib/terms.ts` into account.** The authoritative
sources are those of the root `CLAUDE.md`'s Document precedence. If the
website contradicts them, the website is wrong — never propagate its
wording, claims, or terminology back into any APPA discussion.

Edits flow one way only: into the website, never the reverse. For
`how-it-works.md` and `contracts.md` that flow is same-commit; for the
rest of the content it is whenever someone gets to it.

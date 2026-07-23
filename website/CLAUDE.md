# CLAUDE.md — website

This directory is the marketing/docs website (Next.js, pnpm). Its content —
especially `website/content/docs/` — drifts from the actual project and is
**not** a source of truth.

**When working on anything APPA-related (spec, paper, engine semantics,
terminology, contracts), do not take website content into account.** The
authoritative sources are, in order: `docs/spec.md`, `docs/paper.md`, and the
root `CLAUDE.md`. If the website contradicts them, the website is wrong —
never propagate its wording, claims, or terminology back into the spec,
paper, or any APPA discussion.

Edits flow one way only: from the spec/paper into the website, never the
reverse.

---
title: Logo
category: Brand
order: 10
description: The pixel wordmark and its variants.
---

The OpenAPPA wordmark is a pixel glyph system: every letter is a 5-column bitmap on a 9-row grid. Capitals sit on rows 0–6, lowercase letters use a 5-row x-height, and descenders (like the `p`) drop to rows 7–8 — which is what makes the mixed case read at a glance.

One rule gives the mark its identity: **two-tone case** — capitals render in the strong text color, lowercase in the weak one, so `Open` + `APPA` separate without a space.

The mark is generated at render time by the `Logo` component (`components/Logo.tsx`); there are no image assets to keep in sync, and it inherits the light/dark palette automatically.

## Variants

:::logo-gallery:::

## Usage

- Use the **wordmark** variant everywhere by default; it is the only one that stays legible below 20px cap height, because the blocks are solid.
- The grid and dot variants exist for large-format uses (social cards, slides) where the pixel texture is visible.
- The favicon is the single capital-A glyph (`app/icon.svg`).

import type { CSSProperties } from "react";

import { PixelMark } from "@/components/Logo";

/* The landing hero's ambient figure: the four kinds of system OpenAPPA is
   built to sit inside, each drawn as a small window with a mascot living in
   it.

   The motion is deliberately small. Every mascot floats and blinks on its
   own offset, so the four never move in lockstep, and a slow round-robin
   warms one window's border at a time — the engine attending to one flow,
   then the next. Nothing slides, nothing loops a path; there is no rAF here
   at all, and `prefers-reduced-motion` stops all of it. */

/* 7x7 marks on the brand's pixel grid, so the window chrome reads in the
   same language as the mascot and the wordmark. Seven cells is barely enough
   for a recognisable shape: each of these survives because it is one idiom
   and nothing else — a prompt, a titlebar, a chevron pair, a cylinder. */
const ICON = 7;

const GLYPHS: Record<string, string[]> = {
  /* a shell prompt: chevron and cursor */
  terminal: [".......", ".#.....", "..#....", "...#...", "..#....", ".#.....", "....###"],
  /* an application window: solid title bar over an open body */
  window: ["#######", "#######", "#.....#", "#.....#", "#.....#", "#.....#", "#######"],
  /* a double chevron: something passing through on its way elsewhere */
  relay: [".......", "#.#....", ".#.#...", "..#.#..", ".#.#...", "#.#....", "......."],
  /* stacked records. A cylinder is the usual idiom, but at seven cells its
     discs collapse into the window glyph above; bars stay distinct. */
  store: [".......", "#######", ".......", "#######", ".......", "#######", "......."],
};

interface Surface {
  /** Window title — the category, as a reader would name it. */
  label: string;
  glyph: keyof typeof GLYPHS & string;
  /** One faint line of context, so the window reads as a real place. */
  line: string;
}

const SURFACES: Surface[] = [
  { label: "coding agents", glyph: "terminal", line: "$ claude" },
  { label: "SaaS apps", glyph: "window", line: "POST /chat" },
  { label: "proxies", glyph: "relay", line: "-> model" },
  { label: "data sources", glyph: "store", line: "SELECT *" },
];

function PixelGlyph({ rows }: { rows: string[] }) {
  return (
    <svg
      className="appa-runs-glyph"
      viewBox={`0 0 ${ICON} ${ICON}`}
      width={ICON}
      height={ICON}
      shapeRendering="crispEdges"
      aria-hidden="true"
    >
      {rows.flatMap((row, y) =>
        row.split("").map((cell, x) =>
          cell === "#" ? <rect key={`${x}-${y}`} x={x} y={y} width={1} height={1} fill="currentColor" /> : null,
        ),
      )}
    </svg>
  );
}

export function WhereAppaRuns() {
  return (
    <div
      className="appa-runs"
      role="img"
      aria-label="OpenAPPA runs inside coding agents, SaaS apps, proxies and data sources"
    >
      {SURFACES.map((surface, i) => (
        <div key={surface.label} className="appa-runs-card" style={{ "--i": i } as CSSProperties}>
          <div className="appa-runs-chrome">
            <PixelGlyph rows={GLYPHS[surface.glyph]} />
            <span>{surface.label}</span>
          </div>
          <div className="appa-runs-body">
            <span className="appa-runs-line">{surface.line}</span>
            <PixelMark
              className="appa-runs-mark"
              size={38}
              style={
                {
                  /* Negative delays start each mascot mid-cycle, so the row is
                     already desynchronised on the first paint rather than
                     drifting apart over the first few seconds. */
                  "--appa-float-delay": `${(-1.3 * i).toFixed(1)}s`,
                  "--appa-blink-delay": `${(-1.9 * i).toFixed(1)}s`,
                } as CSSProperties
              }
            />
          </div>
        </div>
      ))}
    </div>
  );
}

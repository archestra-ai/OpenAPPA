import type { CSSProperties, ReactNode } from "react";

/* Pixel-grid wordmark: each letter is a 5-wide bitmap on a 9-row grid —
   caps on rows 0-6, lowercase x-height on rows 2-6, descenders on rows 7-8.
   Solid blocks (no pixel gaps) so it stays crisp at small sizes. Lowercase
   letters render dimmed for the two-tone look. */

const GLYPHS: Record<string, string[]> = {
  O: ["01110", "10001", "10001", "10001", "10001", "10001", "01110", "00000", "00000"],
  A: ["01110", "10001", "10001", "11111", "10001", "10001", "10001", "00000", "00000"],
  P: ["11110", "10001", "10001", "11110", "10000", "10000", "10000", "00000", "00000"],
  p: ["00000", "00000", "11110", "10001", "10001", "10001", "11110", "10000", "10000"],
  e: ["00000", "00000", "01110", "10001", "11111", "10000", "01111", "00000", "00000"],
  n: ["00000", "00000", "10110", "11001", "10001", "10001", "10001", "00000", "00000"],
};

const CELL = 10;
const LETTER_COLS = 6; // 5 glyph columns + 1 spacing column
const ROWS = 9;
const CAP_ROWS = 7;

/* Pixel-grid mascot on the same grid as the wordmark, ported from the
   landing page's <appa-mark> custom element (app/landing/pixel-marks.ts).
   1 body · 3 dim (muzzle, paws) · 2 nose · 4 eyes */

const MARK_COLS = 24;
const MARK_ROWS = 22;

function seg(...runs: [string, number][]): string {
  let s = "";
  for (const [ch, n] of runs) s += ch.repeat(n);
  return s;
}

const BEAST = [
  seg([".", 5], ["1", 2], [".", 10], ["1", 2], [".", 5]),
  seg([".", 5], ["1", 2], [".", 10], ["1", 2], [".", 5]),
  seg([".", 4], ["1", 16], [".", 4]),
  seg([".", 3], ["1", 18], [".", 3]),
  seg([".", 3], ["1", 18], [".", 3]),
  seg([".", 3], ["1", 18], [".", 3]),
  seg([".", 3], ["1", 3], ["4", 3], ["1", 6], ["4", 3], ["1", 3], [".", 3]),
  seg([".", 3], ["1", 3], ["4", 3], ["1", 6], ["4", 3], ["1", 3], [".", 3]),
  seg([".", 3], ["1", 3], ["4", 3], ["1", 6], ["4", 3], ["1", 3], [".", 3]),
  seg([".", 3], ["1", 18], [".", 3]),
  seg([".", 3], ["1", 7], ["3", 4], ["1", 7], [".", 3]),
  seg([".", 3], ["1", 7], ["3", 1], ["2", 2], ["3", 1], ["1", 7], [".", 3]),
  seg([".", 3], ["1", 18], [".", 3]),
  seg([".", 4], ["1", 16], [".", 4]),
  seg([".", 1], ["1", 22], [".", 1]),
  seg(["1", 24]),
  seg(["1", 24]),
  seg(["1", 24]),
  seg(["1", 24]),
  seg(["1", 24]),
  seg(["1", 5], [".", 2], ["1", 4], [".", 2], ["1", 4], [".", 2], ["1", 5]),
  seg(["3", 5], [".", 2], ["3", 4], [".", 2], ["3", 4], [".", 2], ["3", 5]),
];

const MARK_FILL: Record<string, string> = {
  "1": "var(--text-strong)",
  "3": "var(--text-weak)",
  "2": "var(--bg)",
  "4": "var(--bg)",
};

export function PixelMark({ size = 26, style }: { size?: number; style?: CSSProperties }) {
  const height = Math.round((size * MARK_ROWS) / MARK_COLS);
  const body: ReactNode[] = [];
  const eyes: ReactNode[] = [];
  BEAST.forEach((row, y) => {
    for (let x = 0; x < MARK_COLS; x++) {
      const c = row[x];
      if (c === ".") continue;
      // Body pixel behind each eye: blinking scales the eye away and must
      // reveal fur, not the page background (the eye fill IS the bg color).
      if (c === "4") {
        body.push(<rect key={`b${x}-${y}`} x={x} y={y} width={1} height={1} fill={MARK_FILL["1"]} />);
      }
      const rect = <rect key={`${x}-${y}`} x={x} y={y} width={1} height={1} fill={MARK_FILL[c]} />;
      (c === "4" ? eyes : body).push(rect);
    }
  });
  return (
    <svg
      viewBox={`0 0 ${MARK_COLS} ${MARK_ROWS}`}
      width={size}
      height={height}
      shapeRendering="crispEdges"
      role="img"
      aria-label="OpenAPPA mascot"
      style={{ display: "block", overflow: "visible", ...style }}
    >
      <g className="appa-mark-float">
        {body}
        <g className="appa-mark-eyes">{eyes}</g>
      </g>
    </svg>
  );
}

interface Pixel {
  x: number;
  y: number;
  dim: boolean;
}

function layoutWord(word: string) {
  const pixels: Pixel[] = [];
  word.split("").forEach((letter, index) => {
    const glyph = GLYPHS[letter];
    if (!glyph) return;
    const dim = letter === letter.toLowerCase();
    glyph.forEach((row, y) => {
      row.split("").forEach((bit, x) => {
        if (bit === "1") pixels.push({ x: index * LETTER_COLS + x, y, dim });
      });
    });
  });
  return { pixels, width: (word.length * LETTER_COLS - 1) * CELL };
}

export interface WordmarkOptions {
  /** Gap around each pixel inside its cell; 0 = solid blocks. */
  gap?: number;
  shape?: "square" | "dot" | "rounded";
  /** Render lowercase letters in the weak color. */
  dimLowercase?: boolean;
}

export function PixelWordmark({
  word,
  capHeight,
  style,
  gap = 0,
  shape = "square",
  dimLowercase = true,
}: {
  word: string;
  /** Fixed cap height in px; omit for fluid sizing via `style`. */
  capHeight?: number;
  style?: CSSProperties;
} & WordmarkOptions) {
  const { pixels, width } = layoutWord(word);
  const height = ROWS * CELL;
  const boxHeight = capHeight ? (capHeight * ROWS) / CAP_ROWS : undefined;
  const size = CELL - gap * 2;
  return (
    <svg
      viewBox={`0 0 ${width} ${height}`}
      height={boxHeight}
      width={boxHeight ? (boxHeight * width) / height : undefined}
      style={style}
      preserveAspectRatio="xMinYMid meet"
      role="img"
      aria-label="OpenAPPA"
      shapeRendering={shape === "dot" ? "auto" : "crispEdges"}
    >
      {pixels.map(({ x, y, dim }) => {
        const fill = dimLowercase && dim ? "var(--text-weak)" : "currentColor";
        if (shape === "dot") {
          return (
            <circle key={`${x}-${y}`} cx={x * CELL + CELL / 2} cy={y * CELL + CELL / 2} r={size / 2} fill={fill} />
          );
        }
        return (
          <rect
            key={`${x}-${y}`}
            x={x * CELL + gap}
            y={y * CELL + gap}
            width={size}
            height={size}
            rx={shape === "rounded" ? 2 : 0}
            fill={fill}
          />
        );
      })}
    </svg>
  );
}

/** `height` is the cap height in px; the descender extends below it. */
export function Logo({ height = 15 }: { height?: number }) {
  const labelSize = Math.round(height * 0.55);
  return (
    <span style={{ display: "inline-flex", alignItems: "center", gap: height * 0.7 }}>
      <PixelMark size={Math.round((height * 26) / 15)} />
      <span style={{ display: "inline-flex", alignItems: "flex-start", gap: height * 0.45 }}>
        <PixelWordmark word="OpenAPPA" capHeight={height} />
        <span
          className="logo-tagline"
          style={{
            fontSize: labelSize,
            lineHeight: 1,
            marginTop: height - labelSize,
            letterSpacing: "0.08em",
            textTransform: "uppercase",
            color: "var(--text-weak)",
            fontFamily: "var(--font-mono)",
          }}
        >
          Preview &amp; RFC
        </span>
      </span>
    </span>
  );
}

/** Raw pixel grid of the wordmark, for canvas renderers (e.g. tutorial figures). */
export function logoPixelData() {
  const { pixels, width } = layoutWord("OpenAPPA");
  return { pixels, width, cell: CELL, capHeight: CAP_ROWS * CELL };
}

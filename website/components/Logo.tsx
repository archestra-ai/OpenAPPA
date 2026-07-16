import type { CSSProperties } from "react";

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
  return <PixelWordmark word="OpenAPPA" capHeight={height} />;
}

/** Raw pixel grid of the wordmark, for canvas renderers (e.g. tutorial figures). */
export function logoPixelData() {
  const { pixels, width } = layoutWord("OpenAPPA");
  return { pixels, width, cell: CELL, capHeight: CAP_ROWS * CELL };
}

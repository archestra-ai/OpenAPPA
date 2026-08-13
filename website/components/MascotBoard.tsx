import { PixelMark } from "@/components/Logo";

/* The Advisory Board as an amphitheater seen from the stage: three arced
   rows of OpenAPPA mascots — small at the back, large up front, row ends
   wrapping down toward the stage edge below — every mascot floating and
   blinking on its own schedule. Rendered by the :::mascot-board:::
   directive on the Advisory Board page. */

const ROWS = [
  { size: 30, count: 10, curve: 10, gap: 26 },
  { size: 42, count: 8, curve: 18, gap: 38 },
  { size: 56, count: 6, curve: 26, gap: 50 },
];

/* A row is one arc, and an arc only reads as an arc unbroken: at a fixed seat
   size the widest row is ~600px, so on a phone it either overflows the page or
   wraps into a column of stragglers. Instead every length in a row — seat,
   gap, and the curve's drop — is a multiple of `--seat`, which shrinks to
   whatever the column allows. The arc keeps its shape at any width. */
export function MascotBoard() {
  return (
    <div
      className="mascot-theater"
      role="img"
      aria-label="The Advisory Board: rows of OpenAPPA mascots seated like a theater audience facing a stage"
    >
      {ROWS.map((row, r) => {
        const gapRatio = row.gap / row.size;
        // Seats plus gaps span the row, so this divisor converts row width
        // into the seat width that exactly fills it.
        const span = row.count + (row.count - 1) * gapRatio;
        return (
          <div
            key={r}
            className="mascot-theater-row"
            style={
              {
                "--seat": `min(${row.size}px, calc(100% / ${span.toFixed(4)}))`,
                "--seat-gap": gapRatio.toFixed(4),
              } as React.CSSProperties
            }
          >
            {Array.from({ length: row.count }, (_, i) => {
              const t = row.count === 1 ? 0 : (i - (row.count - 1) / 2) / ((row.count - 1) / 2);
              const drop = ((row.curve * t * t) / row.size).toFixed(4);
              const seat = r * 7 + i;
              return (
                <span key={i} className="mascot-seat" style={{ transform: `translateY(calc(var(--seat) * ${drop}))` }}>
                  <PixelMark
                    className="mascot-mark"
                    size={row.size}
                    style={
                      {
                        "--appa-float-delay": `${(-0.9 * seat).toFixed(1)}s`,
                        "--appa-blink-delay": `${(-1.7 * seat).toFixed(1)}s`,
                      } as React.CSSProperties
                    }
                  />
                </span>
              );
            })}
          </div>
        );
      })}
    </div>
  );
}

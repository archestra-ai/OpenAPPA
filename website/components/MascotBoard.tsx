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

export function MascotBoard() {
  return (
    <div
      className="mascot-theater"
      role="img"
      aria-label="The Advisory Board: rows of OpenAPPA mascots seated like a theater audience facing a stage"
    >
      {ROWS.map((row, r) => (
        <div key={r} className="mascot-theater-row" style={{ gap: row.gap }}>
          {Array.from({ length: row.count }, (_, i) => {
            const t = row.count === 1 ? 0 : (i - (row.count - 1) / 2) / ((row.count - 1) / 2);
            const drop = row.curve * t * t;
            const seat = r * 7 + i;
            return (
              <span key={i} style={{ display: "inline-block", transform: `translateY(${drop.toFixed(1)}px)` }}>
                <PixelMark
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
      ))}
    </div>
  );
}

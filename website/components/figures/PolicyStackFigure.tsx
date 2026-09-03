import { PixelMark } from "@/components/Logo";

/* Static scheme for the Basic Principles page: a stack of OpenAPPA policy
   files fanning out to every place the same configuration can be enforced.
   Rendered by the :::fig-policy-stack::: directive. */

const TARGETS = ["Coding Agents", "LLM Proxies", "MCP Gateways", "MCP Servers", "Agents in Production"];

const CARD = { x: 60, y: 150, w: 180, h: 116, step: 12 };
const TARGET = { x: 470, w: 214, h: 44, gap: 14, y0: 26 };

export function PolicyStackFigure() {
  const targetMid = (i: number) => TARGET.y0 + i * (TARGET.h + TARGET.gap) + TARGET.h / 2;
  const stackRight = CARD.x + 2 * CARD.step + CARD.w;
  const stackMid = CARD.y - 2 * CARD.step + (CARD.h + 2 * CARD.step) / 2;

  return (
    <div className="policy-stack-figure">
      <svg viewBox="0 0 720 320" role="img" aria-label="A stack of OpenAPPA policy TOML files applied to coding agents, LLM proxies, MCP gateways, MCP servers, and agents in production">
        <defs>
          <marker id="psf-arrow" viewBox="0 0 8 8" refX="7" refY="4" markerWidth="7" markerHeight="7" orient="auto-start-reverse">
            <path d="M 0 0 L 8 4 L 0 8 z" fill="var(--icon)" />
          </marker>
        </defs>

        {/* flows: one policy stack, many enforcement points */}
        {TARGETS.map((_, i) => (
          <path
            key={i}
            d={`M ${stackRight + 6} ${stackMid} C ${stackRight + 100} ${stackMid}, ${TARGET.x - 110} ${targetMid(i)}, ${TARGET.x - 8} ${targetMid(i)}`}
            fill="none"
            stroke="var(--icon)"
            strokeWidth="1.3"
            opacity="0.75"
            markerEnd="url(#psf-arrow)"
          />
        ))}

        {/* the policy stack: same file shape, three deep */}
        {[2, 1, 0].map((depth) => {
          const x = CARD.x + (2 - depth) * CARD.step;
          const y = CARD.y - (2 - depth) * CARD.step;
          const isTop = depth === 0;
          return (
            <g key={depth}>
              <rect x={x} y={y} width={CARD.w} height={CARD.h} rx="8" fill="var(--bg)" stroke={isTop ? "var(--border)" : "var(--border-weak)"} strokeWidth="1.2" />
              {isTop && (
                <g fontFamily="var(--font-mono)" fontSize="11">
                  <text x={x + 14} y={y + 24} fill="var(--text-strong)" fontWeight="600">
                    appa.toml
                  </text>
                  <line x1={x + 14} y1={y + 34} x2={x + CARD.w - 14} y2={y + 34} stroke="var(--border-weak)" />
                  <text x={x + 14} y={y + 52} fill="var(--text-weak)">
                    version = 2
                  </text>
                  <text x={x + 14} y={y + 70} fill="var(--text-weak)">
                    [[tool]]
                  </text>
                  <text x={x + 14} y={y + 88} fill="var(--text-weak)">
                    delta = {"{ … }"}
                  </text>
                  <text x={x + 14} y={y + 106} fill="var(--text-weak)">
                    requires = {"{ … }"}
                  </text>
                </g>
              )}
            </g>
          );
        })}

        {/* enforcement points */}
        {TARGETS.map((label, i) => {
          const y = TARGET.y0 + i * (TARGET.h + TARGET.gap);
          return (
            <g key={label}>
              <rect x={TARGET.x} y={y} width={TARGET.w} height={TARGET.h} rx="8" fill="var(--bg-weak)" stroke="var(--border-weak)" strokeWidth="1.2" />
              <text x={TARGET.x + 18} y={y + TARGET.h / 2 + 4} fontFamily="var(--font-mono)" fontSize="13" fill="var(--text-strong)">
                {label}
              </text>
            </g>
          );
        })}
      </svg>
      {/* the mascot sits on top of the policy stack */}
      <span className="psf-mascot" aria-hidden>
        <PixelMark size={36} />
      </span>
    </div>
  );
}

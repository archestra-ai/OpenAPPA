const arrow = (id: string) => (
  <marker id={id} viewBox="0 0 8 8" refX="7" refY="4" markerWidth="7" markerHeight="7" orient="auto-start-reverse">
    <path d="M 0 0 L 8 4 L 0 8 z" fill="var(--icon)" />
  </marker>
);

export function BatteryRuleOrderFigure() {
  const rows = [
    [20, "Root appa.toml"],
    [113, "First file in include"],
    [206, "Next file in include"],
  ] as const;

  return (
    <div className="battery-figure battery-order-figure">
      <svg viewBox="0 0 520 290" role="img" aria-label="Root rules run first, followed by each included battery. Rules in every file run from top to bottom.">
        <defs>{arrow("battery-order-arrow")}</defs>

        <g fill="none" stroke="var(--icon)" strokeWidth="1.4" markerEnd="url(#battery-order-arrow)">
          <path d="M 260 84 V 105" />
          <path d="M 260 177 V 198" />
        </g>

        {rows.map(([y, label]) => (
          <g key={label}>
            <rect x="80" y={y} width="360" height="64" rx="6" fill="var(--bg-weak)" stroke="var(--border)" />
            <text x="100" y={y + 27} fill="var(--text-strong)" fontFamily="var(--font-mono)" fontSize="15">
              {label}
            </text>
            <text x="100" y={y + 48} fill="var(--text-weak)" fontFamily="var(--font-mono)" fontSize="12">
              Rules run from top to bottom
            </text>
          </g>
        ))}
      </svg>
    </div>
  );
}

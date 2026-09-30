import Link from "next/link";

import { BENCHMARK_HIGHLIGHT } from "@/lib/directive-content";

interface BenchRowProps {
  name: string;
  pct: number;
  label?: string;
  isSubject?: boolean;
}

function BenchRow({ name, pct, label, isSubject }: BenchRowProps) {
  return (
    <div className={`bench-row${isSubject ? " subject" : ""}`}>
      <span className="bench-row-name">{name}</span>
      <span className="bench-track">
        {pct > 0 && <span className="bench-bar" style={{ width: `${pct}%` }} />}
      </span>
      <span className="bench-row-value">{label ?? `${pct}%`}</span>
    </div>
  );
}

/** `link` is off where the chart already sits on the full results page. */
export function BenchmarkHighlight({ link = true }: { link?: boolean }) {
  return (
    <section className="bench-panel" aria-label="Benchmark results">
      <div className="bench-charts">
        {BENCHMARK_HIGHLIGHT.charts.map((chart) => (
          <figure className="bench-chart" key={chart.title}>
            <figcaption className="bench-chart-title">{chart.title}</figcaption>
            {chart.rows.map((row) => (
              <BenchRow isSubject={"subject" in row && row.subject} key={row.name} name={row.name} pct={row.pct} />
            ))}
          </figure>
        ))}
      </div>

      {link && (
        <Link className="bench-panel-link" href={BENCHMARK_HIGHLIGHT.link.href}>
          {BENCHMARK_HIGHLIGHT.link.label} →
        </Link>
      )}
    </section>
  );
}

import Link from "next/link";

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

export function BenchmarkHighlight() {
  return (
    <section className="bench-panel" aria-label="Benchmark results">
      <div className="bench-charts">
        <figure className="bench-chart">
          <figcaption className="bench-chart-title">
            Task completion
          </figcaption>
          <BenchRow name="Claude Auto mode" pct={90} />
          <BenchRow name="OpenAPPA" pct={89} isSubject />
          <BenchRow name="FIDES (Microsoft)" pct={41} />
        </figure>

        <figure className="bench-chart">
          <figcaption className="bench-chart-title">
            Attacks that succeeded
          </figcaption>
          <BenchRow name="Claude Auto mode" pct={10} />
          <BenchRow name="OpenAPPA" pct={0} isSubject />
          <BenchRow name="FIDES (Microsoft)" pct={31} />
        </figure>
      </div>

      <Link className="bench-panel-link" href="/evaluation">
        Read the full benchmark results →
      </Link>
    </section>
  );
}

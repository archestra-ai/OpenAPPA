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
      <span className="bench-panel-eyebrow">Benchmarks</span>

      <div className="bench-charts">
        <figure className="bench-chart">
          <figcaption className="bench-chart-title">
            Task completion <span className="bench-chart-hint">tasks completed</span>
          </figcaption>
          <BenchRow name="OpenAPPA" pct={89} isSubject />
          <BenchRow name="Defended FIDES" pct={41} />
        </figure>

        <figure className="bench-chart">
          <figcaption className="bench-chart-title">
            Attacks that succeeded <span className="bench-chart-hint">lower is better</span>
          </figcaption>
          <BenchRow name="OpenAPPA" pct={0} isSubject />
          <BenchRow name="Defended FIDES" pct={31} />
        </figure>
      </div>

      <p className="bench-panel-foot">
        Across 600 evaluated episodes over three frontier models in Bench-Corp, guarded OpenAPPA
        retained 89% task completion without a single observed policy breach. Full model breakdown
        and methodology are reported in the paper.
      </p>
      <Link className="bench-panel-link" href="/evaluation">
        Read the full benchmark results →
      </Link>
    </section>
  );
}

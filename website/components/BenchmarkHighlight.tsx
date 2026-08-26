import Link from "next/link";

interface BenchRowProps {
  name: string;
  min: number;
  max: number;
  label: string;
  isSubject?: boolean;
}

function BenchRow({ name, min, max, label, isSubject }: BenchRowProps) {
  return (
    <div className={`bench-row${isSubject ? " subject" : ""}`}>
      <span className="bench-row-name">{name}</span>
      <span className="bench-track">
        {max > 0 && (
          <>
            <span className="bench-bar" style={{ width: `${min}%` }} />
            {max > min && (
              <span
                className="bench-bar-interval"
                style={{ left: `${min}%`, width: `${max - min}%` }}
              />
            )}
          </>
        )}
      </span>
      <span className="bench-row-value">{label}</span>
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
          <BenchRow
            name="OpenAPPA"
            min={88}
            max={90}
            label="88–90%"
            isSubject
          />
          <BenchRow
            name="Defended FIDES"
            min={37}
            max={45}
            label="37–45%"
          />
        </figure>

        <figure className="bench-chart">
          <figcaption className="bench-chart-title">
            Attacks that succeeded <span className="bench-chart-hint">lower is better</span>
          </figcaption>
          <BenchRow
            name="OpenAPPA"
            min={0}
            max={0}
            label="0%"
            isSubject
          />
          <BenchRow
            name="Defended FIDES"
            min={28}
            max={35}
            label="28–35%"
          />
        </figure>
      </div>

      <p className="bench-panel-foot">
        Across three models and 20 Bench-Corp workflows, guarded OpenAPPA retained high task
        completion (88–90%) without a single observed policy breach. Full methodology and limitations
        are reported in the paper.
      </p>
      <Link className="bench-panel-link" href="/evaluation">
        Read the full benchmark results →
      </Link>
    </section>
  );
}

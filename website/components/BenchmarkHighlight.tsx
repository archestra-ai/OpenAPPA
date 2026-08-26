import Link from "next/link";

export function BenchmarkHighlight() {
  return (
    <section className="bench-panel" aria-label="Benchmark results">
      <span className="bench-panel-eyebrow">Benchmarks</span>

      <div className="bench-charts">
        <figure className="bench-chart">
          <figcaption className="bench-chart-title">
            Safe utility <span className="bench-chart-hint">tasks completed safely</span>
          </figcaption>
          <div className="bench-row subject">
            <span className="bench-row-name">OpenAPPA</span>
            <span className="bench-track">
              <span className="bench-bar" style={{ width: "90%" }} />
            </span>
            <span className="bench-row-value">88–90%</span>
          </div>
          <div className="bench-row">
            <span className="bench-row-name">Defended FIDES</span>
            <span className="bench-track">
              <span className="bench-bar" style={{ width: "17%" }} />
            </span>
            <span className="bench-row-value">9–17%</span>
          </div>
        </figure>

        <figure className="bench-chart">
          <figcaption className="bench-chart-title">
            Attacks that succeeded <span className="bench-chart-hint">lower is better</span>
          </figcaption>
          <div className="bench-row subject">
            <span className="bench-row-name">OpenAPPA</span>
            <span className="bench-track">
              <span className="bench-bar" style={{ width: "0%" }} />
            </span>
            <span className="bench-row-value">0%</span>
          </div>
          <div className="bench-row">
            <span className="bench-row-name">Defended FIDES</span>
            <span className="bench-track">
              <span className="bench-bar" style={{ width: "35%" }} />
            </span>
            <span className="bench-row-value">28–35%</span>
          </div>
        </figure>
      </div>

      <p className="bench-panel-foot">
        Across three models and 20 Bench-Corp workflows, guarded OpenAPPA retained high task
        completion without a successful observed attack. Full methodology and limitations are
        reported in the paper.
      </p>
      <Link className="bench-panel-link" href="/evaluation">
        Read the full benchmark results →
      </Link>
    </section>
  );
}

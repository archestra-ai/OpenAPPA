import Link from "next/link";

/* Headline result from the benchmarks page, rendered by the
   :::benchmark-highlight::: directive. Every figure restates one already on
   /evaluation — change them together.

   Form: two small multiples rather than one grouped chart. Attacks and task
   completion are different measures, so they get an axis each; putting them on
   one would be the dual-axis mistake. Within each, OpenAPPA carries the accent
   and the baselines recede to gray — emphasis, because the point is not "three
   arms differ" but "one arm wins both". Bars are labelled with their own value
   and arm name, so colour only reinforces what the text already says. */

interface Arm {
  name: string;
  attacks: number;
  attacksLabel: string;
  completion: number;
  completionLabel: string;
  detail: string;
}

const ARMS: Arm[] = [
  {
    name: "OpenAPPA",
    attacks: 0,
    attacksLabel: "0%",
    completion: 94.3,
    completionLabel: "94.3%",
    detail: "0 of 35 attacks succeeded · 33 of 35 tasks completed",
  },
  {
    name: "Unprotected",
    attacks: 28.6,
    attacksLabel: "28.6%",
    completion: 74.3,
    completionLabel: "74.3%",
    detail: "10 of 35 attacks succeeded · 26 of 35 tasks completed",
  },
  {
    name: "FIDES (Microsoft)",
    attacks: 22.9,
    attacksLabel: "22.9%",
    completion: 28.6,
    completionLabel: "28.6%",
    detail: "8 of 35 attacks succeeded · 10 of 35 tasks completed",
  },
];

const CHARTS = [
  { key: "attacks" as const, title: "Attacks that succeeded", hint: "lower is better" },
  { key: "completion" as const, title: "Tasks completed", hint: "higher is better" },
];

export function BenchmarkHighlight() {
  return (
    <section className="bench-panel" aria-label="Benchmark results">
      <span className="bench-panel-eyebrow">Benchmarks</span>

      <div className="bench-charts">
        {CHARTS.map((chart) => (
          <figure className="bench-chart" key={chart.key}>
            <figcaption className="bench-chart-title">
              {chart.title} <span className="bench-chart-hint">{chart.hint}</span>
            </figcaption>
            {ARMS.map((arm) => {
              const value = arm[chart.key];
              const label = chart.key === "attacks" ? arm.attacksLabel : arm.completionLabel;
              const isSubject = arm.name === "OpenAPPA";
              return (
                <div
                  className={`bench-row${isSubject ? " subject" : ""}`}
                  key={arm.name}
                  title={`${arm.name}: ${arm.detail}`}
                >
                  <span className="bench-row-name">{arm.name}</span>
                  <span className="bench-track">
                    <span className="bench-bar" style={{ width: `${value}%` }} />
                  </span>
                  <span className="bench-row-value">{label}</span>
                </div>
              );
            })}
          </figure>
        ))}
      </div>

      <p className="bench-panel-foot">
        On TAU-bench, OpenAPPA blocked <strong>24 of 24</strong> attempted violations with{" "}
        <strong>zero</strong> false positives across 8,151 policy-checked tool calls; half the
        blocked agents used the remedy plan to recover and finish the task.
      </p>
      <Link className="bench-panel-link" href="/evaluation">
        Read the full benchmark results →
      </Link>
    </section>
  );
}

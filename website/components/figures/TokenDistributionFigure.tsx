import tokenData from "@/data/bench-corp-token-distribution.json";

const MAX_TOKENS = 120_000;
const CHART_WIDTH = 760;
const CHART_HEIGHT = 320;
const PLOT_LEFT = 80;
const PLOT_RIGHT = 12;
const PLOT_TOP = 18;
const PLOT_BOTTOM = 218;
const BAR_WIDTH = 10;
const TICKS = [0, 30_000, 60_000, 90_000, 120_000];

type Episode = (typeof tokenData.episodes)[number];

interface ScenarioUsage {
  scenario: string;
  guarded: Episode;
  permissive: Episode;
}

const formatTokens = new Intl.NumberFormat("en-US");

function yForTokens(tokens: number) {
  return PLOT_BOTTOM - (tokens / MAX_TOKENS) * (PLOT_BOTTOM - PLOT_TOP);
}

function scenarioUsage(): ScenarioUsage[] {
  const guarded = tokenData.episodes.filter((episode) => episode.agent === "appa");
  return guarded
    .map((episode) => ({
      scenario: episode.scenario,
      guarded: episode,
      permissive: tokenData.episodes.find(
        (candidate) => candidate.agent === "appa-open" && candidate.scenario === episode.scenario,
      )!,
    }))
    .sort((a, b) => b.guarded.total_tokens - a.guarded.total_tokens);
}

export function TokenDistributionFigure() {
  const rows = scenarioUsage();
  const groupWidth = (CHART_WIDTH - PLOT_LEFT - PLOT_RIGHT) / rows.length;

  return (
    <figure className="token-distribution-figure" aria-labelledby="token-distribution-title">
      <header className="token-distribution-header">
        <h3 id="token-distribution-title">Provider-reported tokens by scenario</h3>
        <p className="token-distribution-meta">
          Run {tokenData.runId} · engine commit {tokenData.engineCommit}
          <br />
          {tokenData.model} · {tokenData.repetitions} repetition · {tokenData.scenarioCount} scenarios · {tokenData.promptCondition}
        </p>
        <div className="token-distribution-legend" aria-label="Legend">
          <span><i className="guarded" aria-hidden="true" />Guarded OpenAPPA</span>
          <span><i className="permissive" aria-hidden="true" />Permissive policy</span>
          <span><i className="budget-finalized" aria-hidden="true" />Budget finalized (24-call cap)</span>
        </div>
      </header>

      <div className="token-distribution-chart">
        <span className="token-distribution-scroll-cue">Scroll to see all 20 scenarios →</span>
        <svg
          viewBox={`0 0 ${CHART_WIDTH} ${CHART_HEIGHT}`}
          role="img"
          aria-label="Guarded and permissive provider-reported token totals for 20 Bench-Corp scenarios, sorted by guarded usage"
        >
          <defs>
            <pattern id="token-budget-hatch" width="6" height="6" patternUnits="userSpaceOnUse" patternTransform="rotate(45)">
              <rect width="6" height="6" className="token-budget-hatch-bg" />
              <line x1="0" y1="0" x2="0" y2="6" className="token-budget-hatch-line" />
            </pattern>
          </defs>

          {TICKS.map((tick) => {
            const y = yForTokens(tick);
            return (
              <g key={tick} aria-hidden="true">
                <line x1={PLOT_LEFT} x2={CHART_WIDTH - PLOT_RIGHT} y1={y} y2={y} className="token-distribution-gridline" />
                <text x={PLOT_LEFT - 8} y={y + 3} textAnchor="end" className="token-distribution-tick">
                  {tick === 0 ? "0" : `${tick / 1000}k`}
                </text>
              </g>
            );
          })}
          <text x={PLOT_LEFT - 8} y={PLOT_TOP - 7} textAnchor="end" className="token-distribution-axis-title" aria-hidden="true">tokens</text>

          {rows.map((row, index) => {
            const center = PLOT_LEFT + groupWidth * (index + 0.5);
            const guardedY = yForTokens(row.guarded.total_tokens);
            const permissiveY = yForTokens(row.permissive.total_tokens);
            const budgetFinalized = row.guarded.terminal_status === "budget_finalized";
            return (
              <g key={row.scenario}>
                <rect
                  x={center - BAR_WIDTH - 1}
                  y={guardedY}
                  width={BAR_WIDTH}
                  height={PLOT_BOTTOM - guardedY}
                  rx="1"
                  className={`token-distribution-column guarded${budgetFinalized ? " budget-finalized" : ""}`}
                >
                  <title>{`${row.scenario}, guarded OpenAPPA: ${formatTokens.format(row.guarded.total_tokens)} tokens${budgetFinalized ? ", budget finalized at the 24-call cap" : ""}`}</title>
                </rect>
                <rect
                  x={center + 1}
                  y={permissiveY}
                  width={BAR_WIDTH}
                  height={PLOT_BOTTOM - permissiveY}
                  rx="1"
                  className="token-distribution-column permissive"
                >
                  <title>{`${row.scenario}, permissive policy: ${formatTokens.format(row.permissive.total_tokens)} tokens`}</title>
                </rect>
                <text
                  x={center + 4}
                  y={PLOT_BOTTOM + 14}
                  textAnchor="end"
                  transform={`rotate(-45 ${center + 4} ${PLOT_BOTTOM + 14})`}
                  className="token-distribution-scenario"
                  aria-hidden="true"
                >
                  {row.scenario}
                </text>
              </g>
            );
          })}
        </svg>
      </div>

      <figcaption>
        Provider-reported tokens for the whole agent, including isolated child trajectories and recovery—not the OpenAPPA Engine alone.
      </figcaption>
    </figure>
  );
}

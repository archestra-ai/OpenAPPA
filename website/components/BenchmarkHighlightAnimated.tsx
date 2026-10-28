"use client";

import Link from "next/link";
import { useEffect, useRef, useState, type CSSProperties } from "react";

import { BENCHMARK_HIGHLIGHT } from "@/lib/directive-content";

/* The home-page benchmark panel, told as a short story. Each scene opens on a
   headline alone, which then fades into the chart that shows it:
   1. a strict guardrail is secure but completes nothing;
   2. a lenient one completes tasks but lets attacks through;
   3. OpenAPPA hits the ceiling the others stop at, and breaks through.
   Scenes 1–2 show one bar each, task completion then successful
   attacks; scene 3 shows the whole panel. The frame is a pure function
   of elapsed time; a requestAnimationFrame loop drives it once the panel
   scrolls into view. The numbers in scenes 1–2 are illustrative, not
   benchmark data; scene 3 ends on the real results. */

/* `neutral` paints the subject bar like the context arms: green is OpenAPPA's. */
type Cell = { pct: number; danger?: boolean; neutral?: boolean; glow?: number };

type Frame = {
  headline: { text: string; opacity: number };
  chart: number;
  /* Visibility of the task-completion and successful-attacks columns. */
  columns: [number, number];
  tasks: Cell;
  attacks: Cell;
  subjectName: string;
  competitors: number;
  wall: { opacity: number; split: number; strain: number };
};

const CHARTS = BENCHMARK_HIGHLIGHT.charts;
const FINAL = CHARTS.map((chart) => chart.rows.map((row) => row.pct as number));
const SUBJECT = { tasks: FINAL[0][0], attacks: FINAL[1][0] };
/* The wall sits at the best task completion among the arms below it. */
const CEILING = FINAL[0][2];
const LENIENT = { tasks: 90, attacks: 40 };

const HEADLINE = 2000;
const FADE = 400;

/* Scene windows, in ms: each starts with its headline, then its chart. */
const SCENES = [
  { headline: "Before, guardrails were either secure and broke agents…", start: 0, chart: 2000 },
  { headline: "…or didn’t break agents and weren’t secure.", start: 4000, chart: 2400 },
  { headline: "OpenAPPA breaks the trade-off.", start: 8400, chart: 4400 },
];
const FINALE = SCENES[SCENES.length - 1];
const DURATION = FINALE.start + HEADLINE + FINALE.chart;

const clamp01 = (x: number) => Math.min(1, Math.max(0, x));
const progress = (t: number, from: number, to: number) => clamp01((t - from) / (to - from));
const lerp = (a: number, b: number, p: number) => a + (b - a) * p;
const easeOut = (p: number) => 1 - (1 - p) ** 3;
const fadeInOut = (t: number, from: number, to: number) => Math.min(progress(t, from, from + FADE), 1 - progress(t, to - FADE, to));

const NO_WALL = { opacity: 0, split: 0, strain: 0 };

function frameAt(t: number): Frame {
  const index = SCENES.findLastIndex((scene) => t >= scene.start);
  const scene = SCENES[index];
  const chartStart = scene.start + HEADLINE;
  const last = index === SCENES.length - 1;
  const headline = { text: scene.headline, opacity: fadeInOut(t, scene.start, chartStart) };
  const chart = last ? progress(t, chartStart, chartStart + FADE) : fadeInOut(t, chartStart, chartStart + scene.chart);
  // Time since this scene's chart appeared.
  const s = t - chartStart;
  const base = { headline, chart, competitors: 0, wall: NO_WALL, subjectName: "" };

  switch (index) {
    case 0:
      return {
        ...base,
        columns: [1, 0],
        tasks: { pct: 0, danger: true, neutral: true },
        attacks: { pct: 0, neutral: true },
      };
    case 1: {
      const fill = easeOut(progress(s, 200, 1000));
      return {
        ...base,
        columns: [0, 1],
        tasks: { pct: LENIENT.tasks * fill, neutral: true },
        attacks: { pct: LENIENT.attacks * fill, danger: true, neutral: true },
      };
    }
    default: {
      const broken = s >= 2000;
      const strain = progress(s, 1200, 2000);
      const shake = !broken && s >= 1200 ? Math.sin(t / 22) * 0.8 * strain : 0;
      const tasks = broken
        ? lerp(CEILING, SUBJECT.tasks, easeOut(progress(s, 2000, 3000)))
        : CEILING * easeOut(progress(s, 200, 1200)) + shake;
      return {
        ...base,
        tasks: { pct: tasks, glow: broken ? 1 - progress(s, 3000, 4400) : 0 },
        attacks: { pct: SUBJECT.attacks },
        columns: [1, 1],
        subjectName: CHARTS[0].rows[0].name,
        competitors: easeOut(progress(s, 0, 1100)),
        wall: {
          opacity: broken ? 1 - progress(s, 2000, 2600) : easeOut(progress(s, 0, 400)),
          split: easeOut(progress(s, 2000, 2600)),
          strain,
        },
      };
    }
  }
}

function useTimeline() {
  const ref = useRef<HTMLElement>(null);
  const [elapsed, setElapsed] = useState(0);
  const [run, setRun] = useState(0);

  useEffect(() => {
    if (window.matchMedia("(prefers-reduced-motion: reduce)").matches) {
      setElapsed(DURATION);
      return;
    }
    let raf = 0;
    const play = () => {
      const start = performance.now();
      const tick = (now: number) => {
        // A frame timestamp can precede `start`, which was read mid-frame.
        const t = Math.min(Math.max(0, now - start), DURATION);
        setElapsed(t);
        if (t < DURATION) raf = requestAnimationFrame(tick);
      };
      raf = requestAnimationFrame(tick);
    };
    setElapsed(0);
    if (run > 0) {
      play();
      return () => cancelAnimationFrame(raf);
    }
    const observer = new IntersectionObserver(
      ([entry]) => {
        if (!entry.isIntersecting) return;
        observer.disconnect();
        play();
      },
      { threshold: 0.6 },
    );
    if (ref.current) observer.observe(ref.current);
    return () => {
      observer.disconnect();
      cancelAnimationFrame(raf);
    };
  }, [run]);

  return { ref, elapsed, replay: () => setRun((n) => n + 1) };
}

function Bar({ cell }: { cell: Cell }) {
  const glow = cell.glow ?? 0;
  return (
    <span
      className="bench-track"
      style={
        glow > 0
          ? { boxShadow: `0 0 ${12 * glow}px color-mix(in srgb, var(--accent) ${Math.round(70 * glow)}%, transparent)` }
          : undefined
      }
    >
      <span
        className="bench-bar"
        style={{
          width: `${Math.min(100, Math.max(0, cell.pct))}%`,
          background: cell.danger ? "var(--danger)" : cell.neutral ? "var(--text-weak)" : undefined,
        }}
      />
    </span>
  );
}

export function BenchmarkHighlightAnimated() {
  const { ref, elapsed, replay } = useTimeline();
  const frame = frameAt(elapsed);
  const subjectCells = [frame.tasks, frame.attacks];

  return (
    <section className="bench-panel bench-panel-animated" aria-label="Benchmark results" ref={ref}>
      <div className="bench-stage">
        <div className="bench-charts" style={{ opacity: frame.chart }}>
          {CHARTS.map((chart, c) => (
            <figure className="bench-chart" key={chart.title} style={{ opacity: frame.columns[c] }}>
              <figcaption className="bench-chart-title">{chart.title}</figcaption>
              {chart.rows.map((row, r) => {
                const subject = r === 0;
                const cell = subject ? subjectCells[c] : { pct: FINAL[c][r] * frame.competitors };
                return (
                  <div
                    className={`bench-row${subject ? " subject" : ""}`}
                    key={row.name}
                    style={subject ? undefined : { opacity: frame.competitors }}
                  >
                    <span className="bench-row-name">{subject ? frame.subjectName : row.name}</span>
                    <span className="bench-track-wrap">
                      <Bar cell={cell} />
                      {subject && c === 0 && frame.wall.opacity > 0 && (
                        <span
                          className="bench-wall"
                          style={
                            {
                              left: `${CEILING}%`,
                              opacity: frame.wall.opacity,
                              "--split": frame.wall.split,
                              "--strain": `${Math.round(frame.wall.strain * 100)}%`,
                            } as CSSProperties
                          }
                        >
                          <i />
                          <i />
                        </span>
                      )}
                    </span>
                    <span className="bench-row-value" style={cell.danger ? { color: "var(--danger)" } : undefined}>
                      {Math.round(Math.min(100, Math.max(0, cell.pct)))}%
                    </span>
                  </div>
                );
              })}
            </figure>
          ))}
        </div>
        {frame.headline.opacity > 0 && (
          <p
            className="bench-headline"
            aria-hidden="true"
            style={{ opacity: frame.headline.opacity, transform: `translateY(${(1 - frame.headline.opacity) * 6}px)` }}
          >
            {frame.headline.text}
          </p>
        )}
      </div>

      <div className="bench-panel-footer">
        <Link className="bench-panel-link" href={BENCHMARK_HIGHLIGHT.link.href}>
          {BENCHMARK_HIGHLIGHT.link.label} →
        </Link>
        <button className="bench-replay" type="button" onClick={replay}>
          Replay
        </button>
      </div>
    </section>
  );
}

"use client";

import React, { useCallback, useEffect, useRef, useState } from "react";

import {
  IDLE,
  INSTALLERS,
  LAYERS,
  lines,
  SCENARIOS,
  SOURCES,
  SYNTAXES,
  type DemoState,
} from "./landing-data";
import { registerPixelMarks } from "./pixel-marks";

declare module "react" {
  namespace JSX {
    interface IntrinsicElements {
      "appa-mark": React.DetailedHTMLProps<React.HTMLAttributes<HTMLElement>, HTMLElement> & {
        size?: number | string;
      };
      "appa-word": React.DetailedHTMLProps<React.HTMLAttributes<HTMLElement>, HTMLElement> & {
        word?: string;
        cap?: number | string;
      };
    }
  }
}

// Style strings are verbatim from Landing.dc.html; parsing them instead of
// hand-transcribing object literals keeps the port pixel-identical.
function css(s: string): React.CSSProperties {
  const o: Record<string, string> = {};
  for (const decl of s.split(";")) {
    const i = decl.indexOf(":");
    if (i === -1) continue;
    const prop = decl.slice(0, i).trim();
    if (!prop) continue;
    o[prop.replace(/-([a-z])/g, (_, c: string) => c.toUpperCase())] = decl.slice(i + 1).trim();
  }
  return o as React.CSSProperties;
}

const S = {
  root: css("min-height:100vh;display:flex;flex-direction:column;background:var(--bg);color:var(--text)"),
  header: css(
    "display:flex;align-items:center;justify-content:space-between;gap:2rem;padding:1.75rem 2.5rem;max-width:78rem;width:100%;margin:0 auto",
  ),
  logo: css("display:flex;align-items:center;gap:0.7rem;color:var(--text-strong);text-decoration:none"),
  nav: css("display:flex;align-items:center;gap:2rem;font-size:14px"),
  navLink: css("color:var(--text-weak);text-decoration:none"),
  navBtn: css(
    "display:inline-flex;align-items:center;gap:0.5rem;padding:0.5rem 0.95rem;border:1px solid var(--border);border-radius:6px;color:var(--text-strong);text-decoration:none",
  ),
  main: css(
    "flex:1;display:flex;flex-direction:column;align-items:center;text-align:center;padding:4.5rem 2.5rem 6rem;max-width:78rem;width:100%;margin:0 auto",
  ),
  heroWrap: css(
    "position:relative;display:flex;align-items:center;justify-content:center;animation:appa-rise 0.7s ease-out both",
  ),
  glow: css(
    "position:absolute;width:26rem;height:26rem;max-width:80vw;border-radius:50%;background:radial-gradient(circle, var(--accent-bg) 0%, transparent 68%);opacity:0.55",
  ),
  relative: css("position:relative"),
  h1: css(
    "margin:2.75rem 0 0;font-size:clamp(30px,4.6vw,52px);font-weight:600;line-height:1.12;letter-spacing:-0.035em;color:var(--text-strong);max-width:18ch;text-wrap:balance",
  ),
  sub: css("margin:1.4rem 0 0;font-size:17px;line-height:1.7;color:var(--text-weak);max-width:40rem"),
  ctaRow: css("display:flex;flex-wrap:wrap;gap:0.9rem;justify-content:center;margin-top:2.5rem"),
  whatKicker: css(
    "margin:6rem 0 0;font-size:12px;letter-spacing:0.12em;text-transform:uppercase;color:var(--icon)",
  ),
  kicker: css("margin:0;font-size:12px;letter-spacing:0.12em;text-transform:uppercase;color:var(--icon)"),
  question: css(
    "margin:1.25rem 0 0;font-size:clamp(19px,2.2vw,26px);line-height:1.5;color:var(--text-strong);max-width:30ch;font-weight:500;text-wrap:balance",
  ),
  howGrid: css(
    "display:grid;grid-template-columns:repeat(auto-fit,minmax(15rem,1fr));gap:1px;background:var(--border-weak);border:1px solid var(--border-weak);border-radius:10px;overflow:hidden;margin-top:5rem;width:100%;max-width:64rem;text-align:left",
  ),
  howCell: css("background:var(--bg);padding:2rem 1.75rem"),
  howNum: css("color:var(--accent);font-size:13px"),
  howH3: css(
    "margin:0.85rem 0 0.6rem;font-size:17px;font-weight:600;color:var(--text-strong);letter-spacing:-0.02em",
  ),
  howP: css("margin:0;font-size:14.5px;line-height:1.75;color:var(--text-weak)"),
  section: css("width:100%;max-width:64rem;margin-top:6rem;text-align:left"),
  demoCard: css(
    "margin-top:1.75rem;border:1px solid var(--border-weak);border-radius:10px;overflow:hidden;background:var(--bg-weak)",
  ),
  demoBar: css("display:flex;align-items:center;gap:0.5rem;padding:0.7rem 1rem;border-bottom:1px solid var(--border-weak)"),
  dotR: css("width:10px;height:10px;border-radius:50%;background:#ff5f57"),
  dotY: css("width:10px;height:10px;border-radius:50%;background:#febc2e"),
  dotG: css("width:10px;height:10px;border-radius:50%;background:#28c840"),
  demoTitle: css("margin-left:0.6rem;font-size:12.5px;color:var(--icon)"),
  replayBtn: css(
    "margin-left:auto;font:inherit;font-size:12px;padding:0.25rem 0.7rem;border-radius:5px;border:1px solid var(--border-weak);background:transparent;color:var(--text-weak);cursor:pointer",
  ),
  demoBody: css("padding:1.25rem 1.25rem 1.5rem;font-size:13px;line-height:1.9;overflow-x:auto"),
  demoLines: css("min-height:24rem"),
  lineCmd: css("white-space:pre;color:var(--text-strong)"),
  lineTool: css("white-space:pre;margin-top:0.5rem;color:var(--text)"),
  lineBad: css(
    "white-space:pre;margin-top:0.5rem;color:#e08a80;text-decoration:line-through;text-decoration-color:#8c2f2f",
  ),
  lineWarn: css("white-space:pre;color:#e08a80"),
  lineDim: css("white-space:pre;color:var(--text-weak)"),
  lineSay: css("display:flex;gap:0.6rem;margin-top:0.6rem;max-width:56rem;color:var(--text)"),
  sayDot: css("color:var(--accent)"),
  sayBody: css("line-height:1.7"),
  sayStrong: css(
    "color:var(--text-strong);font-weight:600;background:var(--accent-bg);box-decoration-break:clone;-webkit-box-decoration-break:clone;padding:0.05rem 0;border-radius:3px",
  ),
  hintBox: css(
    "margin-top:1.25rem;padding:0.55rem 0.85rem;border:1px solid #3c8f9c;border-radius:6px;color:var(--text-weak);font-size:13px;display:flex;align-items:center;gap:0.6rem",
  ),
  hintChevron: css("color:#5fb3c4"),
  hintCursor: css("width:8px;height:16px;background:var(--text-weak);opacity:0.7"),
  hintText: css("color:var(--icon)"),
  statusRow: css(
    "display:flex;align-items:stretch;width:max-content;margin-top:0.6rem;font-size:13px;line-height:1.7;white-space:pre",
  ),
  statusAppa: css(
    "display:flex;align-items:center;gap:0.5rem;padding:0.15rem 0.6rem;background:#14213d;color:#e8edf7;font-weight:600;letter-spacing:0.04em",
  ),
  statusAllowed: css("padding:0.15rem 0.7rem;background:#1f6b46;color:#eafff2;font-weight:600"),
  statusBlocked: css("padding:0.15rem 0.7rem;background:#8c2f2f;color:#ffecec;font-weight:600"),
  statusIdle: css("padding:0.15rem 0.7rem;background:#2b2f36;color:#b9c0cc;font-weight:600"),
  statusFlow: css("padding:0.15rem 0.8rem;color:var(--text-weak)"),
  statusFlowVal: css("color:var(--text-strong)"),
  statusTrust: css("padding:0.15rem 0.7rem;background:#1f6b46;color:#eafff2"),
  statusAudience: css("padding:0.15rem 0.7rem;background:#3f3168;color:#efe9ff"),
  autoLine: css("margin-top:0.35rem;font-size:13px;color:var(--text-weak);white-space:pre"),
  autoOn: css("color:#d8a13a"),
  autoAgents: css("color:var(--text-weak)"),
  autoShell: css("color:#5fb3c4"),
  demoNote: css(
    "margin:0;padding:1rem 1.25rem 1.25rem;font-size:13px;line-height:1.7;color:var(--text-weak);border-top:1px solid var(--border-weak)",
  ),
  examplesHead: css("display:flex;align-items:flex-end;justify-content:space-between;gap:1.5rem;flex-wrap:wrap"),
  h2: css(
    "margin:1rem 0 0.6rem;font-size:clamp(20px,2.2vw,27px);font-weight:600;letter-spacing:-0.025em;color:var(--text-strong)",
  ),
  lead: css("margin:0;font-size:15px;line-height:1.7;color:var(--text-weak);max-width:46rem"),
  leadStrong: css("color: var(--text-strong); font-weight: 600;"),
  dialectRow: css("display:flex;flex-wrap:wrap;gap:0.75rem;margin-top:2rem"),
  dialectOn: css(
    "flex:1 1 12rem;min-width:11rem;text-align:left;cursor:pointer;font:inherit;padding:1rem 1.15rem;border-radius:9px;background:var(--accent-bg);border:1px solid var(--accent)",
  ),
  dialectOff: css(
    "flex:1 1 12rem;min-width:11rem;text-align:left;cursor:pointer;font:inherit;padding:1rem 1.15rem;border-radius:9px;background:var(--bg-weak);border:1px solid var(--border-weak)",
  ),
  cardKickerOn: css("font-size:11px;letter-spacing:0.12em;text-transform:uppercase;color:var(--accent)"),
  cardKickerOff: css("font-size:11px;letter-spacing:0.12em;text-transform:uppercase;color:var(--icon)"),
  cardNameOn: css(
    "margin-top:0.45rem;font-size:14.5px;font-weight:600;letter-spacing:-0.01em;color:var(--text-strong)",
  ),
  cardNameOff: css("margin-top:0.45rem;font-size:14.5px;font-weight:600;letter-spacing:-0.01em;color:var(--text)"),
  cardBlurb: css("margin-top:0.4rem;font-size:13px;line-height:1.6;color:var(--text-weak)"),
  syntaxWrap: css("position:relative;margin-top:1rem"),
  syntaxLabel: css(
    "position:absolute;top:0.85rem;right:1rem;z-index:1;display:flex;align-items:center;gap:0.55rem;font-size:11px;letter-spacing:0.1em;text-transform:uppercase;color:var(--icon)",
  ),
  syntaxSelect: css(
    "font:inherit;font-size:13px;letter-spacing:0;text-transform:none;padding:0.4rem 0.65rem;border-radius:6px;border:1px solid var(--border);background:var(--bg);color:var(--text-strong);cursor:pointer",
  ),
  codePre: css(
    "margin:0;padding:3.4rem 0 1.25rem;border:1px solid var(--border-weak);background:var(--bg-weak);border-radius:10px;overflow-x:auto;font-size:13px;line-height:1.85;tab-size:2",
  ),
  codeHl: css(
    "display:block;padding:0 0.9rem;white-space:pre;background:var(--accent-bg);box-shadow:inset 2px 0 0 var(--accent);color:var(--text-strong)",
  ),
  codePlain: css("display:block;padding:0 0.9rem;white-space:pre;color:var(--text-weak)"),
  layerOn: css(
    "flex:1 1 14rem;min-width:13rem;text-align:left;cursor:pointer;font:inherit;padding:1.1rem 1.25rem;border-radius:9px;background:var(--accent-bg);border:1px solid var(--accent)",
  ),
  layerOff: css(
    "flex:1 1 14rem;min-width:13rem;text-align:left;cursor:pointer;font:inherit;padding:1.1rem 1.25rem;border-radius:9px;background:var(--bg-weak);border:1px solid var(--border-weak)",
  ),
  layerSurfacesOn: css("margin-top:0.7rem;font-size:12px;line-height:1.6;color:var(--text-strong)"),
  layerSurfacesOff: css("margin-top:0.7rem;font-size:12px;line-height:1.6;color:var(--icon)"),
  layerDetailBox: css(
    "margin-top:1rem;border:1px solid var(--border-weak);border-radius:10px;background:var(--bg-weak);overflow:hidden",
  ),
  layerPre: css("margin:0;padding:1.25rem 0;overflow-x:auto;font-size:13px;line-height:1.85"),
  layerCodeHl: css(
    "display:block;padding:0 1.15rem;white-space:pre;background:var(--accent-bg);box-shadow:inset 2px 0 0 var(--accent);color:var(--text-strong)",
  ),
  layerCodePlain: css("display:block;padding:0 1.15rem;white-space:pre;color:var(--text-weak)"),
  layerExplain: css(
    "margin:0;padding:1rem 1.25rem 1.25rem;font-size:13.5px;line-height:1.75;color:var(--text-weak);border-top:1px solid var(--border-weak)",
  ),
  benchLead: css("margin:0 0 1.75rem;font-size:15px;line-height:1.7;color:var(--text-weak);max-width:46rem"),
  benchCode: css(
    "font-size:13.5px;padding:0.1rem 0.35rem;border:1px solid var(--border-weak);border-radius:4px;color:var(--text-strong)",
  ),
  benchGrid: css(
    "display:grid;grid-template-columns:1fr auto auto auto auto;align-items:center;gap:0.6rem 1.1rem;font-size:13.5px;color:var(--text-weak)",
  ),
  benchColWithout: css(
    "grid-column:span 2;font-size:11px;letter-spacing:0.1em;text-transform:uppercase;color:var(--icon)",
  ),
  benchColWith: css(
    "grid-column:span 2;font-size:11px;letter-spacing:0.1em;text-transform:uppercase;color:var(--accent)",
  ),
  barOuter: css("display:block;width:52px;height:8px;border-radius:3px;background:var(--border-weak)"),
  barRed: css("display:block;height:8px;border-radius:3px;background:#c05c52"),
  barAccent: css("display:block;height:8px;border-radius:3px;background:var(--accent)"),
  benchNum: css("color:var(--text-strong);font-variant-numeric:tabular-nums"),
  dojoWrap: css("margin-top:1.75rem;padding-top:1.5rem;border-top:1px solid var(--border-weak)"),
  dojoLead: css("margin:0 0 1.1rem;font-size:13.5px;line-height:1.7;color:var(--text-weak);max-width:52rem"),
  benchFoot: css("margin:1.1rem 0 0;font-size:13px;line-height:1.7;color:var(--icon);max-width:52rem"),
  benchFootLink: css("color:var(--text-strong)"),
  chipsRow: css("display:flex;flex-wrap:wrap;gap:0.5rem"),
  chipOn: css(
    "font:inherit;font-size:13.5px;padding:0.45rem 0.9rem;border-radius:999px;cursor:pointer;background:var(--accent-bg);border:1px solid var(--accent);color:var(--text-strong)",
  ),
  chipOff: css(
    "font:inherit;font-size:13.5px;padding:0.45rem 0.9rem;border-radius:999px;cursor:pointer;background:var(--bg-weak);border:1px solid var(--border-weak);color:var(--text-weak)",
  ),
  cmdBox: css(
    "display:flex;align-items:center;gap:1rem;margin-top:1.25rem;padding:0.85rem 0.85rem 0.85rem 1.15rem;border:1px solid var(--border-weak);background:var(--bg-weak);border-radius:8px;font-size:14px;color:var(--text)",
  ),
  cmdDollar: css("color:var(--accent)"),
  cmdText: css("flex:1;overflow-x:auto;white-space:pre"),
  copyBtn: css(
    "padding:0.4rem 0.8rem;border:1px solid var(--border);background:transparent;color:var(--text-weak);border-radius:5px;font:inherit;font-size:13px;cursor:pointer",
  ),
  footer: css("border-top:1px solid var(--border-weak)"),
  footerInner: css(
    "display:flex;flex-wrap:wrap;align-items:center;justify-content:space-between;gap:1rem;padding:1.75rem 2.5rem;max-width:78rem;margin:0 auto;font-size:13.5px;color:var(--text-weak)",
  ),
  footerLink: css("color:var(--text-weak);text-decoration:none"),
};

type BenchEntry = {
  model: string;
  withoutPct: string;
  without: string;
  withPct: string;
  withOpenappa: string;
};

const BENCH_CORP: BenchEntry[] = [
  { model: "Gemini 3.5 Flash-Lite", withoutPct: "31%", without: "13/42", withPct: "0%", withOpenappa: "0/42" },
  { model: "GPT-5.6 Luna", withoutPct: "36%", without: "15/42", withPct: "3%", withOpenappa: "1/42" },
  { model: "GPT-4o", withoutPct: "50%", without: "21/42", withPct: "8%", withOpenappa: "3/42" },
  { model: "Qwen 3.6 35B", withoutPct: "43%", without: "18/42", withPct: "0%", withOpenappa: "0/42" },
];

const BENCH_DOJO: BenchEntry[] = [
  { model: "Gemini 3.5 Flash-Lite", withoutPct: "30%", without: "24/79", withPct: "3%", withOpenappa: "2/79" },
  { model: "GPT-5.6 Luna", withoutPct: "39%", without: "31/79", withPct: "1%", withOpenappa: "1/79" },
  { model: "GPT-4o", withoutPct: "48%", without: "38/79", withPct: "5%", withOpenappa: "4/79" },
  { model: "Qwen 3.6 35B", withoutPct: "34%", without: "27/79", withPct: "0%", withOpenappa: "0/79" },
];

function BenchGrid({ entries }: { entries: BenchEntry[] }) {
  return (
    <div style={S.benchGrid}>
      <span></span>
      <span style={S.benchColWithout}>Without</span>
      <span style={S.benchColWith}>With OpenAPPA</span>
      {entries.map((e) => (
        <React.Fragment key={e.model}>
          <span>{e.model}</span>
          <span style={S.barOuter}>
            <span style={{ ...S.barRed, width: e.withoutPct }}></span>
          </span>
          <span style={S.benchNum}>{e.without}</span>
          <span style={S.barOuter}>
            <span style={{ ...S.barAccent, width: e.withPct }}></span>
          </span>
          <span style={S.benchNum}>{e.withOpenappa}</span>
        </React.Fragment>
      ))}
    </div>
  );
}

const MASCOT_SIZE = 220;
const SHOW_INSTALL = true;

export function Landing() {
  const [source, setSource] = useState(SOURCES[0].id);
  const [syntax, setSyntax] = useState(SYNTAXES[0].id);
  const [installer, setInstaller] = useState("brew");
  const [copied, setCopied] = useState(false);
  const [layer, setLayer] = useState<string | null>(null);
  const [step, setStep] = useState(0);

  const runTimer = useRef<ReturnType<typeof setInterval> | null>(null);
  const copyTimer = useRef<ReturnType<typeof setTimeout> | null>(null);

  const scenario = SCENARIOS[0];

  const run = useCallback(() => {
    if (runTimer.current) clearInterval(runTimer.current);
    setStep(0);
    runTimer.current = setInterval(() => {
      setStep((st) => {
        if (st >= scenario.lines.length) {
          if (runTimer.current) clearInterval(runTimer.current);
          return st;
        }
        return st + 1;
      });
    }, 900);
  }, [scenario]);

  useEffect(() => {
    registerPixelMarks();
    run();
    return () => {
      if (runTimer.current) clearInterval(runTimer.current);
      if (copyTimer.current) clearTimeout(copyTimer.current);
    };
  }, [run]);

  const shown = scenario.lines.slice(0, step);
  let demoState: DemoState = IDLE;
  for (const l of shown) if (l[2]) demoState = l[2];
  const verdictLabel =
    demoState.v === "allowed" ? "✓ allowed" : demoState.v === "blocked" ? "✗ blocked" : "· watching";

  const sel = SOURCES.find((d) => d.id === source) ?? SOURCES[0];
  const codeLines = lines(sel.code[syntax] ?? sel.code.toml);

  const layerSel = LAYERS.find((x) => x.id === layer) ?? null;

  const inst = INSTALLERS.find((p) => p.id === installer) ?? INSTALLERS[0];

  const copyCmd = () => {
    if (navigator.clipboard) navigator.clipboard.writeText(inst.cmd);
    setCopied(true);
    if (copyTimer.current) clearTimeout(copyTimer.current);
    copyTimer.current = setTimeout(() => setCopied(false), 1600);
  };

  return (
    <div className="appa-landing" style={S.root}>
      <header style={S.header}>
        <a href="#" style={S.logo}>
          <appa-mark size={26}></appa-mark>
          <appa-word word="OpenAPPA" cap={15}></appa-word>
        </a>
        <nav style={S.nav}>
          <a href="#what" style={S.navLink}>
            Spec
          </a>
          <a href="#how" style={S.navLink}>
            Algebra
          </a>
          <a href="#" style={S.navLink}>
            Docs
          </a>
          <a href="#" style={S.navBtn}>
            GitHub
          </a>
        </nav>
      </header>
      <main style={S.main}>
        <div style={S.heroWrap}>
          <div style={S.glow}></div>
          <appa-mark size={MASCOT_SIZE} style={S.relative}></appa-mark>
        </div>
        <h1 style={S.h1}>Agentic permissions, 100% algebraically</h1>
        <p style={S.sub}>
          An open specification for deterministic guardrails. One decision, made before every flow an agent
          proposes.
        </p>
        <div style={S.ctaRow}></div>
        <p id="what" style={S.whatKicker}>
          The one question
        </p>
        <p style={S.question}>
          How to make sure an agent doesn&apos;t post your sensitive data online or drop your database.
        </p>
        <div id="how" style={S.howGrid}>
          <div style={S.howCell}>
            <div style={S.howNum}>01</div>
            <h3 style={S.howH3}>Sensitivity only escalates, never widens the audience</h3>
            <p style={S.howP}>
              Every source the agent touches raises the label on what it carries. Labels fold, never widen.
            </p>
          </div>
          <div style={S.howCell}>
            <div style={S.howNum}>02</div>
            <h3 style={S.howH3}>Contracts gate the call</h3>
            <p style={S.howP}>
              Declared requirements decide what may be dispatched. The check runs before the call, not after.
              In 5{" "}ms
              <br />
            </p>
          </div>
          <div style={S.howCell}>
            <div style={S.howNum}>03</div>
            <h3 style={S.howH3}>Same trajectory, same verdict</h3>
            <p style={S.howP}>Run 1 and run 40 agree. A ruling admits a dispatch and never edits the trajectory.</p>
          </div>
        </div>
        <div id="demo" style={S.section}>
          <p style={S.kicker}>In action</p>
          <div style={S.demoCard}>
            <div style={S.demoBar}>
              <span style={S.dotR}></span>
              <span style={S.dotY}></span>
              <span style={S.dotG}></span>
              <span style={S.demoTitle}>claude — openappa</span>
              <button type="button" onClick={run} className="lp-replay" style={S.replayBtn}>
                Replay
              </button>
            </div>
            <div style={S.demoBody}>
              <div style={S.demoLines}>
                {shown.map((l, i) => {
                  const key = scenario.id + i;
                  const [text, kind, meta] = l;
                  if (kind === "cmd")
                    return (
                      <div key={key} style={S.lineCmd}>
                        {text}
                      </div>
                    );
                  if (kind === "tool")
                    return (
                      <div key={key} style={meta && meta.v === "blocked" ? S.lineBad : S.lineTool}>
                        {text}
                      </div>
                    );
                  if (kind === "warn")
                    return (
                      <div key={key} style={S.lineWarn}>
                        {text}
                      </div>
                    );
                  if (kind === "dim")
                    return (
                      <div key={key} style={S.lineDim}>
                        {text}
                      </div>
                    );
                  return (
                    <div key={key} style={S.lineSay}>
                      <span style={S.sayDot}>⏺</span>
                      <span style={S.sayBody}>
                        {text.split("**").map((t, j) =>
                          j % 2 === 1 ? (
                            <span key={key + "-" + j} style={S.sayStrong}>
                              {t}
                            </span>
                          ) : (
                            <span key={key + "-" + j}>{t}</span>
                          ),
                        )}
                      </span>
                    </div>
                  );
                })}
              </div>
              <div style={S.hintBox}>
                <span style={S.hintChevron}>❯</span>
                <span style={S.hintCursor}></span>
                <span style={S.hintText}>Try &quot;run the demo again&quot;</span>
              </div>
              <div style={S.statusRow}>
                <span style={S.statusAppa}>
                  <appa-mark size={13}></appa-mark>APPA
                </span>
                <span
                  style={
                    demoState.v === "allowed"
                      ? S.statusAllowed
                      : demoState.v === "blocked"
                        ? S.statusBlocked
                        : S.statusIdle
                  }
                >
                  {verdictLabel}
                </span>
                <span style={S.statusFlow}>
                  flow: <span style={S.statusFlowVal}>{demoState.flow}</span>
                </span>
                <span style={S.statusTrust}>{demoState.trust}</span>
                <span style={S.statusAudience}>{demoState.audience}</span>
              </div>
              <div style={S.autoLine}>
                <span style={S.autoOn}>▶▶ auto mode on</span> · <span style={S.autoAgents}>← 2 agents</span> ·{" "}
                <span style={S.autoShell}>1 shell</span>
              </div>
            </div>
            <p style={S.demoNote}>{scenario.note}</p>
          </div>
        </div>
        <div id="examples" style={S.section}>
          <div style={S.examplesHead}>
            <p style={S.kicker}>Examples</p>
          </div>
          <h2 style={S.h2}>One policy, assembled from every source</h2>
          <p style={S.lead}>
            A contract states the <strong style={S.leadStrong}>trust</strong> and{" "}
            <strong style={S.leadStrong}>audience</strong> a call requires. Three ways it reaches APPA.
          </p>
          <div style={S.dialectRow}>
            {SOURCES.map((d) => {
              const on = d.id === source;
              return (
                <button
                  key={d.id}
                  type="button"
                  onClick={() => setSource(d.id)}
                  className={on ? undefined : "lp-card"}
                  style={on ? S.dialectOn : S.dialectOff}
                >
                  <div style={on ? S.cardKickerOn : S.cardKickerOff}>{d.kicker}</div>
                  <div style={on ? S.cardNameOn : S.cardNameOff}>{d.name}</div>
                  <div style={S.cardBlurb}>{d.blurb}</div>
                </button>
              );
            })}
          </div>
          <div style={S.syntaxWrap}>
            <label style={S.syntaxLabel}>
              Syntax
              <select value={syntax} onChange={(e) => setSyntax(e.target.value)} style={S.syntaxSelect}>
                {SYNTAXES.map((s) => (
                  <option key={s.id} value={s.id}>
                    {s.label}
                  </option>
                ))}
              </select>
            </label>
            <pre style={S.codePre}>
              {codeLines.map((l, i) => (
                <code key={sel.id + syntax + i} style={l.hl ? S.codeHl : S.codePlain}>
                  {l.text === "" ? " " : l.text}
                </code>
              ))}
            </pre>
          </div>
        </div>
        <div id="layers" style={S.section}>
          <p style={S.kicker}>Where it runs</p>
          <h2 style={S.h2}>Three layers, one mediator</h2>
          <p style={S.lead}>Pick the layer you can reach. Same policy, same ruling.</p>
          <div style={S.dialectRow}>
            {LAYERS.map((l) => {
              const on = l.id === layer;
              return (
                <button
                  key={l.id}
                  type="button"
                  onClick={() => setLayer(layer === l.id ? null : l.id)}
                  className={on ? undefined : "lp-card"}
                  style={on ? S.layerOn : S.layerOff}
                >
                  <div style={on ? S.cardKickerOn : S.cardKickerOff}>{l.kicker}</div>
                  <div style={on ? S.cardNameOn : S.cardNameOff}>{l.name}</div>
                  <div style={S.cardBlurb}>{l.blurb}</div>
                  <div style={on ? S.layerSurfacesOn : S.layerSurfacesOff}>{l.surfaces}</div>
                </button>
              );
            })}
          </div>
          {layerSel && (
            <div style={S.layerDetailBox}>
              <pre style={S.layerPre}>
                {lines(layerSel.code).map((c, i) => (
                  <code key={layerSel.id + i} style={c.hl ? S.layerCodeHl : S.layerCodePlain}>
                    {c.text === "" ? " " : c.text}
                  </code>
                ))}
              </pre>
              <p style={S.layerExplain}>{layerSel.explain}</p>
            </div>
          )}
        </div>
        <div id="benchmarks" style={S.section}>
          <p style={S.kicker}>Benchmarks</p>
          <h2 style={S.h2}>The same agent, the same attacks</h2>
          <p style={S.benchLead}>
            <code style={S.benchCode}>bench-corp</code> runs a seventeen-tool corporate agent through 42
            attacked episodes, twice per model. Attacks that succeeded:
          </p>
          <BenchGrid entries={BENCH_CORP} />
          <div style={S.dojoWrap}>
            <p style={S.dojoLead}>
              <span style={S.leadStrong}>AgentDojo</span> — popular public benchmark, 79 attacked episodes.
              Illustrative, not pinned yet:
            </p>
            <BenchGrid entries={BENCH_DOJO} />
          </div>
          <p style={S.benchFoot}>
            Both benches score what the tools actually did — files written, emails sent — never the
            conversation text.{" "}
            <a href="#" style={S.benchFootLink}>
              Method and pinned results →
            </a>
          </p>
        </div>
        {SHOW_INSTALL && (
          <div id="install" style={S.section}>
            <p style={S.kicker}>Installation</p>
            <div style={S.chipsRow}>
              {INSTALLERS.map((p) => {
                const on = p.id === installer;
                return (
                  <button
                    key={p.id}
                    type="button"
                    onClick={() => {
                      setInstaller(p.id);
                      setCopied(false);
                    }}
                    className={on ? undefined : "lp-chip"}
                    style={on ? S.chipOn : S.chipOff}
                  >
                    {p.label}
                  </button>
                );
              })}
            </div>
            <div style={S.cmdBox}>
              <span style={S.cmdDollar}>$</span>
              <span style={S.cmdText}>{inst.cmd}</span>
              <button type="button" onClick={copyCmd} className="lp-copy" style={S.copyBtn}>
                {copied ? "Copied" : "Copy"}
              </button>
            </div>
          </div>
        )}
      </main>
      <footer style={S.footer}>
        <div style={S.footerInner}>
          <span>© 2026 OpenAPPA — Agentic Permissions Policy Algebra</span>
          <a href="#" style={S.footerLink}>
            GitHub
          </a>
        </div>
      </footer>
    </div>
  );
}

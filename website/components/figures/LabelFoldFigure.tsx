"use client";

import { Figure } from "@/components/figures/Figure";
import { chip, drawCard, ease, lerp, seg, type Theme } from "@/components/figures/lib";

/* Tutorial figure: labels only move one way. Three reads fold into the run's
   label — a neutral read leaves it alone, the CRM narrows the audience to
   private, a fetched page drops the trust — and the way back up does not
   exist. */

const W = 900;
const H = 340;

const START_X = 90;
const STATIONS = [320, 550, 780]; // where each read folds in
const LEVELS = [186, 232, 278]; // label level: start, after the CRM, after the page
const CHIP_END_X = 800;
const TAIL_END_X = 850;

const CARD = { w: 168, h: 52, y: 40 };

const READS = [
  { source: "public docs", colorKey: "accent" as const, tag: "delta = {}", pop: 0.04 },
  { source: "private CRM", colorKey: "warn" as const, tag: "audience ↓", pop: 0.3 },
  { source: "fetched webpage", colorKey: "danger" as const, tag: "trust ↓", pop: 0.56 },
];

const LABELS = ["public · trusted", "private · trusted", "private · suspicious"];
const TAG_AT = [0.23, 0.48, 0.72];

function chipStyle(th: Theme, idx: number): { color: string; bg: string } {
  switch (idx) {
    case 0:
      return { color: th.accent, bg: th.accentBg };
    case 1:
      return { color: th.warn, bg: th.warnBg };
    default:
      return { color: th.danger, bg: th.dangerBg };
  }
}

function draw(ctx: CanvasRenderingContext2D, t: number, th: Theme) {
  const font = (size: number, weight = 400) => `${weight} ${size}px ${th.mono}`;

  /* the staircase the label walks down, visible from the start */
  ctx.strokeStyle = th.borderWeak;
  ctx.lineWidth = 1;
  ctx.beginPath();
  ctx.moveTo(START_X, LEVELS[0]);
  ctx.lineTo(STATIONS[1], LEVELS[0]);
  ctx.lineTo(STATIONS[1], LEVELS[1]);
  ctx.lineTo(STATIONS[2], LEVELS[1]);
  ctx.lineTo(STATIONS[2], LEVELS[2]);
  ctx.lineTo(TAIL_END_X, LEVELS[2]);
  ctx.stroke();

  /* reach shrinks downward */
  ctx.beginPath();
  ctx.moveTo(48, LEVELS[0]);
  ctx.lineTo(48, LEVELS[2]);
  ctx.moveTo(44, LEVELS[2] - 7);
  ctx.lineTo(48, LEVELS[2]);
  ctx.lineTo(52, LEVELS[2] - 7);
  ctx.stroke();
  ctx.save();
  ctx.translate(34, (LEVELS[0] + LEVELS[2]) / 2);
  ctx.rotate(-Math.PI / 2);
  ctx.fillStyle = th.textWeak;
  ctx.font = font(11);
  ctx.textAlign = "center";
  ctx.textBaseline = "middle";
  ctx.fillText("reach", 0, 0);
  ctx.restore();

  /* source cards popping above their fold stations */
  READS.forEach((read, i) => {
    const pop = ease(seg(t, read.pop, read.pop + 0.05));
    if (pop <= 0) return;
    const foldY = LEVELS[Math.max(0, i - 1)];
    ctx.save();
    ctx.globalAlpha = pop;
    drawCard(ctx, th, STATIONS[i] - CARD.w / 2, CARD.y, CARD.w, CARD.h, read.source, th[read.colorKey]);
    ctx.strokeStyle = th.borderWeak;
    ctx.beginPath();
    ctx.moveTo(STATIONS[i], CARD.y + CARD.h + 6);
    ctx.lineTo(STATIONS[i], foldY - 14);
    ctx.moveTo(STATIONS[i] - 4, foldY - 21);
    ctx.lineTo(STATIONS[i], foldY - 14);
    ctx.lineTo(STATIONS[i] + 4, foldY - 21);
    ctx.stroke();
    ctx.restore();
  });

  /* what each fold did to the label */
  READS.forEach((read, i) => {
    const show = ease(seg(t, TAG_AT[i], TAG_AT[i] + 0.05));
    if (show <= 0) return;
    ctx.save();
    ctx.globalAlpha = show;
    ctx.fillStyle = th.textWeak;
    ctx.font = font(11);
    ctx.textBaseline = "middle";
    if (i === 0) {
      ctx.textAlign = "center";
      ctx.fillText(read.tag, STATIONS[0], LEVELS[0] + 24);
    } else {
      ctx.textAlign = "left";
      ctx.fillText(read.tag, STATIONS[i] + 14, (LEVELS[i - 1] + LEVELS[i]) / 2);
    }
    ctx.restore();
  });

  /* the label chip riding the fold */
  const s0 = ease(seg(t, 0.1, 0.22));
  const s1 = ease(seg(t, 0.34, 0.46));
  const d1 = ease(seg(t, 0.46, 0.52));
  const s2 = ease(seg(t, 0.58, 0.7));
  const d2 = ease(seg(t, 0.7, 0.76));
  const tail = ease(seg(t, 0.76, 0.8));

  let x = lerp(START_X, STATIONS[0], s0);
  if (s1 > 0) x = lerp(STATIONS[0], STATIONS[1], s1);
  if (s2 > 0) x = lerp(STATIONS[1], STATIONS[2], s2);
  if (tail > 0) x = lerp(STATIONS[2], CHIP_END_X, tail);
  let y = LEVELS[0];
  if (d1 > 0) y = lerp(LEVELS[0], LEVELS[1], d1);
  if (d2 > 0) y = lerp(LEVELS[1], LEVELS[2], d2);
  const idx = d2 >= 0.5 ? 2 : d1 >= 0.5 ? 1 : 0;
  const style = chipStyle(th, idx);

  /* a fold lands with a ring */
  [seg(t, 0.52, 0.6), seg(t, 0.76, 0.84)].forEach((ring) => {
    if (ring <= 0 || ring >= 1) return;
    ctx.save();
    ctx.globalAlpha = 1 - ring;
    ctx.strokeStyle = style.color;
    ctx.beginPath();
    ctx.arc(x, y, 14 + 22 * ring, 0, Math.PI * 2);
    ctx.stroke();
    ctx.restore();
  });

  /* the way back up, bounced */
  const rise = ease(seg(t, 0.86, 0.91));
  const fall = ease(seg(t, 0.91, 0.96));
  if (rise > 0) {
    const ax = 845;
    ctx.save();
    ctx.setLineDash([3, 5]);
    ctx.strokeStyle = th.danger;
    ctx.globalAlpha = 0.7;
    ctx.beginPath();
    ctx.moveTo(ax, LEVELS[2] - 14);
    ctx.lineTo(ax, lerp(LEVELS[2] - 14, LEVELS[0] + 10, rise));
    ctx.stroke();
    ctx.restore();

    const ghostY = fall > 0 ? lerp(LEVELS[1], LEVELS[2], fall) : lerp(LEVELS[2], LEVELS[1], rise);
    ctx.save();
    ctx.globalAlpha = 0.35 * (1 - 0.7 * fall);
    chip(ctx, CHIP_END_X, ghostY, LABELS[0], th.accent, th.accentBg, th.mono);
    ctx.restore();
  }
  if (t > 0.93) {
    const pop = ease(seg(t, 0.93, 0.97));
    ctx.save();
    ctx.globalAlpha = pop;
    ctx.fillStyle = th.dangerBg;
    ctx.strokeStyle = th.danger;
    ctx.beginPath();
    ctx.arc(845, LEVELS[0] + 2, 9, 0, Math.PI * 2);
    ctx.fill();
    ctx.stroke();
    ctx.fillStyle = th.danger;
    ctx.font = font(12, 600);
    ctx.textAlign = "center";
    ctx.textBaseline = "middle";
    ctx.fillText("✕", 845, LEVELS[0] + 2.5);
    chip(ctx, 720, 316, "no step moves a label up", th.danger, th.dangerBg, th.mono);
    ctx.restore();
  }

  chip(ctx, x, y, LABELS[idx], style.color, style.bg, th.mono);
}

export function LabelFoldFigure() {
  return <Figure draw={draw} designW={W} designH={H} durationMs={12000} />;
}

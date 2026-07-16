/* Shared helpers for the tutorial's canvas figures. */

export interface Theme {
  bg: string;
  bgWeak: string;
  text: string;
  textWeak: string;
  textStrong: string;
  border: string;
  borderWeak: string;
  accent: string;
  accentBg: string;
  accentBorder: string;
  warn: string;
  warnBg: string;
  danger: string;
  dangerBg: string;
  info: string;
  infoBg: string;
  mono: string;
}

export function readTheme(el: HTMLElement): Theme {
  const s = getComputedStyle(el);
  const v = (name: string) => s.getPropertyValue(name).trim();
  return {
    bg: v("--bg"),
    bgWeak: v("--bg-weak"),
    text: v("--text"),
    textWeak: v("--text-weak"),
    textStrong: v("--text-strong"),
    border: v("--border"),
    borderWeak: v("--border-weak"),
    accent: v("--accent"),
    accentBg: v("--accent-bg"),
    accentBorder: v("--accent-border"),
    warn: v("--warn"),
    warnBg: v("--warn-bg"),
    danger: v("--danger"),
    dangerBg: v("--danger-bg"),
    info: v("--info"),
    infoBg: v("--info-bg"),
    mono: s.fontFamily || "monospace",
  };
}

export function clamp01(v: number) {
  return Math.min(1, Math.max(0, v));
}

/** Progress of t through [t0, t1], clamped to [0, 1]. */
export function seg(t: number, t0: number, t1: number) {
  return clamp01((t - t0) / (t1 - t0));
}

export function ease(v: number) {
  return v < 0.5 ? 4 * v * v * v : 1 - Math.pow(-2 * v + 2, 3) / 2;
}

export function lerp(a: number, b: number, f: number) {
  return a + (b - a) * f;
}

export function roundRect(
  ctx: CanvasRenderingContext2D,
  x: number,
  y: number,
  w: number,
  h: number,
  r: number,
) {
  ctx.beginPath();
  ctx.moveTo(x + r, y);
  ctx.arcTo(x + w, y, x + w, y + h, r);
  ctx.arcTo(x + w, y + h, x, y + h, r);
  ctx.arcTo(x, y + h, x, y, r);
  ctx.arcTo(x, y, x + w, y, r);
  ctx.closePath();
}

/** A small record card: colored provenance stripe, label, faint summary bar.
    Typography scales with the card height and content is clipped, so a card
    shrinking in flight never overflows. */
export function drawCard(
  ctx: CanvasRenderingContext2D,
  th: Theme,
  x: number,
  y: number,
  w: number,
  h: number,
  label: string,
  color: string,
  alpha = 1,
) {
  const k = Math.min(1, h / 40);
  ctx.save();
  ctx.globalAlpha = alpha;
  ctx.fillStyle = th.bgWeak;
  ctx.strokeStyle = th.border;
  roundRect(ctx, x, y, w, h, 3);
  ctx.fill();
  ctx.stroke();
  ctx.clip();
  ctx.fillStyle = color;
  ctx.fillRect(x + 1, y + 1, 3, h - 2);
  ctx.fillStyle = th.textStrong;
  ctx.font = `500 ${13 * k}px ${th.mono}`;
  ctx.textAlign = "left";
  ctx.textBaseline = "middle";
  ctx.fillText(label, x + 12 * k, y + h * 0.33);
  ctx.fillStyle = th.textWeak;
  ctx.fillRect(x + 12 * k, y + h - 16 * k, w - 34 * k, 4 * k);
  ctx.restore();
}

/** An envelope with provenance dots, centered on (cx, cy). */
export function drawEnvelope(
  ctx: CanvasRenderingContext2D,
  th: Theme,
  cx: number,
  cy: number,
  dotColors: string[],
  scaleF = 1,
  alpha = 1,
) {
  const w = 42 * scaleF;
  const h = 28 * scaleF;
  ctx.save();
  ctx.globalAlpha = alpha;
  ctx.fillStyle = th.bg;
  ctx.strokeStyle = th.textStrong;
  roundRect(ctx, cx - w / 2, cy - h / 2, w, h, 3);
  ctx.fill();
  ctx.stroke();
  ctx.beginPath();
  ctx.moveTo(cx - w / 2, cy - h / 2);
  ctx.lineTo(cx, cy + h * 0.12);
  ctx.lineTo(cx + w / 2, cy - h / 2);
  ctx.stroke();
  dotColors.forEach((color, i) => {
    ctx.fillStyle = color;
    ctx.beginPath();
    ctx.arc(cx + (i - (dotColors.length - 1) / 2) * 9 * scaleF, cy + h * 0.28, 2.4 * scaleF, 0, Math.PI * 2);
    ctx.fill();
  });
  ctx.restore();
}

/** A rounded pill with a mono label, centered on (x, y). */
export function chip(
  ctx: CanvasRenderingContext2D,
  x: number,
  y: number,
  label: string,
  color: string,
  bg: string,
  mono: string,
) {
  ctx.font = `500 13px ${mono}`;
  const w = ctx.measureText(label).width + 18;
  ctx.fillStyle = bg;
  ctx.strokeStyle = color;
  roundRect(ctx, x - w / 2, y - 10, w, 20, 10);
  ctx.fill();
  ctx.stroke();
  ctx.fillStyle = color;
  ctx.textAlign = "center";
  ctx.textBaseline = "middle";
  ctx.fillText(label, x, y + 0.5);
}

"use client";

import { Figure } from "@/components/figures/Figure";
import { drawCard, drawEnvelope, ease, lerp, roundRect, seg, type Theme } from "@/components/figures/lib";
import { logoPixelData } from "@/components/Logo";

/* Tutorial figure 4: OpenAPPA sits around the agent as a dotted boundary and
   keeps the books. Reading Acme's data is ok. Reading Beta's data is still
   ok. Talking to Acme with Beta's data in the trajectory is not — the email
   is stopped at the boundary, on the way out. */

const W = 900;
const H = 460;

const LOGO = logoPixelData();

const SOURCES = [
  { title: "Jira", item: "PROJ-101 · Acme", colorKey: "accent" as const },
  { title: "Salesforce", item: "Opp: Acme renewal", colorKey: "info" as const },
  { title: "GitHub", item: "PR #482 · approved", colorKey: "textStrong" as const },
  { title: "Granola", item: "Call · Acme Corp", colorKey: "warn" as const },
];

const sourceBox = (i: number) => ({ x: 40, y: 28 + i * 96, w: 185, h: 86 });
const itemHome = (i: number) => ({ x: 56, y: sourceBox(i).y + 36, w: 153, h: 38 });

const RAIL_Y = 215;
const BOUNDARY = { x: 355, y: 140, w: 270, h: 280 };
const AGENT = { x: 405, y: 190, w: 130, h: 50 };
const AGENT_C: [number, number] = [AGENT.x + AGENT.w / 2, RAIL_Y];
const CLIENT = { x: 640, y: 90, w: 240, h: 250 };

const cardStack = (i: number) => ({ x: 402 + i * 10, y: 255 + i * 6, rot: (i - 2) * 0.04 });

const BETA_PULL = { label: "Call · Beta Ltd", cardT: 0.4 };

const ENVELOPE_DOTS = ["accent", "info", "textStrong", "warn", "danger"] as const;

/* the guardrail's bookkeeping, one line per flow */
const NOTES: { at: number; text: string; color: "accent" | "warn" | "danger" }[] = [
  { at: 0.26, text: "✓ got Acme Corp's data — ok", color: "accent" },
  { at: 0.52, text: "✓ got Beta Ltd's data — still ok", color: "warn" },
  { at: 0.78, text: "✕ trying to talk to Acme — nope!", color: "danger" },
];

function sourceColor(th: Theme, i: number) {
  return th[SOURCES[i].colorKey];
}

/** The pixel wordmark, drawn straight onto the canvas. */
function drawLogo(ctx: CanvasRenderingContext2D, th: Theme, x: number, y: number, capPx: number) {
  const s = capPx / LOGO.capHeight;
  const size = LOGO.cell * s + 0.4; // slight overlap so adjacent cells don't seam
  for (const p of LOGO.pixels) {
    ctx.fillStyle = p.dim ? th.textWeak : th.textStrong;
    ctx.fillRect(x + p.x * LOGO.cell * s, y + p.y * LOGO.cell * s, size, size);
  }
}

function draw(ctx: CanvasRenderingContext2D, t: number, th: Theme) {
  const font = (size: number, weight = 400) => `${weight} ${size}px ${th.mono}`;
  const granolaBusy = t > 0.36 && t < 0.52;
  const blocked = t > 0.76;

  /* rails: sources → agent and agent → client, straight through the boundary */
  ctx.strokeStyle = th.borderWeak;
  ctx.lineWidth = 1;
  SOURCES.forEach((_, i) => {
    const box = sourceBox(i);
    ctx.beginPath();
    ctx.moveTo(box.x + box.w, box.y + box.h / 2);
    ctx.lineTo(AGENT.x, RAIL_Y);
    ctx.stroke();
  });
  ctx.beginPath();
  ctx.moveTo(AGENT.x + AGENT.w, RAIL_Y);
  ctx.lineTo(CLIENT.x, RAIL_Y);
  ctx.stroke();

  /* source panels; Granola glows while the Beta call is pulled */
  SOURCES.forEach((source, i) => {
    const box = sourceBox(i);
    ctx.save();
    if (i === 3 && granolaBusy) {
      ctx.shadowColor = th.danger;
      ctx.shadowBlur = 10 + 5 * Math.sin(t * 90);
      ctx.strokeStyle = th.danger;
    } else {
      ctx.strokeStyle = th.border;
    }
    ctx.fillStyle = th.bg;
    roundRect(ctx, box.x, box.y, box.w, box.h, 5);
    ctx.fill();
    ctx.stroke();
    ctx.restore();
    ctx.fillStyle = th.textStrong;
    ctx.font = font(12, 500);
    ctx.textAlign = "left";
    ctx.textBaseline = "middle";
    ctx.fillText(source.title, box.x + 14, box.y + 19);
    const flyStart = 0.04 + i * 0.035;
    if (t < flyStart) {
      const home = itemHome(i);
      drawCard(ctx, th, home.x, home.y, home.w, home.h, source.item, sourceColor(th, i));
    }
  });

  /* Client panel, centered on the rail */
  ctx.fillStyle = th.bg;
  ctx.strokeStyle = th.border;
  roundRect(ctx, CLIENT.x, CLIENT.y, CLIENT.w, CLIENT.h, 5);
  ctx.fill();
  ctx.stroke();
  ctx.fillStyle = th.textStrong;
  ctx.font = font(13, 500);
  ctx.fillText("Acme Corp", CLIENT.x + 16, CLIENT.y + 20);

  /* the OpenAPPA boundary — a dotted guardrail frame around the agent */
  ctx.save();
  ctx.strokeStyle = blocked ? th.danger : th.border;
  ctx.setLineDash([3, 5]);
  if (blocked) {
    ctx.shadowColor = th.danger;
    ctx.shadowBlur = 8;
  }
  roundRect(ctx, BOUNDARY.x, BOUNDARY.y, BOUNDARY.w, BOUNDARY.h, 6);
  ctx.stroke();
  ctx.restore();
  drawLogo(ctx, th, BOUNDARY.x + 12, BOUNDARY.y + 10, 10);

  /* Agent inside, on the rail */
  ctx.fillStyle = th.bgWeak;
  ctx.strokeStyle = th.border;
  roundRect(ctx, AGENT.x, AGENT.y, AGENT.w, AGENT.h, 4);
  ctx.fill();
  ctx.stroke();
  ctx.fillStyle = th.textStrong;
  ctx.font = font(13, 500);
  ctx.textAlign = "center";
  ctx.fillText("Agent", AGENT_C[0], AGENT_C[1]);

  /* the ??? moment before the Beta pull */
  const qIn = ease(seg(t, 0.32, 0.36));
  const qOut = ease(seg(t, 0.48, 0.52));
  if (qIn > 0 && qOut < 1) {
    ctx.save();
    ctx.globalAlpha = qIn * (1 - qOut);
    ctx.fillStyle = th.warn;
    ctx.font = font(16, 600);
    ctx.fillText("???", AGENT_C[0], AGENT.y - 12);
    ctx.restore();
  }

  /* Acme's items fly in */
  SOURCES.forEach((source, i) => {
    const t0 = 0.04 + i * 0.035;
    const t1 = t0 + 0.08;
    const home = itemHome(i);
    const stack = cardStack(i);
    const color = sourceColor(th, i);
    if (t >= t0 && t < t1) {
      const f = ease(seg(t, t0, t1));
      const x = lerp(home.x, stack.x, f);
      const y = lerp(home.y, stack.y, f) - Math.sin(f * Math.PI) * 30;
      drawCard(ctx, th, x, y, home.w * (1 - 0.25 * f), home.h * (1 - 0.15 * f), source.item, color);
    } else if (t >= t1) {
      ctx.save();
      ctx.translate(stack.x + 65, stack.y + 16);
      ctx.rotate(stack.rot);
      drawCard(ctx, th, -65, -16, 130, 32, source.item, color);
      ctx.restore();
    }
  });

  /* the Beta pull */
  {
    const ct0 = BETA_PULL.cardT;
    const ct1 = ct0 + 0.08;
    const home = itemHome(3);
    const stack = cardStack(4);
    if (t >= ct0 && t < ct1) {
      const f = ease(seg(t, ct0, ct1));
      const x = lerp(home.x, stack.x, f);
      const y = lerp(home.y, stack.y, f) - Math.sin(f * Math.PI) * 30;
      ctx.save();
      ctx.shadowColor = th.danger;
      ctx.shadowBlur = 14;
      drawCard(ctx, th, x, y, home.w * (1 - 0.25 * f), home.h * (1 - 0.15 * f), BETA_PULL.label, th.danger);
      ctx.restore();
    } else if (t >= ct1) {
      ctx.save();
      ctx.translate(stack.x + 65, stack.y + 16);
      ctx.rotate(stack.rot);
      ctx.shadowColor = th.danger;
      ctx.shadowBlur = 12;
      drawCard(ctx, th, -65, -16, 130, 32, BETA_PULL.label, th.danger);
      ctx.restore();
    }
  }

  /* the guardrail's notes */
  ctx.textAlign = "left";
  ctx.font = font(12.5);
  NOTES.forEach((note, i) => {
    const lineF = seg(t, note.at, note.at + 0.05);
    if (lineF <= 0) return;
    ctx.fillStyle = th[note.color];
    ctx.fillText(note.text.slice(0, Math.ceil(lineF * note.text.length)), BOUNDARY.x + 14, 340 + i * 20);
  });

  /* the email leaves the agent along the rail — and stops at the boundary */
  const envIn = seg(t, 0.62, 0.66);
  const flyF = seg(t, 0.68, 0.76);
  const envOut = seg(t, 0.88, 0.94);
  if (envIn > 0 && envOut < 1) {
    const x = lerp(AGENT_C[0], BOUNDARY.x + BOUNDARY.w - 26, ease(flyF));
    const dots = ENVELOPE_DOTS.map((key) => th[key]);
    drawEnvelope(ctx, th, x, RAIL_Y, dots, ease(envIn), 1 - ease(envOut));
  }
  if (blocked) {
    const pulse = 1 + 0.15 * Math.sin(t * 70);
    ctx.save();
    ctx.fillStyle = th.dangerBg;
    ctx.strokeStyle = th.danger;
    ctx.beginPath();
    ctx.arc(BOUNDARY.x + BOUNDARY.w, RAIL_Y, 10 * pulse, 0, Math.PI * 2);
    ctx.fill();
    ctx.stroke();
    ctx.fillStyle = th.danger;
    ctx.font = font(12, 600);
    ctx.textAlign = "center";
    ctx.fillText("✕", BOUNDARY.x + BOUNDARY.w, RAIL_Y + 0.5);
    ctx.restore();
  }
}

export function GuardrailFigure() {
  return <Figure draw={draw} designW={W} designH={H} durationMs={14000} />;
}

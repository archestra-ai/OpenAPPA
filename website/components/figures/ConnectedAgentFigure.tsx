"use client";

import { Figure } from "@/components/figures/Figure";
import { drawCard, drawEnvelope, ease, lerp, roundRect, seg, type Theme } from "@/components/figures/lib";

/* Tutorial figure 2: the same agent, now connected to four systems. More
   context, sharper analysis, crisper email — and four provenance colors
   flowing into one outbox. */

const W = 900;
const H = 410;

const SOURCES = [
  { title: "Jira", item: "PROJ-101 · blocked", colorKey: "accent" as const },
  { title: "Salesforce", item: "Opp: Q3 renewal", colorKey: "info" as const },
  { title: "GitHub", item: "PR #482 · approved", colorKey: "textStrong" as const },
  { title: "Granola", item: "Call 07/12 · notes", colorKey: "warn" as const },
];

const sourceBox = (i: number) => ({ x: 2, y: 28 + i * 96, w: 185, h: 86 });
const itemHome = (i: number) => ({ x: 18, y: sourceBox(i).y + 36, w: 153, h: 38 });

const AGENT = { x: 390, y: 177, w: 130, h: 56 };
const AGENT_C: [number, number] = [AGENT.x + AGENT.w / 2, AGENT.y + AGENT.h / 2];
const CLIENT = { x: 633, y: 80, w: 265, h: 250 };

const cardStack = (i: number) => ({ x: 398 + i * 12, y: 262 + i * 6, rot: (i - 1.5) * 0.04 });

/* envelope dot → source index */
const MAIL_LINES = [1, 3, 2, 0];

/* the email as a human would read it; font color marks which system each
   part of the text derives from */
const MAIL_TEXT: { text: string; source?: number; gapBefore?: boolean }[] = [
  { text: "Hi Maria," },
  { text: "Your Q3 renewal is on track.", source: 1, gapBefore: true },
  { text: "About the slowness you raised", source: 3 },
  { text: "on Friday's call —", source: 3 },
  { text: "the fix passed review and", source: 2 },
  { text: "ships later this week.", source: 2 },
  { text: "The last open item closes", source: 0 },
  { text: "this sprint.", source: 0 },
  { text: "Best — the Atlas team", gapBefore: true },
];

function sourceColor(th: Theme, i: number) {
  return th[SOURCES[i].colorKey];
}

function draw(ctx: CanvasRenderingContext2D, t: number, th: Theme) {
  const font = (size: number, weight = 400) => `${weight} ${size}px ${th.mono}`;

  /* connectors: draw in during phase 1, stay as faint rails */
  SOURCES.forEach((_, i) => {
    const box = sourceBox(i);
    const from: [number, number] = [box.x + box.w, box.y + box.h / 2];
    const growth = ease(seg(t, 0.025 + i * 0.018, 0.075 + i * 0.018));
    if (growth <= 0) return;
    ctx.strokeStyle = th.borderWeak;
    ctx.lineWidth = 1;
    ctx.beginPath();
    ctx.moveTo(from[0], from[1]);
    ctx.lineTo(lerp(from[0], AGENT_C[0], growth), lerp(from[1], AGENT_C[1], growth));
    ctx.stroke();
  });
  /* agent → client rail */
  ctx.strokeStyle = th.borderWeak;
  ctx.beginPath();
  ctx.moveTo(AGENT.x + AGENT.w, AGENT_C[1]);
  ctx.lineTo(CLIENT.x, AGENT_C[1]);
  ctx.stroke();

  /* source panels */
  SOURCES.forEach((source, i) => {
    const box = sourceBox(i);
    const flash = t > 0.025 + i * 0.018 && t < 0.092 + i * 0.018;
    ctx.fillStyle = th.bg;
    ctx.strokeStyle = flash ? sourceColor(th, i) : th.border;
    roundRect(ctx, box.x, box.y, box.w, box.h, 5);
    ctx.fill();
    ctx.stroke();
    ctx.fillStyle = th.textStrong;
    ctx.font = font(12, 500);
    ctx.textAlign = "left";
    ctx.textBaseline = "middle";
    ctx.fillText(source.title, box.x + 14, box.y + 19);
    /* item card, until it flies off */
    const flyStart = 0.117 + i * 0.042;
    if (t < flyStart) {
      const home = itemHome(i);
      drawCard(ctx, th, home.x, home.y, home.w, home.h, source.item, sourceColor(th, i));
    }
  });

  /* Client panel */
  ctx.fillStyle = th.bg;
  ctx.strokeStyle = th.border;
  roundRect(ctx, CLIENT.x, CLIENT.y, CLIENT.w, CLIENT.h, 5);
  ctx.fill();
  ctx.stroke();
  ctx.fillStyle = th.textStrong;
  ctx.font = font(13, 500);
  ctx.textAlign = "left";
  ctx.fillText("Client", CLIENT.x + 16, CLIENT.y + 20);

  /* Agent box + working dots */
  ctx.fillStyle = th.bgWeak;
  ctx.strokeStyle = th.border;
  roundRect(ctx, AGENT.x, AGENT.y, AGENT.w, AGENT.h, 4);
  ctx.fill();
  ctx.stroke();
  ctx.fillStyle = th.textStrong;
  ctx.font = font(13, 500);
  ctx.textAlign = "center";
  ctx.fillText("Agent", AGENT_C[0], AGENT_C[1]);
  if (t > 0.333 && t < 0.483) {
    for (let i = 0; i < 3; i++) {
      const pulse = 0.35 + 0.65 * Math.abs(Math.sin((t * 40 + i) * 1.1));
      ctx.globalAlpha = pulse;
      ctx.fillStyle = th.textWeak;
      ctx.beginPath();
      ctx.arc(AGENT_C[0] + (i - 1) * 10, AGENT.y + AGENT.h + 12, 2.5, 0, Math.PI * 2);
      ctx.fill();
    }
    ctx.globalAlpha = 1;
  }

  /* flying items + stack */
  SOURCES.forEach((source, i) => {
    const t0 = 0.117 + i * 0.042;
    const t1 = t0 + 0.083;
    const home = itemHome(i);
    const stack = cardStack(i);
    const color = sourceColor(th, i);
    const consumed = seg(t, 0.35, 0.483);
    if (t >= t0 && t < t1) {
      const f = ease(seg(t, t0, t1));
      const x = lerp(home.x, stack.x, f);
      const y = lerp(home.y, stack.y, f) - Math.sin(f * Math.PI) * 30;
      drawCard(ctx, th, x, y, home.w * (1 - 0.25 * f), home.h * (1 - 0.15 * f), source.item, color);
    } else if (t >= t1) {
      ctx.save();
      ctx.translate(stack.x + 65, stack.y + 16);
      ctx.rotate(stack.rot);
      drawCard(ctx, th, -65, -16, 130, 32, source.item, color, 1 - 0.75 * consumed);
      ctx.restore();
    }
  });

  /* envelope: forms at the agent, flies to the client */
  const envIn = seg(t, 0.483, 0.525);
  const flyF = seg(t, 0.525, 0.642);
  if (envIn > 0 && t < 0.658) {
    const start: [number, number] = [AGENT_C[0], AGENT.y - 24];
    const end: [number, number] = [CLIENT.x + CLIENT.w / 2, AGENT_C[1] - 20];
    const f = ease(flyF);
    const x = lerp(start[0], end[0], f);
    const y = lerp(start[1], end[1], f) - Math.sin(f * Math.PI) * 24;
    const dots = MAIL_LINES.map((source) => sourceColor(th, source));
    drawEnvelope(ctx, th, x, y, dots, ease(envIn), t > 0.642 ? seg(t, 0.658, 0.642) : 1);
  }

  /* the crisp email lands, typing itself out (kept at the old, slower pace) */
  const mailIn = seg(t, 0.65, 0.675);
  if (mailIn > 0) {
    ctx.globalAlpha = ease(mailIn);
    ctx.fillStyle = th.textStrong;
    ctx.font = font(13, 500);
    ctx.textAlign = "left";
    ctx.fillText("Atlas status ahead of renewal", CLIENT.x + 16, CLIENT.y + 48);
    ctx.globalAlpha = 1;
    let lineY = CLIENT.y + 70;
    MAIL_TEXT.forEach((line, i) => {
      if (line.gapBefore) lineY += 8;
      const lineF = seg(t, 0.66 + i * 0.03, 0.708 + i * 0.03);
      if (lineF > 0) {
        /* source-derived text renders in its source's color */
        ctx.fillStyle = line.source !== undefined ? sourceColor(th, line.source) : th.text;
        ctx.font = font(12.5);
        ctx.fillText(line.text.slice(0, Math.ceil(lineF * line.text.length)), CLIENT.x + 16, lineY);
      }
      lineY += 17;
    });
  }
}

export function ConnectedAgentFigure() {
  return <Figure draw={draw} designW={W} designH={H} durationMs={12000} />;
}

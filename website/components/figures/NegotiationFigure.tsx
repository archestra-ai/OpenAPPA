"use client";

import { Figure } from "@/components/figures/Figure";
import { drawCard, drawEnvelope, ease, lerp, roundRect, seg, type Theme } from "@/components/figures/lib";
import { logoPixelData } from "@/components/Logo";

/* Tutorial figure 5: the guardrail is not a wall, it's a counterpart. The
   agent asks before pulling, OpenAPPA explains the consequence, the agent
   re-plans — and the report ships to Acme, with Acme's data only. */

const W = 900;
const H = 490;

const LOGO = logoPixelData();

const SOURCES = [
  { title: "Jira", item: "PROJ-101 · Acme", colorKey: "accent" as const },
  { title: "Salesforce", item: "Opp: Acme renewal", colorKey: "info" as const },
  { title: "GitHub", item: "PR #482 · approved", colorKey: "textStrong" as const },
  { title: "Granola", item: "Call · Acme Corp", colorKey: "warn" as const },
];

const sourceBox = (i: number) => ({ x: 40, y: 28 + i * 96, w: 185, h: 86 });
const itemHome = (i: number) => ({ x: 56, y: sourceBox(i).y + 36, w: 153, h: 38 });

const BOUNDARY = { x: 250, y: 60, w: 420, h: 406 };
const AGENT = { x: 254, y: 92, w: 120, h: 44 };
const RAIL_Y = AGENT.y + AGENT.h / 2;
const CLIENT = { x: 690, y: 40, w: 190, h: 150 };

const MESSAGES: { from: "agent" | "appa"; lines: string[] }[] = [
  { from: "agent", lines: ["Hey, I want to pull Beta's", "call recording"] },
  { from: "appa", lines: ["Are you sure? What's your goal?"] },
  { from: "agent", lines: ["To send a report to Acme"] },
  { from: "appa", lines: ["Oh, you won't be able to if", "you pull Beta's call…"] },
  { from: "agent", lines: ["Okay, I'll figure it out with", "Acme data only, thanks!"] },
];

const SLOT = 0.15;
const FIRST = 0.05;
const LINE_H = 17;
const TOP = 160;

const ENVELOPE_DOTS = ["accent", "info", "textStrong", "warn"] as const;

function sourceColor(th: Theme, i: number) {
  return th[SOURCES[i].colorKey];
}

function drawLogo(ctx: CanvasRenderingContext2D, th: Theme, x: number, y: number, capPx: number) {
  const s = capPx / LOGO.capHeight;
  const size = LOGO.cell * s + 0.4;
  for (const p of LOGO.pixels) {
    ctx.fillStyle = p.dim ? th.textWeak : th.textStrong;
    ctx.fillRect(x + p.x * LOGO.cell * s, y + p.y * LOGO.cell * s, size, size);
  }
}

function draw(ctx: CanvasRenderingContext2D, t: number, th: Theme) {
  const font = (size: number, weight = 400) => `${weight} ${size}px ${th.mono}`;

  /* rails: sources → agent and agent → client, through the boundary */
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

  /* source panels, data sitting at home — nothing gets pulled prematurely */
  SOURCES.forEach((source, i) => {
    const box = sourceBox(i);
    ctx.fillStyle = th.bg;
    ctx.strokeStyle = th.border;
    roundRect(ctx, box.x, box.y, box.w, box.h, 5);
    ctx.fill();
    ctx.stroke();
    ctx.fillStyle = th.textStrong;
    ctx.font = font(12, 500);
    ctx.textAlign = "left";
    ctx.textBaseline = "middle";
    ctx.fillText(source.title, box.x + 14, box.y + 19);
    const home = itemHome(i);
    drawCard(ctx, th, home.x, home.y, home.w, home.h, source.item, sourceColor(th, i));
  });

  /* Client panel on the rail */
  ctx.fillStyle = th.bg;
  ctx.strokeStyle = th.border;
  roundRect(ctx, CLIENT.x, CLIENT.y, CLIENT.w, CLIENT.h, 5);
  ctx.fill();
  ctx.stroke();
  ctx.fillStyle = th.textStrong;
  ctx.font = font(13, 500);
  ctx.fillText("Acme Corp", CLIENT.x + 16, CLIENT.y + 20);

  /* the OpenAPPA boundary around the agent — and the conversation */
  ctx.save();
  ctx.strokeStyle = th.border;
  ctx.setLineDash([3, 5]);
  roundRect(ctx, BOUNDARY.x, BOUNDARY.y, BOUNDARY.w, BOUNDARY.h, 6);
  ctx.stroke();
  ctx.restore();
  drawLogo(ctx, th, BOUNDARY.x + BOUNDARY.w - 82, BOUNDARY.y + 12, 10);

  /* Agent inside, on the rail */
  ctx.fillStyle = th.bgWeak;
  ctx.strokeStyle = th.border;
  roundRect(ctx, AGENT.x, AGENT.y, AGENT.w, AGENT.h, 4);
  ctx.fill();
  ctx.stroke();
  ctx.fillStyle = th.textStrong;
  ctx.font = font(13, 500);
  ctx.textAlign = "center";
  ctx.fillText("Agent", AGENT.x + AGENT.w / 2, RAIL_Y);

  /* the chat */
  let rowY = TOP;
  MESSAGES.forEach((message, i) => {
    const bubbleH = 14 + message.lines.length * LINE_H;
    const y = rowY;
    rowY += bubbleH + 20;

    const t0 = FIRST + i * SLOT;
    const pop = ease(seg(t, t0, t0 + 0.03));
    if (pop <= 0) return;
    const typeF = seg(t, t0 + 0.02, t0 + 0.11);

    ctx.font = font(13);
    const textW = Math.max(...message.lines.map((line) => ctx.measureText(line).width));
    const w = textW + 26;
    const isAgent = message.from === "agent";
    const x = isAgent ? BOUNDARY.x + 20 : BOUNDARY.x + BOUNDARY.w - 20 - w;

    ctx.save();
    const ax = isAgent ? x : x + w;
    ctx.translate(ax, y + bubbleH / 2);
    ctx.scale(0.85 + 0.15 * pop, 0.85 + 0.15 * pop);
    ctx.globalAlpha = pop;
    ctx.translate(-ax, -(y + bubbleH / 2));

    /* speaker label */
    ctx.textBaseline = "alphabetic";
    if (isAgent) {
      ctx.fillStyle = th.textWeak;
      ctx.font = font(11);
      ctx.textAlign = "left";
      ctx.fillText("Agent", x + 2, y - 6);
    } else {
      drawLogo(ctx, th, x + w - 52, y - 14, 7.5);
    }

    /* bubble */
    ctx.fillStyle = isAgent ? th.bgWeak : th.accentBg;
    ctx.strokeStyle = isAgent ? th.border : th.accentBorder;
    roundRect(ctx, x, y, w, bubbleH, 8);
    ctx.fill();
    ctx.stroke();
    /* tail */
    ctx.beginPath();
    if (isAgent) {
      ctx.moveTo(x + 2, y + bubbleH - 1);
      ctx.lineTo(x - 6, y + bubbleH + 5);
      ctx.lineTo(x + 12, y + bubbleH - 1);
    } else {
      ctx.moveTo(x + w - 2, y + bubbleH - 1);
      ctx.lineTo(x + w + 6, y + bubbleH + 5);
      ctx.lineTo(x + w - 12, y + bubbleH - 1);
    }
    ctx.closePath();
    ctx.fill();

    /* text, typing across the wrapped lines */
    ctx.fillStyle = th.textStrong;
    ctx.font = font(13);
    ctx.textAlign = "left";
    ctx.textBaseline = "middle";
    const total = message.lines.reduce((sum, line) => sum + line.length, 0);
    let remaining = Math.ceil(typeF * total);
    message.lines.forEach((line, lineIndex) => {
      if (remaining <= 0) return;
      const shown = line.slice(0, remaining);
      remaining -= shown.length;
      ctx.fillText(shown, x + 13, y + 7 + LINE_H * (lineIndex + 0.5));
    });
    ctx.restore();
  });

  /* the envelope crosses the boundary freely this time */
  const envIn = seg(t, 0.86, 0.9);
  const flyF = seg(t, 0.88, 0.97);
  if (envIn > 0) {
    const x = lerp(AGENT.x + AGENT.w / 2, CLIENT.x + CLIENT.w / 2, ease(flyF));
    const dots = ENVELOPE_DOTS.map((key) => th[key]);
    drawEnvelope(ctx, th, x, RAIL_Y, dots, ease(envIn), flyF >= 1 ? 0.9 : 1);
  }
  const crossed = t > 0.92;
  if (crossed) {
    ctx.save();
    ctx.fillStyle = th.accentBg;
    ctx.strokeStyle = th.accent;
    ctx.beginPath();
    ctx.arc(BOUNDARY.x + BOUNDARY.w, RAIL_Y, 9, 0, Math.PI * 2);
    ctx.fill();
    ctx.stroke();
    ctx.fillStyle = th.accent;
    ctx.font = font(12, 600);
    ctx.textAlign = "center";
    ctx.textBaseline = "middle";
    ctx.fillText("✓", BOUNDARY.x + BOUNDARY.w, RAIL_Y + 0.5);
    ctx.restore();
  }
}

export function NegotiationFigure() {
  return <Figure draw={draw} designW={W} designH={H} durationMs={14000} />;
}

"use client";

import { Figure } from "@/components/figures/Figure";
import { drawCard, drawEnvelope, ease, lerp, roundRect, seg, type Theme } from "@/components/figures/lib";

/* Tutorial figure 3: no attacker, no injection — just non-determinism.
   The agent gathers the usual context, then decides on its own that a
   persuasive update needs comparison data, pulls other clients' calls
   from Granola (glowing red), and the email leaks pricing. */

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
const CLIENT = { x: 633, y: 70, w: 265, h: 290 };

const cardStack = (i: number) => ({ x: 398 + i * 12, y: 262 + i * 6, rot: (i - 1.5) * 0.04 });

/* the pull nobody asked for */
const EXTRA_CALLS = [{ label: "Call · Beta Ltd", cardT: 0.44 }];

const ENVELOPE_DOTS = ["info", "textStrong", "accent", "warn", "danger"] as const;

/* the email; highlighted segments render in the danger color */
interface MailSeg {
  text: string;
  hl?: boolean;
}
const MAIL_TEXT: { segs: MailSeg[]; gapBefore?: boolean }[] = [
  { segs: [{ text: "Hi Maria," }] },
  { segs: [{ text: "Your Q3 renewal is on track." }], gapBefore: true },
  { segs: [{ text: "The latency fix you raised is" }] },
  { segs: [{ text: "approved and ships this week," }] },
  { segs: [{ text: "and the last open item closes" }] },
  { segs: [{ text: "this sprint." }] },
  { segs: [{ text: "As you're paying " }, { text: "1.75x", hl: true }], gapBefore: true },
  { segs: [{ text: "compared to our other", hl: true }] },
  { segs: [{ text: "clients", hl: true }, { text: ", we're dedicated" }] },
  { segs: [{ text: "to make sure you're happy" }] },
  { segs: [{ text: "with the service we provide." }] },
  { segs: [{ text: "Best — the Atlas team" }], gapBefore: true },
];

function sourceColor(th: Theme, i: number) {
  return th[SOURCES[i].colorKey];
}

function draw(ctx: CanvasRenderingContext2D, t: number, th: Theme) {
  const font = (size: number, weight = 400) => `${weight} ${size}px ${th.mono}`;
  const granolaBusy = t > 0.3 && t < 0.62;

  /* connectors (established from the previous slide) */
  ctx.strokeStyle = th.borderWeak;
  ctx.lineWidth = 1;
  SOURCES.forEach((_, i) => {
    const box = sourceBox(i);
    ctx.beginPath();
    ctx.moveTo(box.x + box.w, box.y + box.h / 2);
    ctx.lineTo(AGENT_C[0], AGENT_C[1]);
    ctx.stroke();
  });
  ctx.beginPath();
  ctx.moveTo(AGENT.x + AGENT.w, AGENT_C[1]);
  ctx.lineTo(CLIENT.x, AGENT_C[1]);
  ctx.stroke();

  /* source panels; Granola glows red while it's being over-queried */
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
    const flyStart = 0.04 + i * 0.03;
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
  ctx.fillText("Client", CLIENT.x + 16, CLIENT.y + 20);

  /* Agent */
  ctx.fillStyle = th.bgWeak;
  ctx.strokeStyle = th.border;
  roundRect(ctx, AGENT.x, AGENT.y, AGENT.w, AGENT.h, 4);
  ctx.fill();
  ctx.stroke();
  ctx.fillStyle = th.textStrong;
  ctx.font = font(13, 500);
  ctx.textAlign = "center";
  ctx.fillText("Agent", AGENT_C[0], AGENT_C[1]);

  /* the ??? moment — the model decides it wants more */
  const qIn = ease(seg(t, 0.24, 0.28));
  const qOut = ease(seg(t, 0.56, 0.6));
  if (qIn > 0 && qOut < 1) {
    ctx.save();
    ctx.globalAlpha = qIn * (1 - qOut);
    ctx.fillStyle = th.warn;
    ctx.font = font(19, 600);
    ctx.fillText("???", AGENT_C[0], AGENT.y - 22 - 6 * qIn);
    ctx.restore();
  }

  /* usual items fly in quickly */
  SOURCES.forEach((source, i) => {
    const t0 = 0.04 + i * 0.03;
    const t1 = t0 + 0.07;
    const home = itemHome(i);
    const stack = cardStack(i);
    const color = sourceColor(th, i);
    const consumed = seg(t, 0.56, 0.64);
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

  /* the extra Granola pulls nobody asked for */
  EXTRA_CALLS.forEach((call, k) => {
    const ct0 = call.cardT;
    const ct1 = ct0 + 0.08;
    const home = itemHome(3);
    const stack = cardStack(4 + k);
    const consumed = seg(t, 0.56, 0.64);
    if (t >= ct0 && t < ct1) {
      const f = ease(seg(t, ct0, ct1));
      const x = lerp(home.x, stack.x, f);
      const y = lerp(home.y, stack.y, f) - Math.sin(f * Math.PI) * 30;
      ctx.save();
      ctx.shadowColor = th.danger;
      ctx.shadowBlur = 14;
      drawCard(ctx, th, x, y, home.w * (1 - 0.25 * f), home.h * (1 - 0.15 * f), call.label, th.danger);
      ctx.restore();
    } else if (t >= ct1) {
      ctx.save();
      ctx.translate(stack.x + 65, stack.y + 16);
      ctx.rotate(stack.rot);
      ctx.shadowColor = th.danger;
      ctx.shadowBlur = 12 * (1 - consumed);
      drawCard(ctx, th, -65, -16, 130, 32, call.label, th.danger, 1 - 0.6 * consumed);
      ctx.restore();
    }
  });

  /* label the red cards for what they are */
  const labelIn = ease(seg(t, 0.48, 0.52));
  const labelOut = ease(seg(t, 0.62, 0.66));
  if (labelIn > 0 && labelOut < 1) {
    ctx.save();
    ctx.globalAlpha = labelIn * (1 - labelOut);
    ctx.fillStyle = th.danger;
    ctx.font = font(12.5, 500);
    ctx.textAlign = "left";
    ctx.fillText("another client's information", cardStack(0).x, cardStack(4).y + 46);
    ctx.restore();
  }

  /* envelope — now carrying a red dot too */
  const envIn = seg(t, 0.64, 0.68);
  const flyF = seg(t, 0.68, 0.78);
  if (envIn > 0 && t < 0.8) {
    const start: [number, number] = [AGENT_C[0], AGENT.y - 24];
    const end: [number, number] = [CLIENT.x + CLIENT.w / 2, AGENT_C[1] - 20];
    const f = ease(flyF);
    const x = lerp(start[0], end[0], f);
    const y = lerp(start[1], end[1], f) - Math.sin(f * Math.PI) * 24;
    const dots = ENVELOPE_DOTS.map((key) => th[key]);
    drawEnvelope(ctx, th, x, y, dots, ease(envIn), t > 0.78 ? seg(t, 0.8, 0.78) : 1);
  }

  /* the email lands — with the sentence you never want a client to read */
  const mailIn = seg(t, 0.79, 0.815);
  if (mailIn > 0) {
    ctx.globalAlpha = ease(mailIn);
    ctx.fillStyle = th.textStrong;
    ctx.font = font(13, 500);
    ctx.textAlign = "left";
    ctx.fillText("Our commitment to your success", CLIENT.x + 16, CLIENT.y + 48);
    ctx.globalAlpha = 1;
    let lineY = CLIENT.y + 70;
    MAIL_TEXT.forEach((line, i) => {
      if (line.gapBefore) lineY += 8;
      const lineF = seg(t, 0.795 + i * 0.014, 0.825 + i * 0.014);
      if (lineF > 0) {
        const total = line.segs.reduce((sum, s) => sum + s.text.length, 0);
        let remaining = Math.ceil(lineF * total);
        let x = CLIENT.x + 16;
        ctx.font = font(12.5);
        for (const segment of line.segs) {
          if (remaining <= 0) break;
          const shown = segment.text.slice(0, remaining);
          remaining -= shown.length;
          ctx.fillStyle = segment.hl ? th.danger : th.text;
          ctx.fillText(shown, x, lineY);
          x += ctx.measureText(shown).width;
        }
      }
      lineY += 16;
    });
  }
}

export function ExfiltrationFigure() {
  return <Figure draw={draw} designW={W} designH={H} durationMs={13000} />;
}

"use client";

import { Figure } from "@/components/figures/Figure";
import { chip, drawCard, drawEnvelope, ease, lerp, roundRect, seg, type Theme } from "@/components/figures/lib";

/* Tutorial figure: one fetch, two endings. Ending one — accept the
   narrowing, go internal, and the auditor mail waits on a ruling. Ending
   two — fetch in a child, return through remove_pii, and the parent stays
   public with GitHub open. The slider's first half plays ending one, the
   second half ending two; finished states persist. */

const W = 900;
const H = 520;

const CRM = { x: 30, y: 212, w: 180, h: 96 };
const TICKET = { x: 44, y: 250, w: 152, h: 40 };

const A = {
  agent: { x: 300, y: 84, w: 110, h: 40 },
  chip: { x: 355, y: 152 },
  github: { x: 680, y: 56, w: 190, h: 40 },
  mail: { x: 680, y: 148, w: 190, h: 40 },
};

const B = {
  agent: { x: 300, y: 324, w: 110, h: 40 },
  chip: { x: 355, y: 390 },
  child: { x: 280, y: 420, w: 200, h: 66 },
  childCard: { x: 294, y: 446, w: 110, h: 32 },
  sanitizer: { x: 560, y: 436, w: 130, h: 36 },
  github: { x: 680, y: 324, w: 190, h: 40 },
};

type Point = { x: number; y: number };

const center = (box: { x: number; y: number; w: number; h: number }): Point => ({
  x: box.x + box.w / 2,
  y: box.y + box.h / 2,
});

const onLine = (from: Point, to: Point, f: number): Point => ({
  x: lerp(from.x, to.x, f),
  y: lerp(from.y, to.y, f),
});

const onCurve = (from: Point, to: Point, control: Point, f: number): Point => ({
  x: (1 - f) ** 2 * from.x + 2 * (1 - f) * f * control.x + f ** 2 * to.x,
  y: (1 - f) ** 2 * from.y + 2 * (1 - f) * f * control.y + f ** 2 * to.y,
});

function panel(
  ctx: CanvasRenderingContext2D,
  th: Theme,
  box: { x: number; y: number; w: number; h: number },
  label: string,
) {
  ctx.fillStyle = th.bg;
  ctx.strokeStyle = th.border;
  roundRect(ctx, box.x, box.y, box.w, box.h, 5);
  ctx.fill();
  ctx.stroke();
  ctx.fillStyle = th.textStrong;
  ctx.font = `500 12px ${th.mono}`;
  ctx.textAlign = "left";
  ctx.textBaseline = "middle";
  ctx.fillText(label, box.x + 14, box.y + box.h / 2);
}

function badge(ctx: CanvasRenderingContext2D, th: Theme, x: number, y: number, ok: boolean, alpha: number) {
  if (alpha <= 0) return;
  ctx.save();
  ctx.globalAlpha = alpha;
  ctx.fillStyle = ok ? th.accentBg : th.dangerBg;
  ctx.strokeStyle = ok ? th.accent : th.danger;
  ctx.beginPath();
  ctx.arc(x, y, 9, 0, Math.PI * 2);
  ctx.fill();
  ctx.stroke();
  ctx.fillStyle = ok ? th.accent : th.danger;
  ctx.font = `600 12px ${th.mono}`;
  ctx.textAlign = "center";
  ctx.textBaseline = "middle";
  ctx.fillText(ok ? "✓" : "✕", x, y + 0.5);
  ctx.restore();
}

function ring(ctx: CanvasRenderingContext2D, color: string, x: number, y: number, f: number) {
  if (f <= 0 || f >= 1) return;
  ctx.save();
  ctx.globalAlpha = 1 - f;
  ctx.strokeStyle = color;
  ctx.beginPath();
  ctx.arc(x, y, 14 + 24 * f, 0, Math.PI * 2);
  ctx.stroke();
  ctx.restore();
}

function rail(ctx: CanvasRenderingContext2D, from: Point, to: Point) {
  ctx.beginPath();
  ctx.moveTo(from.x, from.y);
  ctx.lineTo(to.x, to.y);
  ctx.stroke();
}

function draw(ctx: CanvasRenderingContext2D, t: number, th: Theme) {
  const font = (size: number, weight = 400) => `${weight} ${size}px ${th.mono}`;
  const label = (text: string, x: number, y: number, alpha: number, align: CanvasTextAlign = "center") => {
    if (alpha <= 0) return;
    ctx.save();
    ctx.globalAlpha = alpha;
    ctx.fillStyle = th.textWeak;
    ctx.font = font(11);
    ctx.textAlign = align;
    ctx.textBaseline = "middle";
    ctx.fillText(text, x, y);
    ctx.restore();
  };

  /* lane divider */
  ctx.save();
  ctx.strokeStyle = th.borderWeak;
  ctx.setLineDash([3, 5]);
  rail(ctx, { x: 240, y: 260 }, { x: 880, y: 260 });
  ctx.restore();

  /* rails, lane A */
  ctx.strokeStyle = th.borderWeak;
  ctx.lineWidth = 1;
  const crmOut: Point = { x: CRM.x + CRM.w, y: 260 };
  rail(ctx, crmOut, { x: A.agent.x, y: center(A.agent).y });
  rail(ctx, { x: A.agent.x + A.agent.w, y: 96 }, { x: A.github.x, y: center(A.github).y });
  rail(ctx, { x: A.agent.x + A.agent.w, y: 112 }, { x: A.mail.x, y: center(A.mail).y });
  rail(ctx, { x: B.agent.x + B.agent.w, y: center(B.agent).y }, { x: B.github.x, y: center(B.github).y });

  /* the CRM and its ticket, sitting at home */
  panel(ctx, th, CRM, "");
  ctx.fillStyle = th.textStrong;
  ctx.font = font(12, 500);
  ctx.textAlign = "left";
  ctx.fillText("CRM", CRM.x + 14, CRM.y + 20);
  drawCard(ctx, th, TICKET.x, TICKET.y, TICKET.w, TICKET.h, "ticket #4821", th.warn);

  /* agents and sinks */
  panel(ctx, th, A.agent, "  Agent");
  panel(ctx, th, B.agent, "  Agent");
  panel(ctx, th, A.github, "file_github_issue");
  panel(ctx, th, A.mail, "send_email(auditor)");
  panel(ctx, th, B.github, "file_github_issue");
  panel(ctx, th, B.sanitizer, "remove_pii");

  /* lane titles, over everything static */
  ctx.fillStyle = th.textWeak;
  ctx.font = font(12, 500);
  ctx.textAlign = "left";
  ctx.textBaseline = "middle";
  ctx.fillText("ending one · accept the narrowing", 40, 48);
  ctx.fillText("ending two · fetch in a child", 40, 340);

  /* ---- ending one ---- */
  const ticketFrom = center(TICKET);
  const agentATo = center(A.agent);
  const haltA = onLine(ticketFrom, agentATo, 0.55);

  const approach = ease(seg(t, 0.05, 0.12));
  const resume = ease(seg(t, 0.2, 0.26));
  if (approach > 0 && resume < 1) {
    const f = resume > 0 ? lerp(0.55, 1, resume) : 0.55 * approach;
    const p = onLine(ticketFrom, agentATo, f);
    drawCard(ctx, th, p.x - 55, p.y - 15, 110, 30, "ticket #4821", th.warn, 1 - seg(resume, 0.8, 1));
  }

  const stopA = ease(seg(t, 0.12, 0.15)) * (1 - seg(t, 0.26, 0.3));
  if (stopA > 0) {
    ctx.save();
    ctx.globalAlpha = stopA;
    chip(ctx, haltA.x, haltA.y - 30, "narrowing → internal?", th.warn, th.warnBg, th.mono);
    ctx.restore();
  }
  label("accepted on the record", haltA.x, haltA.y + 28, ease(seg(t, 0.19, 0.22)) * (1 - seg(t, 0.26, 0.3)));

  const flippedA = t >= 0.28;
  ring(ctx, th.warn, A.chip.x, A.chip.y, seg(t, 0.28, 0.36));
  chip(
    ctx,
    A.chip.x,
    A.chip.y,
    flippedA ? "internal · trusted" : "public · trusted",
    flippedA ? th.warn : th.accent,
    flippedA ? th.warnBg : th.accentBg,
    th.mono,
  );

  const closedA = ease(seg(t, 0.32, 0.36));
  badge(ctx, th, A.github.x - 12, center(A.github).y, false, closedA);
  label("closed for this run", center(A.github).x, A.github.y + A.github.h + 14, closedA);

  const mailFrom: Point = { x: A.agent.x + A.agent.w, y: 112 };
  const mailTo: Point = { x: A.mail.x - 44, y: center(A.mail).y };
  const haltMail = onLine(mailFrom, mailTo, 0.55);
  const depart = ease(seg(t, 0.38, 0.44));
  const release = ease(seg(t, 0.54, 0.6));
  if (depart > 0) {
    const f = release > 0 ? lerp(0.55, 1, release) : 0.55 * depart;
    const p = onLine(mailFrom, mailTo, f);
    drawEnvelope(ctx, th, p.x, p.y, [th.warn], 1, release >= 1 ? 0.9 : 1);
  }
  const blockA = ease(seg(t, 0.44, 0.47));
  if (blockA > 0) {
    ctx.save();
    ctx.globalAlpha = blockA;
    chip(ctx, haltMail.x, haltMail.y - 28, "auditor ∉ readers", th.danger, th.dangerBg, th.mono);
    ctx.restore();
  }
  const ruling = ease(seg(t, 0.5, 0.53));
  if (ruling > 0) {
    ctx.save();
    ctx.globalAlpha = ruling;
    chip(ctx, haltMail.x, haltMail.y + 64, "user ✓", th.accent, th.accentBg, th.mono);
    ctx.restore();
  }
  badge(ctx, th, A.mail.x - 12, center(A.mail).y, true, ease(seg(t, 0.6, 0.64)));
  label("egress ▸ log", center(A.mail).x, A.mail.y + A.mail.h + 14, ease(seg(t, 0.6, 0.64)));

  /* ---- ending two ---- */
  const bubble = ease(seg(t, 0.62, 0.66));
  if (bubble > 0) {
    ctx.save();
    ctx.globalAlpha = bubble;

    ctx.strokeStyle = th.borderWeak;
    rail(ctx, { x: CRM.x + CRM.w, y: 276 }, { x: B.child.x, y: center(B.child).y });
    rail(ctx, { x: B.child.x + B.child.w, y: center(B.child).y }, { x: B.sanitizer.x, y: center(B.sanitizer).y });
    ctx.setLineDash([3, 5]);
    rail(ctx, { x: 355, y: B.agent.y + B.agent.h }, { x: 355, y: B.child.y });
    ctx.setLineDash([]);

    ctx.strokeStyle = th.border;
    ctx.setLineDash([3, 5]);
    roundRect(ctx, B.child.x, B.child.y, B.child.w, B.child.h, 6);
    ctx.stroke();
    ctx.setLineDash([]);
    ctx.fillStyle = th.textWeak;
    ctx.font = font(11);
    ctx.textAlign = "left";
    ctx.textBaseline = "middle";
    ctx.fillText("child", B.child.x + 12, B.child.y + 12);
    ctx.restore();
  }

  const fetchB = ease(seg(t, 0.66, 0.72));
  const childSlot = center(B.childCard);
  if (fetchB > 0 && fetchB < 1) {
    const p = onLine(ticketFrom, childSlot, fetchB);
    drawCard(ctx, th, p.x - 55, p.y - 15, 110, 30, "ticket #4821", th.warn);
  }
  /* the raw ticket stays behind in the child; only the derivation crosses */
  const returned = ease(seg(t, 0.76, 0.81));
  if (fetchB >= 1) {
    drawCard(ctx, th, B.childCard.x, B.childCard.y, B.childCard.w, B.childCard.h, "ticket #4821", th.warn);
  }
  const childNarrowed = ease(seg(t, 0.72, 0.75));
  if (childNarrowed > 0) {
    ctx.save();
    ctx.globalAlpha = childNarrowed;
    chip(ctx, 380, 502, "child · internal · trusted", th.warn, th.warnBg, th.mono);
    ctx.restore();
  }

  const sanitizerAt = center(B.sanitizer);
  if (returned > 0 && returned < 1) {
    const p = onLine(childSlot, sanitizerAt, returned);
    drawCard(ctx, th, p.x - 55, p.y - 15, 110, 30, "ticket #4821", th.warn);
  }
  ring(ctx, th.accent, sanitizerAt.x, sanitizerAt.y, seg(t, 0.81, 0.85));

  const cleaned = ease(seg(t, 0.84, 0.89));
  if (cleaned > 0 && cleaned < 1) {
    const p = onCurve({ x: sanitizerAt.x, y: B.sanitizer.y }, center(B.agent), { x: 520, y: 296 }, cleaned);
    drawCard(ctx, th, p.x - 55, p.y - 15, 110, 30, "ticket · redacted", th.accent, 1 - seg(cleaned, 0.8, 1));
  }

  ring(ctx, th.accent, B.chip.x, B.chip.y, seg(t, 0.89, 0.95));
  chip(ctx, B.chip.x, B.chip.y, "public · trusted", th.accent, th.accentBg, th.mono);

  const filed = ease(seg(t, 0.9, 0.96));
  if (filed > 0 && filed < 1) {
    const p = onLine({ x: B.agent.x + B.agent.w, y: center(B.agent).y }, { x: B.github.x, y: center(B.github).y }, filed);
    drawCard(ctx, th, p.x - 55, p.y - 15, 110, 30, "issue · public", th.accent);
  }
  badge(ctx, th, B.github.x - 12, center(B.github).y, true, ease(seg(t, 0.96, 0.99)));
  label("egress · mutation ▸ log", center(B.github).x, B.github.y + B.github.h + 14, ease(seg(t, 0.96, 0.99)));
}

export function TwoEndingsFigure() {
  return <Figure draw={draw} designW={W} designH={H} durationMs={18000} />;
}

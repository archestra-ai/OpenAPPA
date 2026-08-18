"use client";

import { Figure } from "@/components/figures/Figure";
import { chip, drawCard, ease, lerp, roundRect, seg, type Theme } from "@/components/figures/lib";
import { logoPixelData } from "@/components/Logo";

/* Integration figure: a protected Claude Code session. Every hook event
   goes to APPA, and APPA answers before the action runs. One call comes
   back allowed, one comes back blocked with safer options. */

const W = 900;
const H = 430;

const LOGO = logoPixelData();

const SESSION = { x: 16, y: 52, w: 240, h: 274 };
const HOOKS = { x: 336, y: 52, w: 212, h: 274 };
const RUNTIME = { x: 644, y: 52, w: 240, h: 274 };

const HOOK_ROWS = [
  "SessionStart",
  "UserPromptSubmit",
  "PreToolUse",
  "PostToolUse",
  "PostToolUseFailure",
  "SubagentStart",
  "SubagentStop",
];
const rowY = (i: number) => HOOKS.y + 52 + i * 30;
const PRE = 2; // the PreToolUse row carries both round trips
const RAIL_Y = rowY(PRE);

const CARD = { w: 175, h: 34 };
const CARD_HOME_X = SESSION.x + 24;
const CARD_STOP_X = HOOKS.x - CARD.w - 6;

/* beat boundaries for the two round trips */
const CALL_1 = { out: [0.04, 0.14], post: [0.16, 0.24], back: [0.27, 0.35], done: 0.36 };
const CALL_2 = { out: [0.48, 0.58], post: [0.6, 0.68], back: [0.71, 0.79], done: 0.8 };

const NOTES: { at: number; text: string; color: "accent" | "danger" }[] = [
  { at: CALL_1.done, text: "✓ allowed — the call runs", color: "accent" },
  { at: CALL_2.done, text: "✕ blocked — APPA tells the agent how to proceed safely", color: "danger" },
];

/** The pixel wordmark, drawn straight onto the canvas. */
function drawLogo(ctx: CanvasRenderingContext2D, th: Theme, x: number, y: number, capPx: number) {
  const s = capPx / LOGO.capHeight;
  const size = LOGO.cell * s + 0.4; // slight overlap so adjacent cells don't seam
  for (const p of LOGO.pixels) {
    ctx.fillStyle = p.dim ? th.textWeak : th.textStrong;
    ctx.fillRect(x + p.x * LOGO.cell * s, y + p.y * LOGO.cell * s, size, size);
  }
}

/** One request/answer round trip on the PreToolUse rail. */
function roundTrip(
  ctx: CanvasRenderingContext2D,
  th: Theme,
  t: number,
  beat: typeof CALL_1,
  label: string,
  color: string,
  answer: string,
  answerColor: string,
  answerBg: string,
) {
  const outF = ease(seg(t, beat.out[0], beat.out[1]));
  const fade = seg(t, beat.done + 0.04, beat.done + 0.1);
  if (t >= beat.out[0] && fade < 1) {
    const x = lerp(CARD_HOME_X, CARD_STOP_X, outF);
    drawCard(ctx, th, x, RAIL_Y - CARD.h / 2, CARD.w, CARD.h, label, color, 1 - fade);
  }
  const postF = seg(t, beat.post[0], beat.post[1]);
  if (postF > 0 && postF < 1) {
    chip(ctx, lerp(HOOKS.x + HOOKS.w, RUNTIME.x, ease(postF)), RAIL_Y, "POST /hook", th.textStrong, th.bg, th.mono);
  }
  const backF = seg(t, beat.back[0], beat.back[1]);
  if (backF > 0 && backF < 1) {
    chip(ctx, lerp(RUNTIME.x, HOOKS.x + HOOKS.w, ease(backF)), RAIL_Y, answer, answerColor, answerBg, th.mono);
  }
}

function draw(ctx: CanvasRenderingContext2D, t: number, th: Theme) {
  const font = (size: number, weight = 400) => `${weight} ${size}px ${th.mono}`;
  const runtimeBusy =
    (t > CALL_1.post[0] && t < CALL_1.back[1]) || (t > CALL_2.post[0] && t < CALL_2.back[1]);
  const blocked = t >= CALL_2.done;

  /* the PreToolUse rail: session → hooks → runtime */
  ctx.strokeStyle = th.borderWeak;
  ctx.lineWidth = 1;
  ctx.beginPath();
  ctx.moveTo(SESSION.x + SESSION.w, RAIL_Y);
  ctx.lineTo(HOOKS.x, RAIL_Y);
  ctx.moveTo(HOOKS.x + HOOKS.w, RAIL_Y);
  ctx.lineTo(RUNTIME.x, RAIL_Y);
  ctx.stroke();

  /* Claude Code session */
  ctx.fillStyle = th.bg;
  ctx.strokeStyle = th.border;
  roundRect(ctx, SESSION.x, SESSION.y, SESSION.w, SESSION.h, 5);
  ctx.fill();
  ctx.stroke();
  ctx.fillStyle = th.textStrong;
  ctx.font = font(13, 500);
  ctx.textAlign = "left";
  ctx.textBaseline = "middle";
  ctx.fillText("Claude Code session", SESSION.x + 14, SESSION.y + 20);
  ctx.fillStyle = th.textWeak;
  ctx.font = font(11.5);
  ctx.fillText("clappa · APPA_GATE=1", SESSION.x + 14, SESSION.y + 40);
  ctx.fillText("the agent proposes actions", SESSION.x + 14, SESSION.y + SESSION.h - 18);

  /* plugin hooks — a dotted gate between the session and its actions */
  ctx.save();
  ctx.strokeStyle = blocked ? th.danger : th.border;
  ctx.setLineDash([3, 5]);
  if (blocked) {
    ctx.shadowColor = th.danger;
    ctx.shadowBlur = 8;
  }
  roundRect(ctx, HOOKS.x, HOOKS.y, HOOKS.w, HOOKS.h, 6);
  ctx.stroke();
  ctx.restore();
  ctx.fillStyle = th.textStrong;
  ctx.font = font(13, 500);
  ctx.fillText("plugin hooks", HOOKS.x + 14, HOOKS.y + 20);
  HOOK_ROWS.forEach((name, i) => {
    const active = i === PRE && t > CALL_1.out[1];
    ctx.fillStyle = active ? th.accent : th.textWeak;
    ctx.font = font(12, active ? 600 : 400);
    ctx.fillText(name, HOOKS.x + 14, rowY(i));
  });

  /* APPA */
  ctx.save();
  if (runtimeBusy) {
    ctx.shadowColor = th.accent;
    ctx.shadowBlur = 10;
    ctx.strokeStyle = th.accent;
  } else {
    ctx.strokeStyle = th.border;
  }
  ctx.fillStyle = th.bg;
  roundRect(ctx, RUNTIME.x, RUNTIME.y, RUNTIME.w, RUNTIME.h, 5);
  ctx.fill();
  ctx.stroke();
  ctx.restore();
  drawLogo(ctx, th, RUNTIME.x + 14, RUNTIME.y + 13, 10);
  ctx.fillStyle = th.textStrong;
  ctx.font = font(13, 500);
  ctx.fillText("APPA", RUNTIME.x + 14, RUNTIME.y + 46);
  ["Policy Config", "Deterministic Engine", "Event Log"].forEach((line, i) => {
    drawCard(ctx, th, RUNTIME.x + 14, RUNTIME.y + 66 + i * 48, RUNTIME.w - 28, 38, line, th.border);
  });

  /* the two round trips */
  roundTrip(ctx, th, t, CALL_1, "Read · report.md", th.accent, "allow", th.accent, th.accentBg);
  roundTrip(ctx, th, t, CALL_2, "WebFetch · send data", th.danger, "block", th.danger, th.dangerBg);

  /* verdict marks at the gate */
  if (t >= CALL_1.done && t < CALL_2.out[0]) {
    ctx.fillStyle = th.accent;
    ctx.font = font(15, 600);
    ctx.textAlign = "center";
    ctx.fillText("✓", HOOKS.x + HOOKS.w + 18, RAIL_Y - 16);
    ctx.textAlign = "left";
  }
  if (blocked) {
    const pulse = 1 + 0.15 * Math.sin(t * 70);
    ctx.save();
    ctx.fillStyle = th.dangerBg;
    ctx.strokeStyle = th.danger;
    ctx.beginPath();
    ctx.arc(HOOKS.x, RAIL_Y, 10 * pulse, 0, Math.PI * 2);
    ctx.fill();
    ctx.stroke();
    ctx.fillStyle = th.danger;
    ctx.font = font(12, 600);
    ctx.textAlign = "center";
    ctx.fillText("✕", HOOKS.x, RAIL_Y + 0.5);
    ctx.restore();
    ctx.textAlign = "left";
  }

  /* the gate's notes, one line per verdict; they fade in so the text
     never shifts while it appears. chip() leaves the canvas text state
     center-aligned, so the alignment is pinned here — without this the
     lines jump sideways while a pill is in flight. */
  ctx.font = font(12.5);
  ctx.textAlign = "left";
  ctx.textBaseline = "middle";
  NOTES.forEach((note, i) => {
    const lineF = seg(t, note.at, note.at + 0.05);
    if (lineF <= 0) return;
    ctx.save();
    ctx.globalAlpha = ease(lineF);
    ctx.fillStyle = th[note.color];
    ctx.fillText(note.text, HOOKS.x, 356 + i * 22);
    ctx.restore();
  });
}

/* The layout is authored in 900 design units; declaring a smaller design
   width to the shell renders the same scene larger on the page. */
const SCALE = 0.8;

function scaledDraw(ctx: CanvasRenderingContext2D, t: number, th: Theme) {
  ctx.save();
  ctx.scale(SCALE, SCALE);
  draw(ctx, t, th);
  ctx.restore();
}

export function ClaudeCodeHooksFigure() {
  return <Figure draw={scaledDraw} designW={W * SCALE} designH={H * SCALE} durationMs={12000} />;
}

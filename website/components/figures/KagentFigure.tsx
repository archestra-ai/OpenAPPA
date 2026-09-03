"use client";

import { Figure } from "@/components/figures/Figure";
import { chip, drawCard, ease, lerp, roundRect, seg, type Theme } from "@/components/figures/lib";
import { logoPixelData } from "@/components/Logo";

/* Integration figure: a protected kagent declarative agent on Kubernetes.
   Every tool call passes through the Google ADK plugin gate (AppaPluginKagent)
   to OpenAPPA before execution.
   Call 1: Allowed secret read narrows trajectory audience to [ops].
   Call 2: Blocked leak — sending ops data to public sink is blocked.
   Call 3: Full Human-in-the-Loop flow to completion — destructive action suspends,
           operator clicks Approve in kagent UI, and deployment restarts. */

const W = 900;
const H = 400;

const LOGO = logoPixelData();

const POD = { x: 24, y: 40, w: 230, h: 250 };
const GATE = { x: 334, y: 40, w: 232, h: 250 };
const RUNTIME = { x: 646, y: 40, w: 230, h: 250 };

const HOOK_ROWS = [
  "SessionStart",
  "UserPrompt",
  "BeforeTool (PreTool)",
  "AfterTool (PostTool)",
  "A2ASpawn (Child)",
  "TurnEnd",
];
const rowY = (i: number) => GATE.y + 50 + i * 28;
const PRE = 2; // BeforeTool row
const RAIL_Y = rowY(PRE);

const CARD = { w: 198, h: 32 };
const CARD_X = POD.x + 16;
const CARD_Y = RAIL_Y - CARD.h / 2;

/* Beat boundaries for the three cycles */
const CALL_1 = {
  prep: [0.02, 0.07],
  call: [0.07, 0.14],
  post: [0.15, 0.22],
  back: [0.23, 0.29],
  done: 0.30,
};
const CALL_2 = {
  prep: [0.33, 0.38],
  call: [0.38, 0.45],
  post: [0.46, 0.53],
  back: [0.54, 0.60],
  done: 0.61,
};
const CALL_3 = {
  prep: [0.64, 0.69],
  call: [0.69, 0.75],
  post: [0.76, 0.81],
  back: [0.82, 0.86],
  review: [0.86, 0.93],
  vouch: [0.93, 0.96],
  auth: [0.96, 0.98],
  done: 0.98,
};

function drawLogo(ctx: CanvasRenderingContext2D, th: Theme, x: number, y: number, capPx: number) {
  const s = capPx / LOGO.capHeight;
  const size = LOGO.cell * s + 0.4;
  for (const p of LOGO.pixels) {
    ctx.fillStyle = p.dim ? th.textWeak : th.textStrong;
    ctx.fillRect(x + p.x * LOGO.cell * s, y + p.y * LOGO.cell * s, size, size);
  }
}

function renderCallCycle(
  ctx: CanvasRenderingContext2D,
  th: Theme,
  t: number,
  beat: typeof CALL_1,
  toolLabel: string,
  toolColor: string,
  answerText: string,
  answerColor: string,
  answerBg: string,
) {
  // 1. Tool card inside POD
  const appear = ease(seg(t, beat.prep[0], beat.prep[1]));
  const fade = seg(t, beat.done + 0.02, beat.done + 0.05);
  if (t >= beat.prep[0] && fade < 1) {
    drawCard(ctx, th, CARD_X, CARD_Y, CARD.w, CARD.h, toolLabel, toolColor, Math.min(appear, 1 - fade));
  }

  // 2. Dispatch chip from POD to GATE
  const callF = seg(t, beat.call[0], beat.call[1]);
  if (callF > 0 && callF < 1) {
    const chipX = lerp(POD.x + POD.w, GATE.x, ease(callF));
    chip(ctx, chipX, RAIL_Y, "tool_call", th.textStrong, th.bgWeak, th.mono);
  }

  // 3. POST /hook from GATE to RUNTIME
  const postF = seg(t, beat.post[0], beat.post[1]);
  if (postF > 0 && postF < 1) {
    const chipX = lerp(GATE.x + GATE.w, RUNTIME.x, ease(postF));
    chip(ctx, chipX, RAIL_Y, "POST /hook", th.textStrong, th.bgWeak, th.mono);
  }

  // 4. Answer chip from RUNTIME back to GATE
  const backF = seg(t, beat.back[0], beat.back[1]);
  if (backF > 0 && backF < 1) {
    const chipX = lerp(RUNTIME.x, GATE.x + GATE.w, ease(backF));
    chip(ctx, chipX, RAIL_Y, answerText, answerColor, answerBg, th.mono);
  }
}

function draw(ctx: CanvasRenderingContext2D, t: number, th: Theme) {
  const font = (size: number, weight = 400) => `${weight} ${size}px ${th.mono}`;

  const runtimeBusy =
    (t > CALL_1.post[0] && t < CALL_1.back[1]) ||
    (t > CALL_2.post[0] && t < CALL_2.back[1]) ||
    (t > CALL_3.post[0] && t < CALL_3.back[1]) ||
    (t > CALL_3.vouch[0] && t < CALL_3.auth[1]);

  const isBlocked = t >= CALL_2.done && t < CALL_3.prep[0];
  const isUnderReview = t >= CALL_3.back[1] && t < CALL_3.auth[1];
  const isApproved = t >= CALL_3.done;

  /* The connecting rails: Pod -> Gate -> Runtime */
  ctx.strokeStyle = th.borderWeak;
  ctx.lineWidth = 1.5;
  ctx.beginPath();
  // Rail 1: POD to GATE
  ctx.moveTo(POD.x + POD.w, RAIL_Y);
  ctx.lineTo(GATE.x, RAIL_Y);
  // Rail 2: GATE to RUNTIME
  ctx.moveTo(GATE.x + GATE.w, RAIL_Y);
  ctx.lineTo(RUNTIME.x, RAIL_Y);
  ctx.stroke();

  /* 1. kagent Agent Pod */
  ctx.fillStyle = th.bg;
  ctx.strokeStyle = th.border;
  roundRect(ctx, POD.x, POD.y, POD.w, POD.h, 6);
  ctx.fill();
  ctx.stroke();

  ctx.fillStyle = th.textStrong;
  ctx.font = font(13, 600);
  ctx.textAlign = "left";
  ctx.textBaseline = "middle";
  ctx.fillText("kagent Agent Pod", POD.x + 14, POD.y + 20);
  ctx.fillStyle = th.textWeak;
  ctx.font = font(11.5);
  ctx.fillText("cluster-ops · declarative Agent", POD.x + 14, POD.y + 40);
  ctx.fillText("Google ADK runtime in k8s", POD.x + 14, POD.y + POD.h - 18);

  /* 2. ADK Plugin Gate (AppaPluginKagent) */
  ctx.save();
  ctx.strokeStyle = isBlocked ? th.danger : isUnderReview ? th.warn : isApproved ? th.accent : th.border;
  ctx.setLineDash([3, 5]);
  if (isBlocked) {
    ctx.shadowColor = th.danger;
    ctx.shadowBlur = 8;
  } else if (isUnderReview) {
    ctx.shadowColor = th.warn;
    ctx.shadowBlur = 8;
  } else if (isApproved) {
    ctx.shadowColor = th.accent;
    ctx.shadowBlur = 8;
  }
  roundRect(ctx, GATE.x, GATE.y, GATE.w, GATE.h, 6);
  ctx.stroke();
  ctx.restore();

  ctx.fillStyle = th.textStrong;
  ctx.font = font(13, 600);
  ctx.fillText("AppaPluginKagent", GATE.x + 14, GATE.y + 20);
  HOOK_ROWS.forEach((name, i) => {
    const active = i === PRE && (t > CALL_1.call[0] || t > CALL_2.call[0] || t > CALL_3.call[0]);
    ctx.fillStyle = active ? th.accent : th.textWeak;
    ctx.font = font(11.5, active ? 600 : 400);
    ctx.fillText(name, GATE.x + 14, rowY(i));
  });

  /* 3. OpenAPPA Runtime */
  ctx.save();
  if (runtimeBusy) {
    ctx.shadowColor = th.accent;
    ctx.shadowBlur = 10;
    ctx.strokeStyle = th.accent;
  } else {
    ctx.strokeStyle = th.border;
  }
  ctx.fillStyle = th.bg;
  roundRect(ctx, RUNTIME.x, RUNTIME.y, RUNTIME.w, RUNTIME.h, 6);
  ctx.fill();
  ctx.stroke();
  ctx.restore();

  drawLogo(ctx, th, RUNTIME.x + 14, RUNTIME.y + 13, 10);
  ctx.fillStyle = th.textStrong;
  ctx.font = font(13, 600);
  ctx.fillText("OpenAPPA Runtime", RUNTIME.x + 14, RUNTIME.y + 46);
  ["Policy Contract (appa.toml)", "Deterministic Engine", "Trajectory Labels & Log"].forEach((line, i) => {
    drawCard(ctx, th, RUNTIME.x + 14, RUNTIME.y + 66 + i * 44, RUNTIME.w - 28, 36, line, th.border);
  });

  /* Calls 1 and 2 */
  renderCallCycle(ctx, th, t, CALL_1, "read_secret(db)", th.accent, "allow", th.accent, th.accentBg);
  renderCallCycle(ctx, th, t, CALL_2, "post_status(leak)", th.danger, "block", th.danger, th.dangerBg);

  /* Call 3: Full Human-in-the-Loop lifecycle */
  // 3a. Initial tool proposal
  const c3Appear = ease(seg(t, CALL_3.prep[0], CALL_3.prep[1]));
  const c3Fade = seg(t, 0.99, 1.0);
  if (t >= CALL_3.prep[0] && c3Fade < 1) {
    drawCard(
      ctx,
      th,
      CARD_X,
      CARD_Y,
      CARD.w,
      CARD.h,
      isApproved ? "restart_deployment ✓" : "restart_deployment",
      isApproved ? th.accent : th.warn,
      Math.min(c3Appear, 1 - c3Fade),
    );
  }
  // 3b. Dispatch chip
  const c3CallF = seg(t, CALL_3.call[0], CALL_3.call[1]);
  if (c3CallF > 0 && c3CallF < 1) {
    const chipX = lerp(POD.x + POD.w, GATE.x, ease(c3CallF));
    chip(ctx, chipX, RAIL_Y, "tool_call", th.textStrong, th.bgWeak, th.mono);
  }
  // 3c. Initial policy check POST /hook
  const c3PostF = seg(t, CALL_3.post[0], CALL_3.post[1]);
  if (c3PostF > 0 && c3PostF < 1) {
    const chipX = lerp(GATE.x + GATE.w, RUNTIME.x, ease(c3PostF));
    chip(ctx, chipX, RAIL_Y, "POST /hook", th.textStrong, th.bgWeak, th.mono);
  }
  // 3d. Engine returns remedy offer citing oncall authority
  const c3BackF = seg(t, CALL_3.back[0], CALL_3.back[1]);
  if (c3BackF > 0 && c3BackF < 1) {
    const chipX = lerp(RUNTIME.x, GATE.x + GATE.w, ease(c3BackF));
    chip(ctx, chipX, RAIL_Y, "remedy: oncall", th.warn, th.warnBg, th.mono);
  }

  // 3e. kagent UI Approval Card popup at Gate
  const reviewProgress = seg(t, CALL_3.review[0], CALL_3.review[1]);
  if (reviewProgress > 0 && t < CALL_3.auth[1]) {
    const cardAlpha = reviewProgress < 0.2 ? ease(reviewProgress * 5) : t > CALL_3.vouch[1] ? 1 - ease(seg(t, CALL_3.vouch[1], CALL_3.auth[1])) : 1;
    const isClicked = t >= 0.90;

    ctx.save();
    ctx.globalAlpha = cardAlpha;
    const POP_X = GATE.x + 12;
    const POP_Y = GATE.y + 140;
    const POP_W = GATE.w - 24;
    const POP_H = 80;

    ctx.fillStyle = th.bgWeak;
    ctx.strokeStyle = isClicked ? th.accent : th.warn;
    ctx.lineWidth = 1.5;
    roundRect(ctx, POP_X, POP_Y, POP_W, POP_H, 5);
    ctx.fill();
    ctx.stroke();

    ctx.fillStyle = th.textStrong;
    ctx.font = font(11.5, 600);
    ctx.fillText("kagent Confirmation", POP_X + 12, POP_Y + 16);

    ctx.fillStyle = th.textWeak;
    ctx.font = font(10.5);
    ctx.fillText("restart_deployment(api)", POP_X + 12, POP_Y + 34);

    // Approve button
    const BTN_Y = POP_Y + 48;
    const BTN_W = 86;
    const BTN_H = 22;
    ctx.fillStyle = isClicked ? th.accent : th.bg;
    ctx.strokeStyle = th.accent;
    roundRect(ctx, POP_X + 12, BTN_Y, BTN_W, BTN_H, 3);
    ctx.fill();
    ctx.stroke();
    ctx.fillStyle = isClicked ? th.bg : th.accent;
    ctx.font = font(10.5, 700);
    ctx.fillText("Approve", POP_X + 28, BTN_Y + 11);

    // Reject button
    ctx.fillStyle = th.bg;
    ctx.strokeStyle = th.border;
    roundRect(ctx, POP_X + 106, BTN_Y, BTN_W - 10, BTN_H, 3);
    ctx.fill();
    ctx.stroke();
    ctx.fillStyle = th.textWeak;
    ctx.font = font(10.5);
    ctx.fillText("Reject", POP_X + 122, BTN_Y + 11);

    ctx.restore();
  }

  // 3f. Vouch approval sent to Runtime
  const vouchF = seg(t, CALL_3.vouch[0], CALL_3.vouch[1]);
  if (vouchF > 0 && vouchF < 1) {
    const chipX = lerp(GATE.x + GATE.w, RUNTIME.x, ease(vouchF));
    chip(ctx, chipX, RAIL_Y, "ruling: approve", th.accent, th.accentBg, th.mono);
  }

  // 3g. Authorized response back to Gate
  const authF = seg(t, CALL_3.auth[0], CALL_3.auth[1]);
  if (authF > 0 && authF < 1) {
    const chipX = lerp(RUNTIME.x, GATE.x + GATE.w, ease(authF));
    chip(ctx, chipX, RAIL_Y, "authorized", th.accent, th.accentBg, th.mono);
  }

  /* Verdict markers at the gate */
  // Call 1: Allowed checkmark
  if (t >= CALL_1.done && t < CALL_2.prep[0]) {
    ctx.fillStyle = th.accent;
    ctx.font = font(16, 700);
    ctx.textAlign = "center";
    ctx.fillText("✓", GATE.x + GATE.w + 14, RAIL_Y - 14);
    ctx.textAlign = "left";
  }
  // Call 2: Blocked cross
  if (isBlocked) {
    const pulse = 1 + 0.15 * Math.sin(t * 70);
    ctx.save();
    ctx.fillStyle = th.dangerBg;
    ctx.strokeStyle = th.danger;
    ctx.beginPath();
    ctx.arc(GATE.x + GATE.w + 14, RAIL_Y, 10 * pulse, 0, Math.PI * 2);
    ctx.fill();
    ctx.stroke();
    ctx.fillStyle = th.danger;
    ctx.font = font(12, 700);
    ctx.textAlign = "center";
    ctx.fillText("✕", GATE.x + GATE.w + 14, RAIL_Y + 0.5);
    ctx.restore();
    ctx.textAlign = "left";
  }
  // Call 3: Review pause -> Approved checkmark
  if (isUnderReview) {
    ctx.save();
    ctx.fillStyle = th.warnBg;
    ctx.strokeStyle = th.warn;
    ctx.beginPath();
    ctx.arc(GATE.x + GATE.w + 14, RAIL_Y, 10, 0, Math.PI * 2);
    ctx.fill();
    ctx.stroke();
    ctx.fillStyle = th.warn;
    ctx.font = font(11, 700);
    ctx.textAlign = "center";
    ctx.fillText("⏸", GATE.x + GATE.w + 14, RAIL_Y + 0.5);
    ctx.restore();
    ctx.textAlign = "left";
  }
  if (isApproved) {
    ctx.save();
    ctx.fillStyle = th.accentBg;
    ctx.strokeStyle = th.accent;
    ctx.beginPath();
    ctx.arc(GATE.x + GATE.w + 14, RAIL_Y, 10, 0, Math.PI * 2);
    ctx.fill();
    ctx.stroke();
    ctx.fillStyle = th.accent;
    ctx.font = font(12, 700);
    ctx.textAlign = "center";
    ctx.fillText("✓", GATE.x + GATE.w + 14, RAIL_Y + 0.5);
    ctx.restore();
    ctx.textAlign = "left";
  }

  /* Bottom status notes */
  ctx.font = font(12.5);
  ctx.textAlign = "left";
  ctx.textBaseline = "middle";

  const notesList = [
    { at: CALL_1.done, text: "✓ allowed — secret read narrows session audience to [ops]", color: "accent" as const },
    { at: CALL_2.done, text: "✕ blocked — ops data cannot leak into public sink post_status_update", color: "danger" as const },
    {
      at: CALL_3.back[1],
      text: isApproved
        ? "✓ approved & executed — operator confirmed in kagent UI; deployment restarted"
        : "⏸ review — restart_deployment suspends until operator approval in kagent UI",
      color: isApproved ? ("accent" as const) : ("warn" as const),
    },
  ];

  notesList.forEach((note, i) => {
    const lineF = seg(t, note.at, note.at + 0.03);
    if (lineF <= 0) return;
    ctx.save();
    ctx.globalAlpha = ease(lineF);
    ctx.fillStyle = th[note.color];
    ctx.fillText(note.text, POD.x, 315 + i * 24);
    ctx.restore();
  });
}

export function KagentFigure() {
  return <Figure draw={draw} designW={W} designH={H} durationMs={16000} />;
}

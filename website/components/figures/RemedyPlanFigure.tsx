"use client";

import { Figure } from "@/components/figures/Figure";
import { chip, drawCard, ease, roundRect, seg, type Theme } from "@/components/figures/lib";

/* Figure: Engine Refusals Enumerate Every Sound Remedy
   When a call is blocked due to a policy gap, OpenAPPA doesn't stop the agent.
   It returns structured remedy plans: Approval, Sanitizer, or Narrowing Acceptance. */

const W = 900;
const H = 380;

const PROPOSED_BOX = { x: 30, y: 120, w: 210, h: 140 };
const ENGINE_BOX = { x: 280, y: 120, w: 190, h: 140 };
const REMEDIES_X = 520;
const REMEDY_BOXES = [
  { title: "Plan 1: Policy Approval", detail: "Authority: user", colorKey: "accent" as const, y: 50 },
  { title: "Plan 2: Sanitizer Redact", detail: "Transform: remove_pii", colorKey: "info" as const, y: 150 },
  { title: "Plan 3: Accept Narrowing", detail: "Acknowledge reach reduction", colorKey: "warn" as const, y: 250 },
];
const REMEDY_W = 340;
const REMEDY_H = 80;

export function RemedyPlanFigure() {
  return (
    <Figure
      designW={W}
      designH={H}
      durationMs={16000}
      draw={(ctx: CanvasRenderingContext2D, t: number, th: Theme) => {
        const font = (size: number, weight = 400) => `${weight} ${size}px ${th.mono}`;

        // Background connectors
        ctx.strokeStyle = th.borderWeak;
        ctx.lineWidth = 1.5;

        // Arrow 1: Proposed Call -> Engine
        const arrow1 = ease(seg(t, 0.1, 0.25));
        if (arrow1 > 0) {
          ctx.save();
          ctx.globalAlpha = arrow1;
          ctx.beginPath();
          ctx.moveTo(PROPOSED_BOX.x + PROPOSED_BOX.w, PROPOSED_BOX.y + PROPOSED_BOX.h / 2);
          ctx.lineTo(ENGINE_BOX.x, ENGINE_BOX.y + ENGINE_BOX.h / 2);
          ctx.stroke();
          ctx.restore();
        }

        // Arrow 2: Engine -> Remedy Plans
        const arrow2 = ease(seg(t, 0.45, 0.6));
        if (arrow2 > 0) {
          ctx.save();
          ctx.globalAlpha = arrow2;
          REMEDY_BOXES.forEach((box) => {
            ctx.beginPath();
            ctx.moveTo(ENGINE_BOX.x + ENGINE_BOX.w, ENGINE_BOX.y + ENGINE_BOX.h / 2);
            ctx.lineTo(REMEDIES_X, box.y + REMEDY_H / 2);
            ctx.stroke();
          });
          ctx.restore();
        }

        // 1. Proposed Tool Call Card
        const showProposed = ease(seg(t, 0.02, 0.15));
        if (showProposed > 0) {
          ctx.save();
          ctx.globalAlpha = showProposed;
          ctx.fillStyle = th.bg;
          ctx.strokeStyle = th.border;
          roundRect(ctx, PROPOSED_BOX.x, PROPOSED_BOX.y, PROPOSED_BOX.w, PROPOSED_BOX.h, 6);
          ctx.fill();
          ctx.stroke();

          ctx.fillStyle = th.textStrong;
          ctx.font = font(12, 600);
          ctx.textAlign = "left";
          ctx.fillText("Proposed Action", PROPOSED_BOX.x + 14, PROPOSED_BOX.y + 24);

          ctx.fillStyle = th.textWeak;
          ctx.font = font(11, 400);
          ctx.fillText("file_github_issue(title, body)", PROPOSED_BOX.x + 14, PROPOSED_BOX.y + 48);

          chip(ctx, PROPOSED_BOX.x + 14, PROPOSED_BOX.y + 70, "label: private", th.warn, th.warnBg, th.mono);
          chip(ctx, PROPOSED_BOX.x + 14, PROPOSED_BOX.y + 100, "target: public repo", th.accent, th.accentBg, th.mono);
          ctx.restore();
        }

        // 2. OpenAPPA Engine Check
        const showEngine = ease(seg(t, 0.25, 0.45));
        if (showEngine > 0) {
          ctx.save();
          ctx.globalAlpha = showEngine;
          ctx.fillStyle = th.bg;
          ctx.strokeStyle = th.danger;
          ctx.lineWidth = 1.5;
          roundRect(ctx, ENGINE_BOX.x, ENGINE_BOX.y, ENGINE_BOX.w, ENGINE_BOX.h, 6);
          ctx.fill();
          ctx.stroke();

          ctx.fillStyle = th.danger;
          ctx.font = font(12, 600);
          ctx.textAlign = "left";
          ctx.fillText("Engine Verdict", ENGINE_BOX.x + 14, ENGINE_BOX.y + 24);

          ctx.fillStyle = th.textStrong;
          ctx.font = font(11, 500);
          ctx.fillText("outcome: block", ENGINE_BOX.x + 14, ENGINE_BOX.y + 48);

          ctx.fillStyle = th.textWeak;
          ctx.font = font(10, 400);
          ctx.fillText("Requirement Gap:", ENGINE_BOX.x + 14, ENGINE_BOX.y + 74);
          ctx.fillText("public ⊄ private", ENGINE_BOX.x + 14, ENGINE_BOX.y + 92);

          chip(ctx, ENGINE_BOX.x + 14, ENGINE_BOX.y + 110, "remedy_plans: [3]", th.accent, th.accentBg, th.mono);
          ctx.restore();
        }

        // 3. Enumerated Remedy Plans
        REMEDY_BOXES.forEach((box, i) => {
          const showBox = ease(seg(t, 0.5 + i * 0.1, 0.65 + i * 0.1));
          if (showBox <= 0) return;

          const isHighlighted = t >= 0.85 && i === 0;

          ctx.save();
          ctx.globalAlpha = showBox;
          ctx.fillStyle = isHighlighted ? th.accentBg : th.bg;
          ctx.strokeStyle = isHighlighted ? th.accent : th.border;
          ctx.lineWidth = isHighlighted ? 2 : 1;

          roundRect(ctx, REMEDIES_X, box.y, REMEDY_W, REMEDY_H, 6);
          ctx.fill();
          ctx.stroke();

          ctx.fillStyle = isHighlighted ? th.accent : th[box.colorKey];
          ctx.font = font(12, 600);
          ctx.textAlign = "left";
          ctx.fillText(box.title, REMEDIES_X + 16, box.y + 28);

          ctx.fillStyle = th.textStrong;
          ctx.font = font(11, 400);
          ctx.fillText(box.detail, REMEDIES_X + 16, box.y + 52);

          if (isHighlighted) {
            chip(ctx, REMEDIES_X + REMEDY_W - 100, box.y + 20, "Executable", th.accent, th.accentBg, th.mono);
          }

          ctx.restore();
        });

        // Step Label / Caption at top
        ctx.fillStyle = th.textStrong;
        ctx.font = font(13, 500);
        ctx.textAlign = "center";
        let caption = "1. Agent proposes a public issue carrying private CRM data";
        if (t >= 0.25 && t < 0.5) caption = "2. OpenAPPA intercepts and identifies the policy gap";
        else if (t >= 0.5 && t < 0.85) caption = "3. Refusal object enumerates all sound remedy plans";
        else if (t >= 0.85) caption = "4. Agent executes policy approval to safely unblock dispatch";

        ctx.fillText(caption, W / 2, 24);
      }}
    />
  );
}

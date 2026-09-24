import { type RefObject, useEffect } from "react";

import { songPosition, subscribeSong } from "@/lib/song";

/* A sailor's step dance, one step per beat over an eight-beat figure: three
   steps away from the wordmark, a hop and a turn on the fourth beat, three
   steps back, and a turn on the eighth to face the way it came. The pose is
   computed every frame from the audio clock rather than played as a CSS
   animation: the song's tempo drifts, so a fixed-period animation would be
   a whole beat out by the second chorus.
   Amounts are at full intensity, as a share of the mascot's own box. */
const FIGURE_BEATS = 8;
const TRAVEL = 90; // percent of width the figure walks out, to the left
const TRAVEL_NARROW = 45; // with the menu button in the way (see .nav-toggle)
const STEP = 7; // percent of height each step rises
const TURN_HOP = 1.6; // the turning hop, as a multiple of a step
const SQUASH = 0.08; // scaleY lost on landing, gained back as width
const LANDING_WIDTH = 0.18; // share of a beat the landing squash spans
const KICK = 1.2; // grid pixels the stepping legs lift
/* The song starts quiet and ends loud; the steps only get a little higher. */
const FLOOR = 0.6;

interface Pose {
  body: string;
  legsLeft: string;
  legsRight: string;
}

function smoothstep(v: number): number {
  return v * v * (3 - 2 * v);
}

/** Where the figure stands at the start of a beat: 0 by the wordmark, 1 fully out. */
function station(beatInFigure: number): number {
  return beatInFigure <= 3 ? beatInFigure / 3 : (7 - beatInFigure) / 3;
}

function pose(t: number, travel: number): Pose | null {
  const at = songPosition(t);
  if (!at || at.intensity <= 0) return null;
  const k = FLOOR + (1 - FLOOR) * at.intensity;
  const { phase, beat } = at;
  const inFigure = beat % FIGURE_BEATS;
  const turning = inFigure === 3 || inFigure === 7;
  const outbound = inFigure < 4;
  const air = Math.sin(Math.PI * phase);
  const nearGround = Math.min(phase, 1 - phase) / LANDING_WIDTH;
  const landing = Math.exp(-nearGround * nearGround);
  // Each step carries the body from one station to the next while it is in
  // the air; the turn happens on the spot.
  const from = station(inFigure);
  const to = turning ? from : station(inFigure + 1);
  const x = -travel * (from + (to - from) * smoothstep(phase));
  const y = -STEP * k * (turning ? TURN_HOP : 1) * air;
  // The turn: a spin about the vertical axis, thin at the middle of the beat.
  const spin = turning ? Math.max(Math.abs(Math.cos(Math.PI * phase)), 0.12) : 1;
  const sy = 1 - SQUASH * k * landing;
  const sx = spin * (1 + SQUASH * k * landing);
  // The trailing pair of legs lifts to make the step; both hop on a turn.
  const lift = KICK * k * air;
  const liftLeft = turning || !outbound ? lift : 0.3 * lift;
  const liftRight = turning || outbound ? lift : 0.3 * lift;
  return {
    body: `translate(${x.toFixed(2)}%, ${y.toFixed(2)}%) scale(${sx.toFixed(3)}, ${sy.toFixed(3)})`,
    legsLeft: `translate(0px, ${(-liftLeft).toFixed(2)}px)`,
    legsRight: `translate(0px, ${(-liftRight).toFixed(2)}px)`,
  };
}

/** Makes the mascot inside `root` dance while the OpenAPPA song plays. */
export function useMascotDance(root: RefObject<HTMLElement | null>) {
  useEffect(() => {
    const mark = root.current?.querySelector<SVGSVGElement>(".logo-mark");
    const body = mark?.querySelector<SVGGElement>(".appa-mark-float");
    const legsLeft = mark?.querySelector<SVGGElement>(".appa-mark-legs-left");
    const legsRight = mark?.querySelector<SVGGElement>(".appa-mark-legs-right");
    if (!mark || !body || !legsLeft || !legsRight) return;
    if (window.matchMedia("(prefers-reduced-motion: reduce)").matches) return;

    const apply = (p: Pose | null) => {
      // The float animation is off while dancing, so an empty transform is
      // the mascot standing still between the intro and the first beat.
      body.style.transform = p?.body ?? "";
      legsLeft.style.transform = p?.legsLeft ?? "";
      legsRight.style.transform = p?.legsRight ?? "";
    };
    let frame = 0;
    const stop = () => {
      cancelAnimationFrame(frame);
      frame = 0;
      mark.classList.remove("is-dancing");
      apply(null);
    };
    const unsubscribe = subscribeSong((audio) => {
      stop();
      if (!audio) return;
      mark.classList.add("is-dancing");
      const narrow = window.matchMedia("(max-width: 1023px)");
      const tick = () => {
        apply(pose(audio.currentTime, narrow.matches ? TRAVEL_NARROW : TRAVEL));
        frame = requestAnimationFrame(tick);
      };
      tick();
    });
    return () => {
      unsubscribe();
      stop();
    };
  }, [root]);
}

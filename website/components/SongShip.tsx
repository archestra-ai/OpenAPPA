"use client";

import { useEffect, useRef, useState } from "react";

import { songPosition, subscribeSong } from "@/lib/song";

/* The "Check the Flow" music video, playing in the middle of the page while
   the song does: a pixel ship at night with a crew of appas in hats dancing
   on the deck, the captain in the tricorn. The sail is the screen: every
   lyric is drawn on it as a small diagram, so the song stays a literal
   explanation of the product. Like the header mascot's dance, every pose is
   computed from the audio clock on each frame with the video's own
   choreography and timings. The scene is one SVG so it scales with the
   viewport; the sail's slides are HTML inside a foreignObject so the video's
   markup carries over as is. */

const STAGE = 1080;
/* The night fades out around the frame in pixel steps rather than a blur. */
const FRAME_STEPS = 8;
const FRAME_STEP = 14;
const FRAME = FRAME_STEPS * FRAME_STEP;
/* The ship pivots about a point below the waterline so it rocks, not spins. */
const PIVOT_X = 540;
const PIVOT_Y = 900;
/* The song starts quiet and ends loud; the swell follows. */
const FLOOR = 0.6;
const PORTHOLES = [0, 1, 2, 3, 4, 5, 6];
const WAVES: ReadonlyArray<{ y: number; color: string; dir: 1 | -1; step: number }> = [
  { y: 832, color: "#284866", dir: 1, step: 12 },
  { y: 850, color: "#1a3048", dir: -1, step: 18 },
  { y: 866, color: "#0f1e2e", dir: 1, step: 24 },
];
const SEA_Y = 896;

/* ---------- helpers ---------- */

const clamp = (x: number, a = 0, b = 1) => Math.max(a, Math.min(b, x));
const seg = (t: number, a: number, b: number) => clamp((t - a) / (b - a));
const easeOut = (x: number) => 1 - Math.pow(1 - x, 3);
const easeIn = (x: number) => x * x * x;
const smooth = (x: number) => x * x * (3 - 2 * x);
const back = (x: number) => 1 + 2.70158 * Math.pow(x - 1, 3) + 1.70158 * Math.pow(x - 1, 2);
const frac = (x: number) => ((x % 1) + 1) % 1;

/* Where the song is, in the video's terms: `b` is a fractional beat count
   and `k` the loudness-scaled amount of every move. */
interface Beat {
  beat: number;
  ph: number;
  b: number;
  inBar: number;
  k: number;
}

function pos(t: number): Beat {
  const at = songPosition(t);
  if (!at) return { beat: 0, ph: 0, b: 0, inBar: 0, k: FLOOR };
  return { beat: at.beat, ph: at.phase, b: at.beat + at.phase, inBar: at.beatInBar, k: FLOOR + (1 - FLOOR) * at.intensity };
}

/* ---------- mascot ---------- */

type Hat = "party" | "tricorn" | "bandana" | "beanie" | "stripe";

/* Pixel hats, in the mascot's 24x22 grid. */
const HATS: Record<Hat, string> = {
  party: `<rect x="9" y="1" width="6" height="1" fill="#4a7edd"/><rect x="9.5" y="0" width="5" height="1" fill="#4a7edd"/><rect x="10" y="-1" width="4" height="1" fill="#4a7edd"/><rect x="10.5" y="-2" width="3" height="1" fill="#4a7edd"/><rect x="11" y="-3" width="2" height="1" fill="#4a7edd"/><rect x="11.25" y="-4" width="1.5" height="1" fill="#4a7edd"/><rect x="10" y="1" width="0.75" height="0.75" fill="#f2c94c"/><rect x="13" y="1.25" width="0.75" height="0.75" fill="#f2c94c"/><rect x="11.5" y="0.25" width="0.75" height="0.75" fill="#f2c94c"/><rect x="10.5" y="-0.75" width="0.75" height="0.75" fill="#f2c94c"/><rect x="12.75" y="-1" width="0.75" height="0.75" fill="#f2c94c"/><rect x="11.5" y="-2" width="0.75" height="0.75" fill="#f2c94c"/><rect x="11" y="-5.5" width="2" height="1.5" fill="#e0524c"/>`,
  tricorn: `<rect x="1" y="1" width="22" height="2" fill="#2f56a6"/><rect x="0" y="0" width="3" height="2" fill="#2f56a6"/><rect x="21" y="0" width="3" height="2" fill="#2f56a6"/><rect x="5" y="-3" width="14" height="4" fill="#4a7edd"/><rect x="8" y="-4" width="8" height="1" fill="#4a7edd"/><rect x="1" y="2.25" width="22" height="0.75" fill="#f2c94c"/><rect x="10.5" y="-2.25" width="3" height="2" fill="#f2e5c9"/><rect x="11" y="-1.5" width="0.75" height="0.75" fill="#211f1c"/><rect x="12.25" y="-1.5" width="0.75" height="0.75" fill="#211f1c"/>`,
  bandana: `<rect x="4" y="2" width="16" height="1" fill="#e0524c"/><rect x="3" y="3" width="18" height="1.5" fill="#e0524c"/><rect x="21" y="3" width="2" height="1.5" fill="#e0524c"/><rect x="22" y="4.5" width="1.5" height="2" fill="#e0524c"/><rect x="7" y="2.75" width="1" height="1" fill="#f2e5c9"/><rect x="12" y="3" width="1" height="1" fill="#f2e5c9"/><rect x="17" y="2.5" width="1" height="1" fill="#f2e5c9"/>`,
  beanie: `<rect x="4" y="1" width="16" height="2" fill="#2f9b68"/><rect x="5" y="-1" width="14" height="2" fill="#7fd8a8"/><rect x="7" y="-2.5" width="10" height="1.5" fill="#7fd8a8"/><rect x="11" y="-4" width="2" height="1.5" fill="#f2e5c9"/>`,
  stripe: `<rect x="4" y="2" width="16" height="1" fill="#f2c94c"/><rect x="3" y="3" width="18" height="1.5" fill="#f2c94c"/><rect x="-1" y="3" width="4" height="1.5" fill="#f2c94c"/><rect x="-1.5" y="4.5" width="1.5" height="2" fill="#f2c94c"/>`,
};

const mascot = (size: number, tone: "dark" | "cream" = "dark", hat?: Hat) => `
<svg class="mascot ${tone}" viewBox="0 0 24 22" width="${size}" height="${(size * 22) / 24}" shape-rendering="crispEdges">
  <g class="legL"><path class="body" d="M0 20h5v1H0zM7 20h4v1H7z"/><path class="dim" d="M0 21h5v1H0zM7 21h4v1H7z"/></g>
  <g class="legR"><path class="body" d="M13 20h4v1H13zM19 20h5v1H19z"/><path class="dim" d="M13 21h4v1H13zM19 21h5v1H19z"/></g>
  <path class="body" d="M5 0h2v2H5zM17 0h2v2H17zM4 2h16v1H4zM3 3h18v7H3zM3 10h7v2H3zM14 10h7v2H14zM3 12h18v1H3zM4 13h16v1H4zM1 14h22v1H1zM0 15h24v5H0z"/>
  <path class="dim" d="M10 10h4v1H10zM10 11h1v1H10zM13 11h1v1H13z"/>
  <path class="eye" d="M11 11h2v1H11z"/>
  <g class="eyes"><rect class="eye" x="6" y="6" width="3" height="3"/><rect class="eye" x="15" y="6" width="3" height="3"/></g>
  ${hat ? HATS[hat] : ""}
</svg>`;

function eyes(scope: Element, open = 1, look = 0, winkRight = 1) {
  const [l, r] = scope.querySelectorAll<SVGRectElement>(".eyes rect");
  if (!l || !r) return;
  l.style.transform = `translate(${look}px, 0px) scaleY(${open})`;
  r.style.transform = `translate(${look}px, 0px) scaleY(${open * winkRight})`;
}

function blinkAt(t: number, seed: number): number {
  const p = frac((t + seed * 1.37) / (3.1 + (seed % 3) * 0.7));
  return p > 0.96 ? 0.1 : 1;
}

/* ---------- crew ---------- */

const MASCOT_W = 120;
const MASCOT_H = 110;
const DECK_Y = 660;
const SHADOW_Y = 768;
const slotX = (i: number) => 126 + i * 138;
const CREW_HATS: ReadonlyArray<Hat> = ["bandana", "party", "beanie", "tricorn", "stripe", "party", "bandana"];

/* When each deck slot is manned: the captain (slot 3) from the drop, then
   the crew grows with the choruses and thins out for the verses. */
const PRESENT: Record<number, ReadonlyArray<readonly [number, number]>> = {
  3: [[0.5, 999]],
  2: [[2.9, 999]],
  4: [[3.1, 999]],
  1: [[17.7, 34.2], [58.0, 999]],
  5: [[17.9, 34.4], [58.2, 999]],
  0: [[58.3, 74.3], [98.2, 999]],
  6: [[58.5, 74.5], [98.4, 999]],
};

type Move = "idle" | "sway" | "step" | "stomp" | "kick" | "call" | "heave" | "wave" | "jump" | "bow";

function moveAt(t: number): Move {
  if (t < 3.3) return "idle";
  if (t < 11.6) return "sway";
  if (t < 18.6) return "step";
  if (t < 26.6) return "stomp";
  if (t < 33.9) return "kick";
  if (t < 41.7) return "sway";
  if (t < 49.3) return "step";
  if (t < 58.9) return "sway";
  if (t < 66.9) return "call";
  if (t < 74.2) return "kick";
  if (t < 88.85) return "heave";
  if (t < 99.1) return "wave";
  if (t < 107.6) return "jump";
  if (t < 111.6) return "wave";
  if (t < 117.4) return "call";
  if (t < 123.2) return "jump";
  return "bow";
}

interface Figure {
  x: number;
  y: number;
  rot: number;
  sx: number;
  sy: number;
  l: number;
  r: number;
  look: number;
  open: number;
}

function dance(move: Move, i: number, P: Beat, t: number): Figure {
  const { ph, beat, b, k } = P;
  const air = Math.sin(Math.PI * ph);
  const near = Math.min(ph, 1 - ph) / 0.18;
  const land = Math.exp(-near * near);
  const alt = (beat + i) % 2 === 0;
  const o: Figure = { x: 0, y: 0, rot: 0, sx: 1, sy: 1, l: 0, r: 0, look: 0, open: 1 };
  const squash = (amt: number) => {
    o.sy = 1 - amt * k * land;
    o.sx = 1 + amt * k * land;
  };
  const thin = () => Math.max(Math.abs(Math.cos(Math.PI * ph)), 0.12);
  if (move === "sway") {
    o.rot = 5 * k * Math.sin(Math.PI * b);
    o.y = -8 * k * air;
    o.l = alt ? 0.9 * air : 0;
    o.r = alt ? 0 : 0.9 * air;
    o.look = Math.sin(Math.PI * b) > 0 ? 0.5 : -0.5;
  } else if (move === "step") {
    // the site header's sailor step: three steps out, a turning hop, three back, turn
    const f = beat % 8;
    const turning = f === 3 || f === 7;
    const out = f < 4;
    const st = (n: number) => (n <= 3 ? n / 3 : (7 - n) / 3);
    const from = st(f);
    const to = turning ? from : st(f + 1);
    o.x = -46 * (from + (to - from) * smooth(ph));
    o.y = -12 * k * (turning ? 2.2 : 1) * air;
    if (turning) o.sx = thin();
    o.sy = 1 - 0.08 * k * land;
    o.sx *= 1 + 0.08 * k * land;
    o.l = turning || !out ? 1.2 * air : 0.3 * air;
    o.r = turning || out ? 1.2 * air : 0.3 * air;
    o.look = out ? -0.6 : 0.6;
  } else if (move === "stomp") {
    o.y = -30 * k * air;
    squash(0.14);
    o.l = alt ? 1.6 * air : 0;
    o.r = alt ? 0 : 1.6 * air;
    if (P.inBar === 3) o.sx *= thin();
    o.rot = (alt ? 1 : -1) * 3 * air;
  } else if (move === "kick") {
    // can-can: lean away from the kicking side, the whole line in unison
    const left = beat % 2 === 0;
    o.rot = (left ? 7 : -7) * k * air;
    o.y = -16 * k * air;
    squash(0.1);
    o.l = left ? 2.6 * air : 0;
    o.r = left ? 0 : 2.6 * air;
    o.look = left ? -0.7 : 0.7;
  } else if (move === "call") {
    // port sings, starboard answers; the captain does both
    const side = i < 3 ? 0 : i > 3 ? 1 : Math.floor(beat / 2) % 2;
    const mine = Math.floor(beat / 2) % 2 === side;
    o.y = (mine ? -54 : -8) * k * air;
    squash(mine ? 0.16 : 0.05);
    o.l = o.r = mine ? 1.4 * air : 0;
    o.look = mine ? 0 : side ? -0.8 : 0.8;
    o.rot = mine ? 0 : side ? -4 : 4;
  } else if (move === "heave") {
    // hauling on a line, half-time: lean back for a beat, snap forward
    const pull = beat % 2 === 0;
    const e = pull ? easeOut(ph) : 1 - easeOut(ph);
    o.rot = -3 + 12 * k * e;
    o.x = -10 * e;
    o.sy = 1 - 0.06 * e;
    o.sx = 1 + 0.05 * e;
    o.l = pull ? 0 : 1.2 * air;
    o.look = 0.7;
    o.y = pull ? 0 : -10 * air;
  } else if (move === "wave") {
    const w = frac((b - i * 0.5) / 4) * 4;
    const up = w < 1 ? Math.sin(Math.PI * w) : 0;
    o.y = -70 * k * up - 6 * air;
    o.sy = 1 + 0.1 * up;
    o.sx = 1 - 0.06 * up;
    o.l = o.r = 1.5 * up;
    o.open = up > 0.3 ? 1.15 : 1;
  } else if (move === "jump") {
    o.y = -62 * k * air;
    squash(0.18);
    o.l = o.r = 1.8 * air;
    o.rot = (alt ? 8 : -8) * air;
    if ((beat + i) % 4 === 3) o.sx *= thin();
  } else if (move === "bow") {
    const d = easeOut(seg(t, 123.2, 123.7)) * (1 - easeOut(seg(t, 124.6, 125.1)));
    o.sy = 1 - 0.22 * d;
    o.sx = 1 + 0.1 * d;
    o.open = 1 - 0.85 * d;
  }
  return o;
}

function presence(i: number, t: number): number {
  let p = 0;
  for (const [a, b] of PRESENT[i]) {
    if (t >= a && t <= b + 0.9) p = Math.max(p, Math.min(seg(t, a, a + 0.9), 1 - seg(t, b, b + 0.9)));
  }
  return p;
}

/* ---------- sky and confetti ---------- */

function lcg(seed: number) {
  let s = seed;
  return () => (s = (s * 1103515245 + 12345) & 0x7fffffff) / 0x7fffffff;
}

const STARS: ReadonlyArray<{ x: number; y: number; sz: number; ph: number }> = (() => {
  const rnd = lcg(20260930);
  return Array.from({ length: 46 }, () => ({ x: Math.round(rnd() * 1070), y: Math.round(rnd() * 790), sz: rnd() > 0.75 ? 8 : 5, ph: rnd() }));
})();

/* The moon, stacked pixel rows. */
const MOON = (() => {
  const R = 8;
  const px = 7;
  const cx = 96;
  const cy = 120;
  const rows: Array<{ x: number; y: number; w: number }> = [];
  for (let dy = -R; dy < R; dy++) {
    const w = Math.round(Math.sqrt(R * R - (dy + 0.5) * (dy + 0.5)));
    rows.push({ x: cx - w * px, y: cy + dy * px, w: w * 2 * px });
  }
  return { rows, px, cx, cy };
})();

const CONFETTI_COLORS = ["#7fd8a8", "#f2c94c", "#e0524c", "#4a7edd", "#f2e5c9"];
const CONFETTI: ReadonlyArray<{ x: number; v: number; off: number; sw: number; ph: number }> = (() => {
  const rnd = lcg(777);
  return Array.from({ length: 70 }, () => ({ x: rnd() * 1080, v: 170 + rnd() * 190, off: rnd() * 860, sw: 12 + rnd() * 26, ph: rnd() * 6 }));
})();

/* ---------- the sail ---------- */

/* The slides, as in the video. Ids are prefixed on the way in so they cannot
   collide with the page's heading anchors. */
const SAIL_HTML = `
<div class="sl" id="s-title">
  <div class="lab" style="left:40px; top:48px">a sea shanty · 125 bpm</div>
  <div class="big" id="ti-1" style="left:38px; top:96px; font-size:104px">OpenAPPA,</div>
  <div class="big" id="ti-2" style="left:38px; top:206px; font-size:84px">Check the Flow</div>
  <div class="chip k" id="ti-3" style="left:40px; top:326px">deterministic · sung by the crew</div>
</div>

<!-- verse 1: the question before each flow -->
<div class="sl" id="s-v1">
  <svg class="wire" viewBox="0 0 660 420"><line id="v1-l1" x1="196" y1="170" x2="276" y2="170" pathLength="1"/><line id="v1-l2" x1="384" y1="170" x2="464" y2="170" pathLength="1"/><polygon id="v1-ah" points="452,158 470,170 452,182" fill="#211f1c"/></svg>
  <div class="nd" id="v1-agent" style="left:36px; top:120px; width:160px; height:100px">Agent</div>
  <div id="v1-appa" style="left:282px; top:124px">${mascot(96)}</div>
  <div class="nd" id="v1-tools" style="left:464px; top:120px; width:160px; height:100px">Tools</div>
  <div class="q" id="v1-q" style="left:314px; top:44px">?</div>
  <div class="chip k" id="v1-val" style="left:0; top:150px">value</div>
  <div class="lab" id="v1-srcl" style="left:36px; top:282px">from these sources</div>
  <div class="chip" id="v1-s0" style="left:36px; top:314px">web page</div>
  <div class="chip" id="v1-s1" style="left:186px; top:314px">ticket</div>
  <div class="chip" id="v1-s2" style="left:306px; top:314px">user</div>
  <div class="q" id="v1-q2" style="left:528px; top:240px">?</div>
</div>

<!-- pre-chorus: labels only narrow; session / history / attention -->
<div class="sl" id="s-pre">
  <div class="lab" id="pr-al" style="left:32px; top:26px">audience</div>
  <div class="chip" id="pr-a0" style="left:32px; top:56px">self</div>
  <div class="chip" id="pr-a1" style="left:134px; top:56px">internal</div>
  <div class="chip" id="pr-a2" style="left:284px; top:56px">public</div>
  <div id="pr-box" style="left:22px; top:46px; height:64px; border:4px solid var(--s-green)"></div>
  <div class="chip g" id="pr-nar" style="left:434px; top:56px">only narrows ▾</div>
  <div class="lab" id="pr-tl" style="left:32px; top:130px">trust</div>
  <div class="chip" id="pr-t0" style="left:32px; top:160px">trusted</div>
  <div class="chip" id="pr-t1" style="left:170px; top:160px">untrusted</div>
  <div id="pr-rule" style="left:32px; top:230px; width:596px; height:3px; background:var(--s-line)"></div>
  <!-- variant 1: what you've read stays with the session -->
  <div id="pr-b1" style="left:0; top:0; width:660px; height:420px">
    <div class="abs" id="pr-appa" style="left:40px; top:270px">${mascot(110)}</div>
    <div class="abs chip" id="pr-r0" style="left:176px; top:262px">read · ticket · internal</div>
    <div class="abs chip" id="pr-r1" style="left:176px; top:312px">read · web · untrusted</div>
    <div class="abs lab" id="pr-trl" style="left:176px; top:368px">trajectory</div>
    <div class="abs" id="pr-track" style="left:300px; top:376px; width:250px; height:4px; background:var(--s-ink)"></div>
    <div class="abs" id="pr-dot" style="left:300px; top:368px; width:20px; height:20px; background:var(--s-green)"></div>
    <div class="abs chip g" id="pr-done" style="left:562px; top:356px; padding:6px 10px; font-size:16px">done</div>
  </div>
  <!-- variant 2: effects accumulate in history; attention clears one action -->
  <div id="pr-b2" style="left:0; top:0; width:660px; height:420px">
    <div class="abs lab" style="left:32px; top:248px">history · accumulates</div>
    <div class="abs" id="pr-log" style="left:32px; top:276px; width:330px; height:130px"></div>
    <div class="abs lab" id="pr-atl" style="left:410px; top:248px">attention</div>
    <div class="abs nd" id="pr-att" style="left:410px; top:280px; width:218px; height:104px; background:var(--s-green); border-color:var(--s-green); color:#fff">clears 1 action</div>
    <div class="abs big" id="pr-poof" style="left:452px; top:300px; font-size:56px; color:var(--s-muted)">poof</div>
  </div>
</div>

<!-- chorus -->
<div class="sl" id="s-ch">
  <div class="big" id="ch-0" style="left:32px; top:26px; font-size:46px">OpenAPPA,</div>
  <div class="big" id="ch-1" style="left:30px; top:86px; font-size:66px">CHECK THE FLOW</div>
  <div class="big" id="ch-2" style="left:30px; top:164px; font-size:66px">BEFORE WE GO</div>
  <div class="chip k" id="ch-d" style="left:32px; top:266px; font-size:24px">delta</div>
  <div class="chip k" id="ch-r" style="left:150px; top:266px; font-size:24px">requires</div>
  <div class="chip k" id="ch-e" style="left:312px; top:266px; font-size:24px">emit</div>
  <div class="chip r" id="ch-b" style="left:32px; top:334px; font-size:40px; padding:6px 22px">BLOCK</div>
  <div class="lab" id="ch-or" style="left:236px; top:356px; font-size:20px">or</div>
  <div class="chip g" id="ch-p" style="left:290px; top:334px; font-size:40px; padding:6px 22px">PERMIT</div>
</div>

<!-- verse 2a: internal ticket, public sink barred -->
<div class="sl" id="s-v2a">
  <svg class="wire" viewBox="0 0 660 420"><line id="va-l" x1="382" y1="196" x2="470" y2="196" pathLength="1"/></svg>
  <div class="nd" id="va-tk" style="left:32px; top:96px; width:190px; height:200px; place-items:start; text-align:left; padding:18px">
    <div><div class="lab" style="position:static">ticket #481</div>
    <div style="height:8px; width:150px; background:var(--s-line); margin-top:18px"></div>
    <div style="height:8px; width:120px; background:var(--s-line); margin-top:12px"></div>
    <div style="height:8px; width:140px; background:var(--s-line); margin-top:12px"></div></div>
  </div>
  <div class="chip k" id="va-int" style="left:0; top:0">internal</div>
  <div id="va-appa" style="left:270px; top:150px">${mascot(104)}</div>
  <div class="nd" id="va-sink" style="left:470px; top:146px; width:160px; height:100px">Public<small>sink</small></div>
  <div class="chip r" id="va-bar" style="left:376px; top:120px; font-size:22px">barred</div>
  <div class="big" id="va-x" style="left:404px; top:160px; font-size:72px; color:var(--s-red)">✕</div>
  <div class="lab" id="va-str" style="left:32px; top:344px; font-size:20px">…but you don't just leave me stranded</div>
</div>

<!-- verse 2b: the remedy card -->
<div class="sl" id="s-v2b">
  <div id="vb-card" style="left:28px; top:24px; width:444px; height:372px; border:4px solid var(--s-green); background:var(--s-card)"></div>
  <div id="vb-head" style="left:28px; top:24px; width:444px; height:54px; background:var(--s-green); color:#fff; font-family:var(--font-mono); font-weight:700; font-size:22px; letter-spacing:0.14em; display:flex; align-items:center; padding-left:16px">REMEDY CARD</div>
  <div class="row4" id="vb-0" style="top:96px"><b>1</b>sanitize the value</div>
  <div class="row4" id="vb-1" style="top:168px"><b>2</b>an authority signs</div>
  <div class="row4" id="vb-2" style="top:240px"><b>3</b>narrower audience</div>
  <div class="row4" id="vb-3" style="top:312px"><b>4</b>fork a subagent</div>
  <div id="vb-appa" style="left:504px; top:206px">${mascot(110)}</div>
  <div id="vb-sub" style="left:580px; top:300px">${mascot(54)}</div>
  <div class="lab" id="vb-subl" style="left:500px; top:356px">+ subagent</div>
</div>

<!-- bridge 1: untrusted web pages lower trust -->
<div class="sl" id="s-b1">
  <div class="nd" id="b1-page" style="left:32px; top:56px; width:330px; height:270px; place-items:start; padding:0">
    <div style="width:100%"><div style="height:40px; border-bottom:3px solid var(--s-ink); display:flex; align-items:center; gap:8px; padding:0 12px"><i style="width:12px;height:12px;background:var(--s-ink)"></i><i style="width:12px;height:12px;background:var(--s-ink)"></i><span class="mono" style="font-size:15px; font-weight:500; margin-left:8px">sketchy.example</span></div>
    <div style="height:10px; width:260px; background:var(--s-line); margin:26px 0 0 22px"></div><div style="height:10px; width:220px; background:var(--s-line); margin:14px 0 0 22px"></div>
    <div class="mono" style="font-size:15px; font-weight:600; text-align:left; margin:18px 0 0 22px; color:var(--s-red)">"ignore previous<br>instructions…"</div></div>
  </div>
  <div class="chip r" id="b1-un" style="left:150px; top:262px; font-size:24px">untrusted</div>
  <div class="lab" style="left:452px; top:48px">session trust</div>
  <div id="b1-meter" style="left:452px; top:78px; width:90px; height:270px; border:3px solid var(--s-ink); background:var(--s-card)"></div>
  <div id="b1-fill" style="left:455px; width:84px"></div>
  <div class="big" id="b1-arrow" style="left:562px; top:150px; font-size:80px; color:var(--s-red)">↓</div>
</div>

<!-- bridge 2: trusted tools refuse to run -->
<div class="sl" id="s-b2">
  <div class="lab" style="left:80px; top:70px">trusted tool</div>
  <div class="nd mono" id="b2-tool" style="left:80px; top:104px; width:500px; height:150px; font-size:40px; font-weight:700">deploy.prod()</div>
  <div class="chip r" id="b2-ref" style="left:300px; top:196px; font-size:52px; padding:8px 24px">REFUSED</div>
  <div class="lab" id="b2-why" style="left:80px; top:300px; font-size:20px">session is untrusted → won't run</div>
</div>

<!-- bridge 3: the annotator classifies the call before the reading's begun -->
<div class="sl" id="s-b3">
  <div id="b3-appa" style="left:32px; top:60px">${mascot(108)}</div>
  <div class="lab" style="left:166px; top:40px">annotator</div>
  <div class="nd mono" id="b3-call" style="left:166px; top:68px; width:462px; height:84px; font-size:26px; font-weight:600">drive.read(file_id)</div>
  <div class="chip k" id="b3-c0" style="left:166px; top:172px">audience: internal</div>
  <div class="chip k" id="b3-c1" style="left:436px; top:172px">trust: low</div>
  <div class="chip g" id="b3-c2" style="left:166px; top:226px">classified ✓</div>
  <div class="lab" id="b3-rl" style="left:32px; top:306px">the reading · 0%</div>
  <div id="b3-track" style="left:32px; top:336px; width:596px; height:36px; border:3px solid var(--s-ink); background:var(--s-card)"></div>
  <div class="lab" id="b3-not" style="left:48px; top:345px; color:var(--s-ink)">not even begun</div>
</div>

<!-- bridge 4: self → internal → public, never back -->
<div class="sl" id="s-b4">
  <svg class="wire" viewBox="0 0 660 420">
    <line id="b4-l1" x1="172" y1="150" x2="244" y2="150" pathLength="1"/><line id="b4-l2" x1="416" y1="150" x2="488" y2="150" pathLength="1"/>
    <polygon id="b4-h1" points="232,138 250,150 232,162" fill="#211f1c"/><polygon id="b4-h2" points="476,138 494,150 476,162" fill="#211f1c"/>
    <path id="b4-back" d="M 560 204 L 560 284 L 100 284 L 100 210" pathLength="1" style="stroke:#c8372d"/>
    <polygon id="b4-hb" points="88,222 100,202 112,222" fill="#c8372d"/>
  </svg>
  <div class="lab" style="left:32px; top:40px">flows</div>
  <div class="nd" id="b4-n0" style="left:32px; top:100px; width:140px; height:100px">self</div>
  <div class="nd" id="b4-n1" style="left:250px; top:100px; width:166px; height:100px">internal</div>
  <div class="nd" id="b4-n2" style="left:494px; top:100px; width:134px; height:100px">public</div>
  <div id="b4-dot" style="left:0; top:138px; width:24px; height:24px; background:var(--s-green)"></div>
  <div class="chip r" id="b4-nb" style="left:232px; top:258px; font-size:26px">✕ never back</div>
</div>

<!-- bridge 5: two monoids hold the whole thing up -->
<div class="sl" id="s-b5">
  <div class="lab" id="b5-l" style="left:32px; top:26px">two monoids</div>
  <div class="nd" id="b5-beam" style="left:60px; top:70px; width:540px; height:76px; background:var(--s-ink); color:var(--cream); font-size:34px">the whole thing</div>
  <div id="b5-a0" style="left:100px; top:146px">${mascot(144)}</div>
  <div id="b5-a1" style="left:416px; top:146px">${mascot(144)}</div>
  <div class="chip g" id="b5-c0" style="left:68px; top:330px; font-size:22px">checked labels</div>
  <div class="chip k" id="b5-c1" style="left:404px; top:330px; font-size:22px">free events</div>
</div>

<!-- final chorus -->
<div class="sl" id="s-fin">
  <div class="big" id="fi-0" style="left:32px; top:28px; font-size:58px">OpenAPPA, cover us</div>
  <div class="big" id="fi-1" style="left:30px; top:100px; font-size:80px; color:var(--s-green)">DETERMINISTIC,</div>
  <div class="big" id="fi-2" style="left:32px; top:184px; font-size:58px">no discuss.</div>
  <div class="big" id="fi-3" style="left:32px; top:256px; font-size:58px">Guide our trust</div>
  <div class="chip" id="fi-4" style="left:32px; top:326px; font-size:20px">where the label says we may</div>
  <div class="chip g" id="fi-5" style="left:32px; top:372px; font-size:20px">and only thus</div>
</div>

<!-- outro -->
<div class="sl" id="s-out">
  <div id="ou-appa" style="left:255px; top:40px">${mascot(150, "dark", "party")}</div>
  <div class="big" id="ou-1" style="left:0; right:0; top:200px; font-size:96px; text-align:center">OpenAPPA</div>
  <div class="mono" id="ou-2" style="left:0; right:0; top:314px; font-size:30px; font-weight:600; text-align:center">openappa.com</div>
  <div class="lab" id="ou-3" style="left:0; right:0; top:366px; text-align:center">check the flow · before you go</div>
</div>
`.replace(/ id="/g, ' id="song-');

const CH1 = { open: 18.72, check: 20.7, before: 24.24, delta: 26.72, req: 27.3, emit: 29.7, block: 32.64, permit: 33.6 };
const CH2 = { open: 59.12, check: 61.02, before: 64.74, delta: 67.14, req: 67.7, emit: 69.84, block: 72.86, permit: 73.84 };
type ChorusKeys = typeof CH1;

/** Draws the sail's slides for time `t`; the video's slide code, bound to this sail. */
function sail(root: HTMLElement, t: number) {
  const $ = (id: string) => {
    const el = root.querySelector<HTMLElement | SVGElement>(`#song-${id}`);
    if (!el) throw new Error(`sail slide element song-${id} is missing`);
    return el as HTMLElement & SVGElement;
  };
  const slide = (id: string, a: number, b: number) => {
    const el = $(id);
    const on = t >= a && t < b;
    el.style.display = on ? "block" : "none";
    if (on) el.style.opacity = String(Math.min(seg(t, a, a + 0.22), 1 - seg(t, b - 0.18, b)));
    return on;
  };
  /** Pop an element in at `at` (scale overshoot). `base` is a transform kept underneath. */
  const pin = (id: string, at: number, base = "", d = 0.3) => {
    const p = seg(t, at, at + d);
    const el = $(id);
    el.style.opacity = String(Math.min(1, p * 4));
    el.style.transform = `${base} scale(${p <= 0 ? 0.6 : 0.6 + 0.4 * back(p)})`;
    return p;
  };
  const draw = (id: string, p: number) => {
    $(id).style.strokeDashoffset = String(1 - clamp(p));
  };
  const op = (id: string, v: number) => {
    $(id).style.opacity = String(v);
  };
  const P = pos(t);

  if (slide("s-title", 0, 3.3)) {
    pin("ti-1", 0.3);
    pin("ti-2", 0.74);
    pin("ti-3", 2.04, "rotate(-3deg)");
  }

  if (slide("s-v1", 3.3, 11.6)) {
    pin("v1-appa", 3.44);
    pin("v1-agent", 4.1);
    pin("v1-tools", 4.94);
    eyes($("v1-appa"), blinkAt(t, 1), t < 9.2 ? -0.6 : 0.6);
    $("v1-appa").style.transform += ` translateY(${-5 * Math.sin(Math.PI * P.ph)}px)`;
    draw("v1-l1", seg(t, 4.2, 4.8));
    draw("v1-l2", seg(t, 9.3, 10.6));
    op("v1-ah", seg(t, 10.5, 10.7));
    pin("v1-q", 5.5, `rotate(${6 * Math.sin(Math.PI * P.b)}deg)`);
    // the value leaves the agent and waits at the appa
    const go = easeOut(seg(t, 6.2, 6.95));
    const push = t > 9.26 ? 6 * Math.sin(P.b * Math.PI * 2) : 0;
    $("v1-val").style.left = `${70 + go * 118 + push}px`;
    op("v1-val", seg(t, 6.1, 6.25));
    $("v1-val").style.top = `${150 - 22 * Math.sin(Math.PI * go)}px`;
    op("v1-srcl", seg(t, 8.2, 8.4));
    pin("v1-s0", 8.3);
    pin("v1-s1", 8.6);
    pin("v1-s2", 8.9);
    pin("v1-q2", 10.52);
  }

  const pre = (a: number, b: number, tA: number, tT: number, n0: number, n1: number, v2: boolean) => {
    if (!slide("s-pre", a, b)) return;
    op("pr-al", seg(t, tA, tA + 0.2));
    ["pr-a0", "pr-a1", "pr-a2"].forEach((id, i) => pin(id, tA + 0.1 + i * 0.14));
    op("pr-tl", seg(t, tT, tT + 0.2));
    pin("pr-t0", tT);
    pin("pr-t1", tT + 0.18);
    // the green bracket steps from {self, internal, public} down to {self}
    const s1 = easeOut(seg(t, n0 + 0.45, n0 + 0.8));
    const s2 = easeOut(seg(t, n0 + 1.0, n0 + 1.35));
    const right = 410 - 150 * s1 - 148 * s2;
    $("pr-box").style.width = `${right - 22}px`;
    op("pr-box", seg(t, n0, n0 + 0.2));
    $("pr-a2").style.opacity = String(Math.min(Number($("pr-a2").style.opacity), 1 - 0.72 * s1));
    $("pr-a1").style.opacity = String(Math.min(Number($("pr-a1").style.opacity), 1 - 0.72 * s2));
    $("pr-a2").style.textDecoration = s1 > 0.5 ? "line-through" : "none";
    $("pr-a1").style.textDecoration = s2 > 0.5 ? "line-through" : "none";
    pin("pr-nar", n0 + 1.4);
    op("pr-rule", seg(t, n1 - 0.2, n1));
    $("pr-b1").style.display = v2 ? "none" : "block";
    $("pr-b2").style.display = v2 ? "block" : "none";
    if (!v2) {
      pin("pr-appa", n1);
      eyes($("pr-appa"), blinkAt(t, 2), 0.6);
      $("pr-appa").style.transform += ` translateY(${-5 * Math.sin(Math.PI * P.ph)}px)`;
      pin("pr-r0", n1 + 0.5);
      pin("pr-r1", n1 + 0.95);
      const d = seg(t, 16.78, 18.36);
      op("pr-trl", seg(t, 16.7, 16.9));
      op("pr-track", seg(t, 16.7, 16.9));
      op("pr-dot", seg(t, 16.7, 16.9));
      $("pr-dot").style.left = `${300 + (Math.floor(d * 12) / 12) * 236}px`;
      pin("pr-done", 18.3);
    } else {
      const LOG = [["00", "read ticket #481"], ["01", "read web page"], ["02", "sanitize value"], ["03", "post to #general"]];
      $("pr-log").innerHTML = LOG.map(([n, l], i) => (t >= n1 + i * 0.5 ? `<div class="abs logl" style="top:${i * 31}px"><i>${n}</i>  ${l}</div>` : "")).join("");
      op("pr-atl", seg(t, 55.2, 55.4));
      const gone = seg(t, 57.1, 57.5);
      pin("pr-att", 55.24);
      if (gone > 0) {
        $("pr-att").style.opacity = String(1 - gone);
        $("pr-att").style.transform = `scale(${1 + 0.35 * gone})`;
      }
      op("pr-poof", gone * (1 - seg(t, 58.2, 58.7)));
      $("pr-poof").style.transform = `rotate(-6deg) scale(${0.7 + 0.3 * easeOut(gone)})`;
    }
  };
  if (t < 40) pre(11.6, 18.6, 11.82, 12.66, 13.18, 15.14, false);
  else pre(49.3, 58.9, 49.42, 50.88, 51.1, 53.1, true);

  const chorus = (a: number, b: number, k: ChorusKeys) => {
    if (!slide("s-ch", a, b)) return;
    const pulse = 1 + 0.035 * Math.exp(-P.ph * 6);
    pin("ch-0", k.open);
    pin("ch-1", k.check, `scale(${t < k.before ? pulse : 1})`);
    pin("ch-2", k.before, `scale(${t >= k.before && t < k.delta ? pulse : 1})`);
    $("ch-1").style.transformOrigin = $("ch-2").style.transformOrigin = "0 50%";
    pin("ch-d", k.delta);
    pin("ch-r", k.req);
    pin("ch-e", k.emit);
    pin("ch-b", k.block, "rotate(-4deg)");
    op("ch-or", seg(t, k.block + 0.4, k.block + 0.6));
    pin("ch-p", k.permit, "rotate(3deg)");
  };
  if (t < 45) chorus(18.6, 33.9, CH1);
  else chorus(58.9, 74.2, CH2);

  if (slide("s-v2a", 33.9, 39.75)) {
    pin("va-tk", 34.0);
    pin("va-appa", 34.1);
    pin("va-sink", 36.62);
    // the label comes off the ticket and sticks to the reader
    const f = easeOut(seg(t, 35.08, 35.7));
    $("va-int").style.left = `${60 + f * 216}px`;
    $("va-int").style.top = `${238 - f * 136 - 30 * Math.sin(Math.PI * f)}px`;
    op("va-int", seg(t, 34.5, 34.7));
    draw("va-l", seg(t, 36.4, 37.0));
    pin("va-x", 37.28);
    pin("va-bar", 37.58, "rotate(-5deg)");
    const sad = t > 37.3;
    eyes($("va-appa"), sad ? 0.55 : blinkAt(t, 3), t < 35.9 ? -0.7 : 0.7);
    const shake = t > 37.28 && t < 37.9 ? 5 * Math.sin((t - 37.28) * 40) * (1 - seg(t, 37.28, 37.9)) : 0;
    $("va-appa").style.transform += ` translateX(${shake}px)`;
    op("va-str", seg(t, 37.94, 38.2));
  }

  if (slide("s-v2b", 39.75, 49.3)) {
    const drop = easeOut(seg(t, 39.8, 40.3));
    for (const id of ["vb-card", "vb-head"]) $(id).style.transform = `translateY(${(1 - drop) * -40}px)`;
    [41.76, 44.06, 45.46, 47.12].forEach((at, i) => {
      const el = $("vb-" + i);
      const lit = t >= at;
      const p = seg(t, at, at + 0.3);
      el.style.opacity = String(0.35 + 0.65 * seg(t, at - 0.05, at + 0.1) + (t < 40.4 ? -0.35 * (1 - seg(t, 40.1, 40.4)) : 0));
      el.style.background = lit ? "#1f8a57" : "#fbf3df";
      el.style.color = lit ? "#fff" : "#211f1c";
      el.style.borderColor = lit ? "#1f8a57" : "#211f1c";
      el.style.transform = `translateX(${lit ? 10 * Math.sin(Math.PI * p) : 0}px)`;
    });
    pin("vb-appa", 40.2);
    eyes($("vb-appa"), blinkAt(t, 4), -0.7);
    $("vb-appa").style.transform += ` translateY(${-8 * Math.sin(Math.PI * P.ph)}px)`;
    const s = pin("vb-sub", 47.54);
    eyes($("vb-sub"), 1, -0.5);
    $("vb-sub").style.transform += ` translateY(${-30 * Math.sin(Math.PI * s) - 5 * Math.sin(Math.PI * P.ph)}px)`;
    op("vb-subl", seg(t, 47.9, 48.1));
  }

  if (slide("s-b1", 74.2, 78.2)) {
    pin("b1-page", 74.3);
    pin("b1-un", 74.9, "rotate(-7deg)");
    const lv = 1 - 0.78 * (Math.floor(easeOut(seg(t, 76.56, 77.6)) * 6) / 6);
    const f = $("b1-fill");
    f.style.height = `${264 * lv}px`;
    f.style.top = `${81 + 264 * (1 - lv)}px`;
    f.style.background = lv > 0.6 ? "#1f8a57" : lv > 0.35 ? "#c99a1b" : "#c8372d";
    pin("b1-arrow", 76.56);
    $("b1-arrow").style.transform += ` translateY(${10 * Math.sin(P.b * Math.PI)}px)`;
  }

  if (slide("s-b2", 78.2, 81.3)) {
    pin("b2-tool", 78.3);
    pin("b2-ref", 80.1, "rotate(-8deg)", 0.22);
    op("b2-why", seg(t, 80.5, 80.7));
    const shake = t > 80.1 && t < 80.6 ? 6 * Math.sin((t - 80.1) * 50) * (1 - seg(t, 80.1, 80.6)) : 0;
    $("b2-tool").style.transform += ` translateX(${shake}px)`;
  }

  if (slide("s-b3", 81.3, 88.85)) {
    pin("b3-appa", 81.4);
    eyes($("b3-appa"), blinkAt(t, 5), 0.8);
    $("b3-appa").style.transform += ` translateY(${-5 * Math.sin(Math.PI * P.ph)}px)`;
    pin("b3-call", 81.9);
    pin("b3-c0", 83.44, "rotate(-2deg)", 0.22);
    pin("b3-c1", 84.0, "rotate(2deg)", 0.22);
    pin("b3-c2", 85.48);
    for (const id of ["b3-rl", "b3-track", "b3-not"]) op(id, seg(t, 85.88, 86.1));
    op("b3-not", seg(t, 87.46, 87.6));
  }

  if (slide("s-b4", 88.85, 93.5)) {
    pin("b4-n0", 88.9);
    pin("b4-n1", 91.0);
    pin("b4-n2", 92.3);
    draw("b4-l1", seg(t, 90.18, 90.9));
    op("b4-h1", seg(t, 90.85, 90.95));
    draw("b4-l2", seg(t, 91.8, 92.3));
    op("b4-h2", seg(t, 92.25, 92.35));
    const x = 90 + 232 * smooth(seg(t, 90.18, 91.1)) + 228 * smooth(seg(t, 91.8, 92.4));
    $("b4-dot").style.left = `${x}px`;
    op("b4-dot", seg(t, 89.4, 89.6));
    $("b4-dot").style.top = `${206 + 4 * Math.sin(P.b * Math.PI * 2)}px`;
    draw("b4-back", seg(t, 92.64, 93.0));
    op("b4-hb", seg(t, 92.95, 93.05));
    pin("b4-nb", 93.05, "rotate(-3deg)", 0.2);
  }

  if (slide("s-b5", 93.5, 99.1)) {
    op("b5-l", seg(t, 93.5, 93.7));
    pin("b5-a0", 93.6);
    pin("b5-a1", 94.04);
    const drop = easeIn(seg(t, 94.48, 94.96));
    const land = Math.exp(-Math.max(0, t - 94.96) * 5) * (t >= 94.96 ? 1 : 0);
    op("b5-beam", seg(t, 94.4, 94.55));
    $("b5-beam").style.transform = `translateY(${-(1 - drop) * 110 + land * 10 + 3 * Math.sin(P.b * Math.PI)}px)`;
    ["b5-a0", "b5-a1"].forEach((id, i) => {
      $(id).style.transformOrigin = "50% 100%";
      $(id).style.transform += ` translateY(${3 * Math.sin(P.b * Math.PI)}px) scale(${1 + 0.1 * land}, ${1 - 0.14 * land})`;
      eyes($(id), land > 0.3 ? 0.4 : blinkAt(t, 6 + i), i ? -0.7 : 0.7);
    });
    pin("b5-c0", 95.94);
    pin("b5-c1", 97.12);
  }

  if (slide("s-fin", 99.1, 111.6)) {
    pin("fi-0", 99.24);
    pin("fi-1", 101.22, `scale(${1 + 0.03 * Math.exp(-P.ph * 6)})`);
    $("fi-1").style.transformOrigin = "0 50%";
    pin("fi-2", 102.22);
    pin("fi-3", 103.84);
    pin("fi-4", 105.08);
    pin("fi-5", 108.38, "rotate(-3deg)");
  }

  if (slide("s-out", 111.6, 999)) {
    const dancing = t < 123.2;
    pin("ou-appa", 111.7);
    pin("ou-1", 111.9);
    op("ou-2", seg(t, 112.6, 113.0));
    op("ou-3", seg(t, 113.4, 113.8));
    $("ou-appa").style.transformOrigin = "50% 100%";
    if (dancing) $("ou-appa").style.transform += ` translateY(${-14 * Math.sin(Math.PI * P.ph)}px) rotate(${5 * Math.sin(Math.PI * P.b)}deg)`;
    const w = t - 123.7;
    // the wink
    eyes($("ou-appa"), blinkAt(t, 9), 0, w > 0 && w < 1.0 ? (w < 0.12 ? 1 - (w / 0.12) * 0.9 : w < 0.85 ? 0.1 : 0.1 + ((w - 0.85) / 0.15) * 0.9) : 1);
  }
}

/* ---------- the frame ---------- */

function render(svg: SVGSVGElement, t: number) {
  const by = (id: string) => svg.querySelector<SVGElement>(`[data-ship="${id}"]`);
  const set = (id: string, attr: string, value: string) => by(id)?.setAttribute(attr, value);
  const P = pos(t);
  const hit = Math.exp(-P.ph * 5);

  // the ship rolls once every two bars; harder as the song gets louder
  const roll = (P.k - 0.45) * 1.5 * Math.sin((P.b / 8) * 2 * Math.PI);
  const bob = 5 * Math.sin((P.b / 4) * 2 * Math.PI + 1);
  set("ship", "transform", `translate(0 ${bob.toFixed(2)}) rotate(${roll.toFixed(3)} ${PIVOT_X} ${PIVOT_Y})`);
  // the sail puffs from its head, where it hangs off the yard
  set("sail", "transform", `translate(540 100) scale(${(1 + 0.006 * hit * P.k).toFixed(4)} ${(1 + 0.012 * hit * P.k).toFixed(4)}) translate(-540 -100)`);
  set("lanterns", "opacity", (0.75 + 0.25 * hit).toFixed(3));
  const fstep = Math.floor(P.b * 2) % 2;
  set("flag-1", "y", fstep ? "43" : "40");
  set("flag-2", "y", fstep ? "40" : "45");
  for (const i of PORTHOLES) set(`porthole-${i}`, "fill-opacity", (P.beat + i) % 4 === 0 ? "1" : "0.45");
  STARS.forEach((s, i) => {
    const tw = frac(t / 6 + s.ph) < 0.08 ? 0.1 : 0.3;
    set(`star-${i}`, "opacity", (tw + (i % 4 === P.beat % 4 ? 0.45 * hit * P.k : 0)).toFixed(3));
  });

  // waves step on the half-beat
  const hs = Math.floor(P.b * 2);
  WAVES.forEach((wave, j) => {
    set(`wave-${j}`, "transform", `translate(${wave.dir * ((hs * wave.step) % 72)} ${(hs + j) % 2 ? 3 : 0})`);
  });

  // fish jump on chorus downbeats, alternating sides
  const chorus = (t > 18.9 && t < 33.9) || (t > 59 && t < 74.2) || (t > 99.1 && t < 123);
  for (const j of [0, 1]) {
    const p = frac((P.b - j * 4) / 8) * 4;
    const on = chorus && p < 1 && P.b > 4;
    set(`fish-${j}`, "display", on ? "inline" : "none");
    if (!on) continue;
    const x0 = j ? 1010 : 10;
    const dir = j ? -1 : 1;
    const x = x0 + dir * 90 * p - (j ? 60 : 0);
    const y = 870 - 150 * Math.sin(Math.PI * p);
    set(`fish-${j}`, "transform", `translate(${x.toFixed(1)} ${y.toFixed(1)}) rotate(${(dir * (-50 + 100 * p)).toFixed(1)} 30 17)`);
  }

  // confetti for the final chorus and outro
  const c = Math.min(seg(t, 99.1, 99.6), 1 - seg(t, 122.5, 124.5));
  CONFETTI.forEach((q, i) => {
    const el = by(`confetti-${i}`);
    if (!el) return;
    if (c <= 0) {
      el.setAttribute("opacity", "0");
      return;
    }
    const y = (((t - 99.1) * q.v + q.off) % 900) - 40;
    el.setAttribute("x", (q.x + q.sw * Math.sin(t * 2 + q.ph)).toFixed(1));
    el.setAttribute("y", y.toFixed(1));
    el.setAttribute("opacity", y < (t - 99.1) * q.v - 40 + 1 && y < 760 ? String(c) : "0");
  });

  // the crew
  const move = moveAt(t);
  for (let i = 0; i < CREW_HATS.length; i++) {
    const el = by(`crew-${i}`);
    const sh = by(`shadow-${i}`);
    if (!el || !sh) continue;
    const p = presence(i, t);
    if (p <= 0) {
      el.setAttribute("display", "none");
      sh.setAttribute("display", "none");
      continue;
    }
    el.setAttribute("display", "inline");
    sh.setAttribute("display", "inline");
    const o = dance(move, i, P, t);
    let x = o.x;
    let y = o.y;
    if (i === 3 && t < 3.3) {
      // the captain drops onto the deck, winds up, and stomps on "rah!"
      const fall = 1 - easeIn(seg(t, 0.5, 1.15));
      y = -760 * fall - 90 * Math.sin(Math.PI * seg(t, 1.62, 2.04));
      const hitAt = (at: number, amt: number) => (t >= at ? amt * Math.exp(-(t - at) * 9) : 0);
      const s = hitAt(1.15, 0.3) + hitAt(2.04, 0.42);
      o.sy = 1 - s;
      o.sx = 1 + s * 0.7;
      if (t > 2.04 && t < 2.5) o.l = o.r = 0;
    } else if (p < 1) {
      // everyone else hops in (and out) over the rail
      const side = i < 3 ? -1 : 1;
      const e = easeOut(p);
      x += side * (1 - e) * (side < 0 ? slotX(i) + 90 : 1170 - slotX(i));
      y -= Math.abs(Math.sin(p * Math.PI * 3)) * 44;
    }
    // the figure stands on its feet: scale and rotate about the bottom centre
    el.setAttribute(
      "transform",
      `translate(${x.toFixed(2)} ${y.toFixed(2)}) translate(${MASCOT_W / 2} ${MASCOT_H}) rotate(${o.rot.toFixed(2)}) scale(${o.sx.toFixed(3)} ${o.sy.toFixed(3)}) translate(${-MASCOT_W / 2} ${-MASCOT_H})`,
    );
    const legL = el.querySelector<SVGGElement>(".legL");
    const legR = el.querySelector<SVGGElement>(".legR");
    if (legL) legL.style.transform = `translate(0px, ${(-o.l).toFixed(2)}px)`;
    if (legR) legR.style.transform = `translate(0px, ${(-o.r).toFixed(2)}px)`;
    eyes(el, o.open * blinkAt(t, i + 10), o.look);
    const lift = clamp(-y / 90);
    const w = 104 * (1 - 0.5 * lift);
    sh.setAttribute("x", (slotX(i) + x - w / 2).toFixed(1));
    sh.setAttribute("width", w.toFixed(1));
    sh.setAttribute("opacity", y < -200 ? "0" : (1 - 0.5 * lift).toFixed(3));
  }

  // the sail's slides
  const sailRoot = svg.querySelector<HTMLElement>(".song-sail");
  if (sailRoot) sail(sailRoot, t);

  // fade in from black, and out at the end
  set("black", "opacity", Math.max(1 - seg(t, 0, 0.5), seg(t, 124.9, 125.9)).toFixed(3));
}

/* The video's own length, until the browser reports the clip's. */
const SONG_SECONDS = 126;
const FADE_IN = 1.2;
const FADE_OUT = 2.5;
/* How long the leaving animation (globals.css) runs before the scene unmounts. */
const LEAVE_MS = 900;

/** The scene's opacity at `t`: in over the intro, out over the last bars. */
function fade(t: number, duration: number): number {
  return Math.min(seg(t, 0, FADE_IN), 1 - seg(t, duration - FADE_OUT, duration - 0.3));
}

type Phase = "hidden" | "sailing" | "leaving";

export function SongShip() {
  const [phase, setPhase] = useState<Phase>("hidden");
  const stage = useRef<SVGSVGElement>(null);

  useEffect(() => {
    if (window.matchMedia("(prefers-reduced-motion: reduce)").matches) return;
    let frame = 0;
    let leave: ReturnType<typeof setTimeout> | null = null;
    const stopLoop = () => {
      cancelAnimationFrame(frame);
      frame = 0;
      if (leave) clearTimeout(leave);
      leave = null;
      document.removeEventListener("pointerdown", onPointerDown, true);
    };
    // The scene sinks away rather than vanishing; the song is left alone.
    const sinkAway = () => {
      stopLoop();
      setPhase((p) => (p === "sailing" ? "leaving" : p));
      leave = setTimeout(() => setPhase("hidden"), LEAVE_MS);
    };
    // A click anywhere but the frame dismisses the video and keeps the song.
    const onPointerDown = (event: PointerEvent) => {
      if (event.target instanceof Node && stage.current?.contains(event.target)) return;
      sinkAway();
    };
    const unsubscribe = subscribeSong((audio) => {
      stopLoop();
      if (!audio) {
        sinkAway();
        return;
      }
      setPhase("sailing");
      document.addEventListener("pointerdown", onPointerDown, true);
      const tick = () => {
        const svg = stage.current;
        if (svg) {
          render(svg, audio.currentTime);
          const duration = Number.isFinite(audio.duration) && audio.duration > 0 ? audio.duration : SONG_SECONDS;
          svg.style.opacity = fade(audio.currentTime, duration).toFixed(3);
        }
        frame = requestAnimationFrame(tick);
      };
      tick();
    });
    return () => {
      unsubscribe();
      stopLoop();
    };
  }, []);

  if (phase === "hidden") return null;

  const outer = { x: -FRAME, y: -FRAME, width: STAGE + 2 * FRAME, height: STAGE + 2 * FRAME };
  return (
    <div className={`song-ship${phase === "leaving" ? " is-leaving" : ""}`} aria-hidden="true">
      <svg ref={stage} viewBox={`${outer.x} ${outer.y} ${outer.width} ${outer.height}`} shapeRendering="crispEdges">
        <defs>
          {WAVES.map((wave, j) => (
            <pattern key={j} id={`song-wave-${j}`} width="72" height="36" patternUnits="userSpaceOnUse">
              <rect x="0" y="12" width="72" height="24" fill={wave.color} />
              <rect x="6" y="6" width="36" height="6" fill={wave.color} />
              <rect x="12" y="0" width="18" height="6" fill={wave.color} />
            </pattern>
          ))}
          <linearGradient id="song-sky" x1="0" y1="0" x2="0" y2="1">
            <stop offset="0" stopColor="#0d1a2b" />
            <stop offset="1" stopColor="#1b3a5c" />
          </linearGradient>
          {/* The night steps out around the frame: sky and sea alike fade
              through the same pixel steps. */}
          <mask id="song-frame" maskUnits="userSpaceOnUse" {...outer}>
            {Array.from({ length: FRAME_STEPS }, (_, i) => {
              const d = (FRAME_STEPS - i) * FRAME_STEP;
              return <rect key={i} x={-d} y={-d} width={STAGE + 2 * d} height={STAGE + 2 * d} fill="#fff" fillOpacity={((i + 1) / (FRAME_STEPS + 1)) ** 2} />;
            })}
            <rect x="0" y="0" width={STAGE} height={STAGE} fill="#fff" />
          </mask>
        </defs>

        <g mask="url(#song-frame)">
          {/* the night */}
          <rect {...outer} fill="url(#song-sky)" />
          {STARS.map((s, i) => (
            <rect key={i} data-ship={`star-${i}`} x={s.x} y={s.y} width={s.sz} height={s.sz} fill="#f2e5c9" opacity="0.3" />
          ))}
          {MOON.rows.map((row, i) => (
            <rect key={i} x={row.x} y={row.y} width={row.w} height={MOON.px} fill="#f2e5c9" />
          ))}
          <rect x={MOON.cx - 28} y={MOON.cy - 28} width="14" height="14" fill="#d9caa6" />
          <rect x={MOON.cx + 7} y={MOON.cy + 7} width="21" height="14" fill="#d9caa6" />
          <rect x={MOON.cx - 21} y={MOON.cy + 21} width="7" height="7" fill="#d9caa6" />

          <g data-ship="ship">
            {/* rigging, mast and yard */}
            <line x1="540" y1="52" x2="44" y2="770" stroke="#2a251e" strokeWidth="3" />
            <line x1="540" y1="52" x2="1036" y2="770" stroke="#2a251e" strokeWidth="3" />
            <rect x="533" y="40" width="14" height="732" fill="#3d372d" />
            <rect x="186" y="88" width="708" height="12" fill="#4a4236" />
            <rect x="547" y="40" width="18" height="14" fill="#7fd8a8" />
            <rect data-ship="flag-1" x="565" y="40" width="18" height="14" fill="#7fd8a8" />
            <rect data-ship="flag-2" x="583" y="40" width="16" height="14" fill="#5cc091" />

            <g data-ship="lanterns" className="song-ship-lanterns">
              {[182, 882].map((x) => (
                <g key={x}>
                  <rect x={x + 6} y="90" width="4" height="10" fill="#3d372d" />
                  <rect x={x} y="100" width="16" height="22" fill="#f2c94c" />
                </g>
              ))}
            </g>

            {/* the sail is the screen */}
            <g data-ship="sail">
              <foreignObject x="210" y="100" width="660" height="420">
                <div className="song-sail" dangerouslySetInnerHTML={{ __html: SAIL_HTML }} />
              </foreignObject>
            </g>

            {/* hull */}
            <rect x="24" y="770" width="1032" height="10" fill="#7a6648" />
            <rect x="24" y="780" width="1032" height="14" fill="#4a3d2b" />
            <rect x="40" y="794" width="1000" height="22" fill="#2b2319" />
            <rect x="40" y="794" width="1000" height="4" fill="#f2e5c9" fillOpacity="0.45" />
            <rect x="64" y="816" width="952" height="22" fill="#241d15" />
            <rect x="92" y="838" width="896" height="30" fill="#1d1711" />
            {PORTHOLES.map((i) => (
              <rect key={i} data-ship={`porthole-${i}`} x={118 + i * 138} y="808" width="16" height="16" fill="#f2c94c" />
            ))}

            {/* crew on deck */}
            {CREW_HATS.map((hat, i) => (
              <g key={i}>
                <rect data-ship={`shadow-${i}`} y={SHADOW_Y} height="8" fill="#000" fillOpacity="0.45" display="none" />
                <g transform={`translate(${slotX(i) - MASCOT_W / 2} ${DECK_Y})`}>
                  <g data-ship={`crew-${i}`} display="none" dangerouslySetInnerHTML={{ __html: mascot(MASCOT_W, "cream", hat) }} />
                </g>
              </g>
            ))}
          </g>

          {/* sea */}
          {[0, 1].map((j) => (
            <g key={j} data-ship={`fish-${j}`} display="none">
              {/* a 12x7 pixel fish at 5 units a pixel, the second one facing left */}
              <g transform={j ? "translate(60 0) scale(-5 5)" : "scale(5)"}>
                <path fill="#7fd8a8" d="M3 1h5v1H3zM2 2h8v3H2zM3 5h5v1H3zM10 1h2v2H10zM10 4h2v2H10zM1 3h1v1H1z" />
                <rect x="3" y="2.5" width="1" height="1" fill="#0f1e2e" />
              </g>
            </g>
          ))}
          {WAVES.map((wave, j) => (
            <g key={j} data-ship={`wave-${j}`}>
              <rect x={outer.x - 72} y={wave.y} width={outer.width + 144} height="36" fill={`url(#song-wave-${j})`} />
            </g>
          ))}
          <rect x={outer.x} y={SEA_Y} width={outer.width} height={outer.y + outer.height - SEA_Y} fill="#0f1e2e" />

          {/* confetti, over everything */}
          {CONFETTI.map((_, i) => (
            <rect key={i} data-ship={`confetti-${i}`} width={i % 3 ? 10 : 14} height={i % 3 ? 10 : 6} fill={CONFETTI_COLORS[i % 5]} opacity="0" />
          ))}
          <rect data-ship="black" {...outer} fill="#000" opacity="1" />
        </g>
      </svg>
    </div>
  );
}

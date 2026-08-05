// Pixel-grid brand marks for OpenAPPA: <appa-mark> (mascot) and <appa-word>
// (wordmark), ported verbatim from the design project's pixel.js. Both marks
// share the same pixel grid. Registration is deferred to the client because
// HTMLElement does not exist during server rendering.

const W = 24;
const H = 22;

function seg(...runs: [string, number][]): string {
  let s = "";
  for (const [ch, n] of runs) s += ch.repeat(n);
  return s;
}

// 24 x 22 cell creature on the same grid as the pixel wordmark.
// 1 body · 3 dim (muzzle, paws) · 4 eyes · 2 nose
const BEAST = [
  seg([".", 5], ["1", 2], [".", 10], ["1", 2], [".", 5]),
  seg([".", 5], ["1", 2], [".", 10], ["1", 2], [".", 5]),
  seg([".", 4], ["1", 16], [".", 4]),
  seg([".", 3], ["1", 18], [".", 3]),
  seg([".", 3], ["1", 18], [".", 3]),
  seg([".", 3], ["1", 18], [".", 3]),
  seg([".", 3], ["1", 3], ["4", 3], ["1", 6], ["4", 3], ["1", 3], [".", 3]),
  seg([".", 3], ["1", 3], ["4", 3], ["1", 6], ["4", 3], ["1", 3], [".", 3]),
  seg([".", 3], ["1", 3], ["4", 3], ["1", 6], ["4", 3], ["1", 3], [".", 3]),
  seg([".", 3], ["1", 18], [".", 3]),
  seg([".", 3], ["1", 7], ["3", 4], ["1", 7], [".", 3]),
  seg([".", 3], ["1", 7], ["3", 1], ["2", 2], ["3", 1], ["1", 7], [".", 3]),
  seg([".", 3], ["1", 18], [".", 3]),
  seg([".", 4], ["1", 16], [".", 4]),
  seg([".", 1], ["1", 22], [".", 1]),
  seg(["1", 24]),
  seg(["1", 24]),
  seg(["1", 24]),
  seg(["1", 24]),
  seg(["1", 24]),
  seg(["1", 5], [".", 2], ["1", 4], [".", 2], ["1", 4], [".", 2], ["1", 5]),
  seg(["3", 5], [".", 2], ["3", 4], [".", 2], ["3", 4], [".", 2], ["3", 5]),
];

const FILL: Record<string, string> = {
  "1": "var(--text-strong)",
  "3": "var(--text-weak)",
  "2": "var(--bg)",
  "4": "var(--bg)",
};

// Mascot variants: an overlay glyph on the same grid, drawn in `--accent-bg`
// so it cuts light out of the dark body in both themes. `clean` sweeps a
// broom (a sanitized result), `accept` carries a check (an accepted
// narrowing).
const OVERLAYS: Record<string, { ox: number; oy: number; rows: string[] }> = {
  clean: {
    ox: 13,
    oy: 3,
    rows: [
      ".......##",
      "......##.",
      "......##.",
      ".....##..",
      ".....##..",
      "....##...",
      "...##....",
      "..######.",
      ".########",
      ".########",
      "#.##..##.",
    ],
  },
  accept: {
    ox: 10,
    oy: 10,
    rows: [
      "..........##",
      ".........###",
      "........###.",
      "##.....###..",
      "###...###...",
      ".###.###....",
      "..#####.....",
      "...###......",
    ],
  },
};

const GLYPHS: Record<string, string[]> = {
  O: ["01110", "10001", "10001", "10001", "10001", "10001", "01110", "00000", "00000"],
  A: ["01110", "10001", "10001", "11111", "10001", "10001", "10001", "00000", "00000"],
  P: ["11110", "10001", "10001", "11110", "10000", "10000", "10000", "00000", "00000"],
  p: ["00000", "00000", "11110", "10001", "10001", "10001", "11110", "10000", "10000"],
  e: ["00000", "00000", "01110", "10001", "11111", "10000", "01111", "00000", "00000"],
  n: ["00000", "00000", "10110", "11001", "10001", "10001", "10001", "00000", "00000"],
};

export function registerPixelMarks(): void {
  if (typeof window === "undefined") return;

  if (!customElements.get("appa-mark")) {
    class AppaMark extends HTMLElement {
      connectedCallback() {
        if (this.shadowRoot) return;
        const size = parseInt(this.getAttribute("size") || "220", 10);
        const boxH = Math.round((size * H) / W);
        const overlaySpec = OVERLAYS[this.getAttribute("variant") || ""];
        let overlay = "";
        if (overlaySpec)
          for (let y = 0; y < overlaySpec.rows.length; y++)
            for (let x = 0; x < overlaySpec.rows[y].length; x++)
              if (overlaySpec.rows[y][x] === "#")
                overlay +=
                  '<rect x="' + (overlaySpec.ox + x) + '" y="' + (overlaySpec.oy + y) +
                  '" width="1" height="1" fill="var(--accent-bg)"/>';
        let eyes = "";
        let body = "";
        for (let y = 0; y < H; y++)
          for (let x = 0; x < W; x++) {
            const c = BEAST[y][x];
            if (c === ".") continue;
            const r = '<rect x="' + x + '" y="' + y + '" width="1" height="1" fill="' + FILL[c] + '"/>';
            if (c === "4") {
              // Body pixel behind each eye: blinking scales the eye away and
              // must reveal fur, not the page background (eye fill IS --bg).
              body += '<rect x="' + x + '" y="' + y + '" width="1" height="1" fill="' + FILL["1"] + '"/>';
              eyes += r;
            } else body += r;
          }
        this.attachShadow({ mode: "open" }).innerHTML =
          "<style>:host{display:inline-block;line-height:0}" +
          "svg{display:block;overflow:visible}" +
          "@keyframes appa-float{0%,100%{transform:translateY(0)}50%{transform:translateY(-2%)}}" +
          "@keyframes appa-blink{0%,91%,97%,100%{transform:scaleY(1)}94%{transform:scaleY(0.08)}}" +
          ".f{animation:appa-float 5.5s ease-in-out infinite}" +
          ".e{transform-box:fill-box;transform-origin:center;animation:appa-blink 5s ease-in-out infinite}" +
          "@media (prefers-reduced-motion:reduce){.f,.e{animation:none}}</style>" +
          '<svg viewBox="0 0 ' + W + " " + H + '" width="' + size + '" height="' + boxH +
          '" shape-rendering="crispEdges" role="img" aria-label="OpenAPPA mascot">' +
          '<g class="f">' + body + '<g class="e">' + eyes + "</g>" + overlay + "</g></svg>";
      }
    }
    customElements.define("appa-mark", AppaMark);
  }

  if (!customElements.get("appa-word")) {
    class AppaWord extends HTMLElement {
      connectedCallback() {
        if (this.shadowRoot) return;
        const word = this.getAttribute("word") || "OpenAPPA";
        const cap = parseFloat(this.getAttribute("cap") || "18");
        const LC = 6;
        const ROWS = 9;
        const CAP = 7;
        const px: [number, number, boolean][] = [];
        for (let i = 0; i < word.length; i++) {
          const g = GLYPHS[word[i]];
          if (!g) continue;
          const dim = word[i] === word[i].toLowerCase();
          for (let y = 0; y < ROWS; y++)
            for (let x = 0; x < 5; x++) if (g[y][x] === "1") px.push([i * LC + x, y, dim]);
        }
        const w = word.length * LC - 1;
        const boxH = (cap * ROWS) / CAP;
        const boxW = (boxH * w) / ROWS;
        let s = "";
        for (const [x, y, dim] of px)
          s +=
            '<rect x="' + x + '" y="' + y + '" width="1" height="1" fill="' +
            (dim ? "var(--text-weak)" : "currentColor") + '"/>';
        this.attachShadow({ mode: "open" }).innerHTML =
          "<style>:host{display:inline-block;line-height:0}svg{display:block}</style>" +
          '<svg viewBox="0 0 ' + w + " " + ROWS + '" width="' + boxW + '" height="' + boxH +
          '" shape-rendering="crispEdges" role="img" aria-label="' + word + '">' + s + "</svg>";
      }
    }
    customElements.define("appa-word", AppaWord);
  }
}

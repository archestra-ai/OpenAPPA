"use client";

import { useEffect, useRef, useState, type CSSProperties, type ReactNode } from "react";

import { PixelLockup, PixelMark, PixelWordmark } from "@/components/Logo";
import { StlViewer } from "@/components/StlViewer";

/* The Branding page's assets: the mark, the wordmark and the lockup, drawn by
   the same components the header renders. What the page shows is what ships,
   and an identity change reaches this page without anyone editing it.

   The marks are drawn with `var(--token)` fills. That makes them follow the
   site's theme for free, and it makes an exported file meaningless until the
   tokens are resolved — so both the preview and every export here work from
   one small palette, read out of the site's own stylesheet. */

type ThemeName = "light" | "dark";

/** The only tokens the marks are drawn with. */
const MARK_TOKENS = ["--text-strong", "--text-weak", "--bg"] as const;
type MarkToken = (typeof MARK_TOKENS)[number];
type Palette = Record<MarkToken, string>;

const FALLBACK: Record<ThemeName, Palette> = {
  light: { "--text-strong": "hsl(30, 8%, 11%)", "--text-weak": "hsl(32, 3%, 54%)", "--bg": "hsl(40, 25%, 99%)" },
  dark: { "--text-strong": "hsl(40, 12%, 93%)", "--text-weak": "hsl(33, 4%, 50%)", "--bg": "hsl(30, 6%, 8%)" },
};

/**
 * Both themes' values for the mark's tokens, read out of the stylesheet the
 * page is already using: `:root` carries the light theme and `.dark` the dark
 * one. Reading them beats restating them — a page that names its own colours
 * eventually names them wrongly. The literals above are only for the case
 * where no rule can be read at all (a stylesheet the browser will not open).
 */
function useThemePalettes(): Record<ThemeName, Palette> {
  const [palettes, setPalettes] = useState(FALLBACK);

  useEffect(() => {
    const read: Record<ThemeName, Palette> = {
      light: { ...FALLBACK.light },
      dark: { ...FALLBACK.dark },
    };
    const take = (declaration: CSSStyleDeclaration, theme: ThemeName) => {
      for (const token of MARK_TOKENS) {
        const value = declaration.getPropertyValue(token).trim();
        if (value) read[theme][token] = value;
      }
    };

    for (const sheet of Array.from(document.styleSheets)) {
      let rules: CSSRuleList;
      try {
        rules = sheet.cssRules;
      } catch {
        continue; // cross-origin, nothing to read
      }
      for (const rule of Array.from(rules)) {
        if (!(rule instanceof CSSStyleRule)) continue;
        if (rule.selectorText === ":root") take(rule.style, "light");
        if (rule.selectorText === ".dark") take(rule.style, "dark");
      }
    }
    setPalettes(read);
  }, []);

  return palettes;
}

// ---- export ---------------------------------------------------------------

/**
 * The asset as a standalone file: tokens resolved, site-only classes dropped.
 *
 * `keepClasses` is for the one export that wants them — the prompt hands over
 * the animation CSS alongside the markup, so there the hooks are the point.
 * `mutate` runs on the clone while those hooks are still there, which is how
 * a GIF frame finds the eyes it has to close.
 */
function standaloneSvg(
  source: SVGSVGElement,
  palette: Palette,
  size?: { width: number; height: number },
  keepClasses = false,
  mutate?: (clone: SVGSVGElement) => void,
): string {
  const clone = source.cloneNode(true) as SVGSVGElement;
  clone.setAttribute("xmlns", "http://www.w3.org/2000/svg");
  clone.removeAttribute("style");
  mutate?.(clone);
  // Class names carry the float and blink animations, which live in the
  // site's stylesheet and cannot travel with the file.
  if (!keepClasses) {
    clone.querySelectorAll("[class]").forEach((node) => node.removeAttribute("class"));
    clone.removeAttribute("class");
  }
  if (size) {
    clone.setAttribute("width", String(size.width));
    clone.setAttribute("height", String(size.height));
  }

  return new XMLSerializer()
    .serializeToString(clone)
    .replace(/var\(--([\w-]+)\)/g, (whole, name: string) => palette[`--${name}` as MarkToken] ?? whole)
    // The wordmark's capitals inherit their colour from the page.
    .replace(/currentColor/g, palette["--text-strong"]);
}

/** Raster size: an integer scale keeps every pixel of the grid square. */
function rasterSize(source: SVGSVGElement, target = 1024) {
  const box = source.viewBox.baseVal;
  const scale = Math.max(1, Math.round(target / box.width));
  return { width: Math.round(box.width * scale), height: Math.round(box.height * scale) };
}

async function rasterize(
  source: SVGSVGElement,
  palette: Palette,
  background?: string,
  mutate?: (clone: SVGSVGElement) => void,
): Promise<HTMLCanvasElement> {
  const { width, height } = rasterSize(source);
  const svg = standaloneSvg(source, palette, { width, height }, false, mutate);

  const image = new Image();
  image.decoding = "sync";
  await new Promise<void>((resolve, reject) => {
    image.onload = () => resolve();
    image.onerror = () => reject(new Error("the mark did not rasterize"));
    // Base64 rather than a blob URL: a tainted canvas cannot be read back, and
    // some browsers taint on blob-loaded SVG.
    image.src = `data:image/svg+xml;base64,${btoa(unescape(encodeURIComponent(svg)))}`;
  });

  const canvas = document.createElement("canvas");
  canvas.width = width;
  canvas.height = height;
  const ctx = canvas.getContext("2d");
  if (!ctx) throw new Error("no 2d context");
  if (background) {
    ctx.fillStyle = background;
    ctx.fillRect(0, 0, width, height);
  }
  ctx.drawImage(image, 0, 0, width, height);
  return canvas;
}

async function pngBlob(source: SVGSVGElement, palette: Palette): Promise<Blob> {
  // No background: the PNG carries the glyph and nothing else.
  const canvas = await rasterize(source, palette);
  return await new Promise<Blob>((resolve, reject) =>
    canvas.toBlob((blob) => (blob ? resolve(blob) : reject(new Error("PNG encoding failed"))), "image/png"),
  );
}

/**
 * The blink, as frames.
 *
 * The site blinks Appa with a CSS keyframe: the eyes hold open for most of a
 * five-second cycle, then shut and open again inside 6% of it. A GIF has no
 * timing function, so the eased shut is sampled — and because GIF carries a
 * delay per frame, the long open hold costs exactly one frame. Six frames and
 * a few kilobytes buy the whole animation.
 *
 * `eyes` is the vertical scale; the delays sum to the five-second cycle.
 */
const BLINK: { eyes: number; delay: number }[] = [
  { eyes: 1, delay: 4750 },
  { eyes: 0.55, delay: 50 },
  { eyes: 0.2, delay: 50 },
  { eyes: 0.08, delay: 50 },
  { eyes: 0.2, delay: 50 },
  { eyes: 0.55, delay: 50 },
];

/**
 * Closes the eyes to `scale` about their own centre — what
 * `transform-box: fill-box; transform-origin: center` does in the stylesheet,
 * written out, because a standalone frame has no stylesheet. The centre comes
 * from the live element: it is on screen, so it can be measured rather than
 * guessed from the pixel grid.
 */
function blinkFrame(source: SVGSVGElement, scale: number) {
  const live = source.querySelector<SVGGraphicsElement>(".appa-mark-eyes");
  if (!live) return undefined;
  const box = live.getBBox();
  const centre = box.y + box.height / 2;
  return (clone: SVGSVGElement) => {
    const eyes = clone.querySelector<SVGGElement>(".appa-mark-eyes");
    eyes?.setAttribute("transform", `translate(0 ${centre}) scale(1 ${scale}) translate(0 ${-centre})`);
  };
}

async function gifBlob(source: SVGSVGElement, palette: Palette): Promise<Blob> {
  // GIF's transparency is one bit, which would ragged the mark's edges — so
  // every frame is flattened onto the theme's own background instead.
  //
  // Only the blink is animated. The float is a 2% drift on a 5.5s cycle that
  // never lines up with the 5s blink, so carrying it would mean either a
  // 55-second loop or a visible jump at the seam — and it is the eyes anyone
  // looks at. An asset with eyes gets six frames; one without gets a still.
  const frames = source.querySelector(".appa-mark-eyes") ? BLINK : [{ eyes: 1, delay: 0 }];

  const { GIFEncoder, applyPalette, quantize } = await import("gifenc");
  const gif = GIFEncoder();
  let colors: number[][] | null = null;

  for (const [at, frame] of frames.entries()) {
    const canvas = await rasterize(source, palette, palette["--bg"], blinkFrame(source, frame.eyes));
    const ctx = canvas.getContext("2d");
    if (!ctx) throw new Error("no 2d context");
    const { data } = ctx.getImageData(0, 0, canvas.width, canvas.height);
    // Every frame is the same handful of flat colours, so one table serves
    // them all — quantizing per frame would risk the palette shifting mid-blink.
    colors ??= quantize(data, 16);
    gif.writeFrame(applyPalette(data, colors), canvas.width, canvas.height, {
      palette: at === 0 ? colors : undefined,
      delay: frame.delay,
      repeat: 0, // loop forever
    });
  }

  gif.finish();
  // A fresh copy, because bytes() hands back a view onto a reused buffer.
  return new Blob([new Uint8Array(gif.bytes())], { type: "image/gif" });
}

function save(blob: Blob, filename: string) {
  const url = URL.createObjectURL(blob);
  const link = document.createElement("a");
  link.href = url;
  link.download = filename;
  link.click();
  URL.revokeObjectURL(url);
}

/**
 * Appa blinks and floats wherever the site draws it, and that is two CSS
 * animations over one SVG rather than a video or a sprite sheet. This hands
 * the whole thing over as one prompt: the markup with its animation hooks
 * intact, the keyframes that drive them, and the reduced-motion opt-out that
 * has to travel with any looping animation.
 *
 * The keyframes are restated here rather than scraped from the stylesheet —
 * they are the asset being handed over, not a page style, and a prompt that
 * silently changed with a refactor of the site's CSS would be worse than one
 * that has to be edited on purpose.
 */
function blinkingMarkPrompt(source: SVGSVGElement, palette: Palette): string {
  const markup = standaloneSvg(source, palette, { width: 96, height: 88 }, true);
  return `Add the blinking OpenAPPA mascot, Appa, to my page.

It is a 24 × 22 pixel grid drawn as an SVG. One group floats; the eyes are
their own group and scale away for a moment to blink. Use the markup and the
CSS below as they are — keep \`shape-rendering="crispEdges"\`, because the mark
is pixel art and must never be smoothed, and keep the reduced-motion rule.
Change the fills only if you need it in other colours.

\`\`\`html
${markup}
\`\`\`

\`\`\`css
@keyframes appa-float {
  0%,
  100% {
    transform: translateY(0);
  }
  50% {
    transform: translateY(-2%);
  }
}

/* The eyes hold open for most of the cycle, then shut and open again inside
   6% of it — that short beat is what reads as a blink rather than a squint. */
@keyframes appa-blink {
  0%,
  91%,
  97%,
  100% {
    transform: scaleY(1);
  }
  94% {
    transform: scaleY(0.08);
  }
}

.appa-mark-float {
  animation: appa-float 5.5s ease-in-out infinite;
  /* Set a delay per mascot to stop a crowd of them blinking in unison. */
  animation-delay: var(--appa-float-delay, 0s);
}

.appa-mark-eyes {
  /* Scale about the eyes' own box, not the SVG's origin. */
  transform-box: fill-box;
  transform-origin: center;
  animation: appa-blink 5s ease-in-out infinite;
  animation-delay: var(--appa-blink-delay, 0s);
}

@media (prefers-reduced-motion: reduce) {
  .appa-mark-float,
  .appa-mark-eyes {
    animation: none;
  }
}
\`\`\`
`;
}

// ---- panels ---------------------------------------------------------------

const FORMATS = ["svg", "png", "gif"] as const;
type Format = (typeof FORMATS)[number];

/**
 * The printable mascot, in two models and one source apiece.
 *
 * The detailed one builds every pixel of the mark as its own chamfered block,
 * so the grid reads from every direction — and every layer's perimeter has to
 * weave around it. The fast one keeps the silhouette and drops the per-cell
 * chamfers, which is most of the print time. Each ships whole, and the
 * detailed one also ships split where the mark is split, for two colors.
 *
 * The SCAD calls those two halves `primary` and `secondary` (they are what
 * `-D part=…` takes); the files are named for what a reader does with them.
 */
const MODELS: { title: string; note: string; files: { href: string; name: string; note: string }[] }[] = [
  {
    title: "Detailed",
    note: "every pixel a chamfered block · 50 mm tall · ~20k facets",
    files: [
      { href: "/brand/openappa-full-body.stl", name: "openappa-full-body.stl", note: "one piece, one color" },
      {
        href: "/brand/openappa-multi-color-part-1.stl",
        name: "openappa-multi-color-part-1.stl",
        note: "the body — every solid cell of the mark",
      },
      {
        href: "/brand/openappa-multi-color-part-2.stl",
        name: "openappa-multi-color-part-2.stl",
        note: "muzzle and paws — the dimmed cells that drop into part 1",
      },
      { href: "/brand/openappa.scad", name: "openappa.scad", note: "OpenSCAD source" },
    ],
  },
  {
    title: "Fast print",
    note: "the silhouette, flat on its back, no supports · 55 × 50 × 9 mm",
    files: [
      {
        href: "/brand/openappa-fast-printing.stl",
        name: "openappa-fast-printing.stl",
        note: "one piece, one color, chamfered on every outer edge",
      },
      {
        href: "/brand/openappa-fast-printing.scad",
        name: "openappa-fast-printing.scad",
        note: "OpenSCAD source",
      },
    ],
  },
];

/* The stage paints the chosen theme's background and overrides the tokens the
   marks are drawn with, so the preview is the export. `color` is in there
   because the wordmark's capitals are `currentColor`: without it they would
   keep the page's own text colour and vanish on the opposite background. */
function stageStyle(palette: Palette): CSSProperties {
  return { ...palette, background: palette["--bg"], color: palette["--text-strong"] } as CSSProperties;
}

function AssetPanel({
  name,
  file,
  note,
  theme,
  palette,
  className,
  children,
}: {
  name: string;
  /** Basename for the download, without the extension. */
  file: string;
  note: string;
  theme: ThemeName;
  palette: Palette;
  className?: string;
  children: ReactNode;
}) {
  const stage = useRef<HTMLDivElement>(null);
  const [busy, setBusy] = useState<Format | null>(null);

  const download = async (format: Format) => {
    const source = stage.current?.querySelector("svg");
    if (!source) return;
    setBusy(format);
    try {
      const filename = `${file}-${theme}.${format}`;
      if (format === "svg") {
        // Vector, so any size renders — but a file that arrives at 24 px wide
        // is awkward to drop into a document, so it carries the raster size.
        save(new Blob([standaloneSvg(source, palette, rasterSize(source))], { type: "image/svg+xml" }), filename);
      } else if (format === "png") {
        save(await pngBlob(source, palette), filename);
      } else {
        save(await gifBlob(source, palette), filename);
      }
    } finally {
      setBusy(null);
    }
  };

  return (
    <figure className={className ? `brand-panel ${className}` : "brand-panel"} data-asset={file}>
      <div className="brand-stage" ref={stage} style={stageStyle(palette)}>
        {children}
      </div>
      <figcaption className="brand-panel-foot">
        <span className="brand-panel-name">{name}</span>
        <span className="brand-panel-note">{note}</span>
        <span className="brand-panel-actions">
          {FORMATS.map((format) => (
            <button disabled={busy !== null} key={format} onClick={() => download(format)} type="button">
              {busy === format ? "…" : format}
            </button>
          ))}
        </span>
      </figcaption>
    </figure>
  );
}

export function BrandAssets() {
  const palettes = useThemePalettes();
  const [theme, setTheme] = useState<ThemeName>("dark");
  const [promptCopied, setPromptCopied] = useState(false);
  const grid = useRef<HTMLDivElement>(null);
  const palette = palettes[theme];

  // The page's own theme is the one a reader is most likely to want first.
  useEffect(() => {
    setTheme(document.documentElement.classList.contains("dark") ? "dark" : "light");
  }, []);

  useEffect(() => {
    if (!promptCopied) return;
    const timer = setTimeout(() => setPromptCopied(false), 2000);
    return () => clearTimeout(timer);
  }, [promptCopied]);

  const copyPrompt = async () => {
    const mark = grid.current?.querySelector<SVGSVGElement>('[data-asset="openappa-mark"] svg');
    if (!mark) return;
    await navigator.clipboard.writeText(blinkingMarkPrompt(mark, palette));
    setPromptCopied(true);
  };

  return (
    <div className="brand-assets">
      <div className="brand-toolbar">
        <span className="brand-toolbar-label">Theme</span>
        <span className="brand-switch">
          {(["light", "dark"] as const).map((option) => (
            <button
              aria-pressed={theme === option}
              className={theme === option ? "is-on" : undefined}
              key={option}
              onClick={() => setTheme(option)}
              type="button"
            >
              {option === "light" ? "White" : "Black"}
            </button>
          ))}
        </span>
        <span className="brand-toolbar-note">
          SVG and PNG carry the glyph alone; GIF blinks, on the theme&apos;s background.
        </span>
      </div>

      <div className="brand-grid" ref={grid}>
        <AssetPanel
          file="openappa-mark"
          name="The mark"
          note="24 × 22 pixels · Appa"
          palette={palette}
          theme={theme}
        >
          <PixelMark size={96} />
        </AssetPanel>
        <AssetPanel
          file="openappa-wordmark"
          name="The wordmark"
          note="7-row caps · lowercase dimmed"
          palette={palette}
          theme={theme}
        >
          <PixelWordmark word="OpenAPPA" capHeight={38} />
        </AssetPanel>
        <AssetPanel
          className="brand-panel-wide"
          file="openappa-lockup"
          name="The lockup"
          note="mark · wordmark, both sized from the cap height"
          palette={palette}
          theme={theme}
        >
          <PixelLockup capHeight={30} />
        </AssetPanel>
      </div>

      {/* The still files above cannot carry the one thing Appa does. This
          hands over the animation itself, ready to paste into an assistant. */}
      <div className="brand-prompt">
        <button onClick={copyPrompt} type="button">
          {promptCopied ? "Copied to clipboard" : "Copy prompt with code for blinking OpenAPPA"}
        </button>
        <span className="brand-prompt-note">SVG plus the float, blink and reduced-motion CSS.</span>
      </div>

      {/* The same bitmap, off the screen: every lit pixel becomes a chamfered
          block. The two STLs are the mark's solid and dimmed cells, printed
          separately and imported together as one two-colour object; the SCAD
          is what they are exported from. */}
      <div className="brand-files">
        <span className="brand-files-label">3D Print your OpenAPPA!</span>
        {/* Width and height are on the tag so the page does not jump when the
            photo arrives; the CSS scales it inside the column. */}
        <figure className="brand-photo">
          <img
            alt="A printed OpenAPPA mascot sitting on a sofa behind a developer working on a laptop"
            height={1050}
            src="/brand/appa-watching.webp"
            width={1400}
          />
          <figcaption>
            OpenAPPA watching{" "}
            <a href="https://github.com/Konstantinov-Innokentii" rel="noreferrer" target="_blank">
              github.com/Konstantinov-Innokentii
            </a>{" "}
            code. Non-AI-edited photo.
          </figcaption>
        </figure>
        <StlViewer />
        {MODELS.map((model) => (
          <div className="brand-model" key={model.title}>
            <div className="brand-model-head">
              <span className="brand-model-title">{model.title}</span>
              <span className="brand-model-note">{model.note}</span>
            </div>
            <ul>
              {model.files.map(({ href, name, note }) => (
                <li key={href}>
                  <a download href={href}>
                    {name}
                  </a>
                  <span>{note}</span>
                </li>
              ))}
            </ul>
          </div>
        ))}
      </div>
    </div>
  );
}

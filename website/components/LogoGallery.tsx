import { PixelWordmark, type WordmarkOptions } from "@/components/Logo";

const SIZES = [12, 15, 20, 36];

const VARIANTS: { label: string; note: string; options: WordmarkOptions }[] = [
  {
    label: "Wordmark",
    note: "Solid blocks with dimmed lowercase. This is the logo used across the site.",
    options: {},
  },
  {
    label: "Single tone",
    note: "Solid blocks in one color, no case dimming.",
    options: { dimLowercase: false },
  },
  {
    label: "Pixel grid",
    note: "Visible grid gaps. Blends at small sizes; use at 20px cap height and above.",
    options: { gap: 1.5, dimLowercase: false },
  },
  {
    label: "Coarse grid",
    note: "Wider gaps for a stronger grid texture. Large sizes only.",
    options: { gap: 2.5, dimLowercase: false },
  },
  {
    label: "Dot matrix",
    note: "LED-style circles. Large sizes only.",
    options: { shape: "dot", gap: 1.5, dimLowercase: false },
  },
];

export function LogoGallery() {
  return (
    <div className="logo-gallery">
      {VARIANTS.map((variant) => (
        <section key={variant.label} className="logo-gallery-item">
          <div className="logo-gallery-label">{variant.label}</div>
          <div className="logo-gallery-note">{variant.note}</div>
          <div className="logo-gallery-strip">
            {SIZES.map((size) => (
              <PixelWordmark key={size} word="OpenAPPA" capHeight={size} {...variant.options} />
            ))}
          </div>
        </section>
      ))}
    </div>
  );
}

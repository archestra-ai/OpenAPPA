"use client";

import { useEffect, useRef, useState } from "react";

import { CurlPanel } from "@/components/CurlPanel";
import { McpPanel } from "@/components/McpPanel";
import type { TocItem } from "@/lib/docs";

/* The active entry is derived from the current scroll offset, not from
   IntersectionObserver events. An observer only reports headings that cross
   its band, so any scroll longer than the band — a wheel flick, PageDown, or
   an anchor jump, which lands the target above the band entirely — left the
   highlight on whatever heading happened to cross last. Reading positions on
   every scroll makes the answer a function of where the page is, so it cannot
   desynchronize. */
function activeHeadingId(ids: string[], readingLine: number): string | null {
  let active: string | null = null;
  for (const id of ids) {
    const el = document.getElementById(id);
    if (!el) continue;
    if (el.getBoundingClientRect().top > readingLine) break;
    active = id;
  }
  // Nothing above the line yet: the first section owns the top of the page.
  if (active === null) {
    const first = ids.find((id) => document.getElementById(id) !== null);
    return first ?? null;
  }
  return active;
}

export function TableOfContents({ items }: { items: TocItem[] }) {
  const [activeId, setActiveId] = useState<string | null>(null);
  const navRef = useRef<HTMLElement>(null);

  useEffect(() => {
    const ids = items.map((item) => item.id);
    if (ids.length === 0) return;

    let frame = 0;
    const update = () => {
      frame = 0;
      const headerHeight = parseFloat(
        getComputedStyle(document.documentElement).getPropertyValue("--header-h"),
      );
      /* An anchor jump lands the heading exactly at its scroll-margin, which is
         the header plus 1.5rem. The reading line has to sit below that landing
         point, or sub-pixel rounding decides whether the heading you just
         jumped to counts as reached — and half the time it selects the
         previous one. */
      const anchorLanding = (Number.isFinite(headerHeight) ? headerHeight : 73) + 24;
      const readingLine = anchorLanding + 12;

      // The last section can be shorter than the remaining viewport, so it
      // would never reach the reading line; the end of the page selects it.
      const atBottom =
        window.innerHeight + window.scrollY >= document.documentElement.scrollHeight - 2;
      const last = [...ids].reverse().find((id) => document.getElementById(id) !== null) ?? null;

      setActiveId(atBottom ? last : activeHeadingId(ids, readingLine));
    };
    const schedule = () => {
      if (frame === 0) frame = requestAnimationFrame(update);
    };

    update();
    window.addEventListener("scroll", schedule, { passive: true });
    window.addEventListener("resize", schedule);
    return () => {
      if (frame !== 0) cancelAnimationFrame(frame);
      window.removeEventListener("scroll", schedule);
      window.removeEventListener("resize", schedule);
    };
  }, [items]);

  /* Long tables of contents scroll inside their own rail. Keep the active
     entry visible by scrolling that rail only — never the page, which would
     fight the scroll that selected the entry in the first place. */
  useEffect(() => {
    const nav = navRef.current;
    if (!nav || activeId === null) return;
    const rail = nav.closest<HTMLElement>(".toc-inner");
    if (!rail || rail.scrollHeight <= rail.clientHeight) return;

    const link = nav.querySelector<HTMLElement>(`a[href="#${CSS.escape(activeId)}"]`);
    if (!link) return;

    const top = link.offsetTop - rail.offsetTop;
    const bottom = top + link.offsetHeight;
    if (top < rail.scrollTop) rail.scrollTop = top;
    else if (bottom > rail.scrollTop + rail.clientHeight) rail.scrollTop = bottom - rail.clientHeight;
  }, [activeId]);

  return (
    <aside className="toc">
      <div className="toc-inner">
        {items.length > 0 && (
          <>
            <div className="toc-title">On this page</div>
            <nav ref={navRef}>
              {items.map((item) => (
                <a
                  key={item.id}
                  href={`#${item.id}`}
                  aria-current={activeId === item.id ? "location" : undefined}
                  className={[
                    item.level === 3 ? "depth-3" : "",
                    activeId === item.id ? "active" : "",
                    item.proposal ? "toc-proposal" : "",
                  ]
                    .filter(Boolean)
                    .join(" ")}
                >
                  {item.text}
                </a>
              ))}
            </nav>
          </>
        )}
        <McpPanel />
        <CurlPanel />
      </div>
    </aside>
  );
}

"use client";

import { useEffect, useState } from "react";

import type { TocItem } from "@/lib/docs";

export function TableOfContents({ items }: { items: TocItem[] }) {
  const [activeId, setActiveId] = useState<string | null>(null);

  useEffect(() => {
    const headings = items
      .map((item) => document.getElementById(item.id))
      .filter((el): el is HTMLElement => el !== null);
    if (headings.length === 0) return;

    const observer = new IntersectionObserver(
      (entries) => {
        for (const entry of entries) {
          if (entry.isIntersecting) {
            setActiveId(entry.target.id);
            break;
          }
        }
      },
      { rootMargin: "-70px 0% -70% 0%" },
    );
    headings.forEach((el) => observer.observe(el));
    return () => observer.disconnect();
  }, [items]);

  if (items.length === 0) return null;

  return (
    <aside className="toc">
      <div className="toc-inner">
        <div className="toc-title">On this page</div>
        <nav>
          {items.map((item) => (
            <a
              key={item.id}
              href={`#${item.id}`}
              className={[
                item.level === 3 ? "depth-3" : "",
                activeId === item.id ? "active" : "",
              ]
                .filter(Boolean)
                .join(" ")}
            >
              {item.text}
            </a>
          ))}
        </nav>
      </div>
    </aside>
  );
}

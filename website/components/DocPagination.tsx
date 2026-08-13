"use client";

import Link from "next/link";
import { usePathname } from "next/navigation";

export interface PaginationDoc {
  slug: string;
  title: string;
}

export function DocPagination({ docs }: { docs: PaginationDoc[] }) {
  const pathname = usePathname();
  const currentSlug = pathname === "/" ? "index" : pathname.replace(/^\//, "");
  const currentIndex = docs.findIndex((d) => d.slug === currentSlug);

  if (currentIndex === -1) return null;

  const prevDoc = currentIndex > 0 ? docs[currentIndex - 1] : null;
  const nextDoc = currentIndex < docs.length - 1 ? docs[currentIndex + 1] : null;

  if (!prevDoc && !nextDoc) return null;

  return (
    <nav className="doc-pagination" aria-label="Pagination">
      {prevDoc ? (
        <Link
          href={prevDoc.slug === "index" ? "/" : `/${prevDoc.slug}`}
          className="pagination-card prev"
        >
          <span className="pagination-label">← Previous</span>
          <span className="pagination-title">{prevDoc.title}</span>
        </Link>
      ) : (
        <div />
      )}

      {nextDoc && (
        <Link
          href={nextDoc.slug === "index" ? "/" : `/${nextDoc.slug}`}
          className="pagination-card next"
        >
          <span className="pagination-label">Next →</span>
          <span className="pagination-title">{nextDoc.title}</span>
        </Link>
      )}
    </nav>
  );
}

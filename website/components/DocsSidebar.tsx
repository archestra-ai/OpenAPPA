"use client";

import Link from "next/link";
import { usePathname } from "next/navigation";

export interface SidebarCategory {
  name: string;
  docs: { slug: string; title: string }[];
}

function docHref(slug: string): string {
  return slug === "index" ? "/" : `/docs/${slug}`;
}

export function DocsSidebar({ categories }: { categories: SidebarCategory[] }) {
  const pathname = usePathname();

  return (
    <aside className="sidebar">
      <div className="sidebar-inner">
        {categories.map((category) => (
          <div key={category.name}>
            <div className="sidebar-category">{category.name}</div>
            <ul>
              {category.docs.map((doc) => {
                const href = docHref(doc.slug);
                return (
                  <li key={doc.slug}>
                    <Link href={href} aria-current={pathname === href ? "page" : undefined}>
                      {doc.title}
                    </Link>
                  </li>
                );
              })}
            </ul>
          </div>
        ))}
      </div>
    </aside>
  );
}

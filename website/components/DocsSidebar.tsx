"use client";

import { useCallback, useEffect } from "react";
import Link from "next/link";
import { usePathname } from "next/navigation";

import { GitHubSoon } from "@/components/GitHubSoon";
import { useDrawerDismissal, useMobileNav } from "@/components/MobileNav";
import { SearchIcon, useSearch } from "@/components/SearchProvider";
import { ThemeToggle } from "@/components/ThemeToggle";
import type { TocItem } from "@/lib/docs";

export interface SidebarCategory {
  name: string;
  docs: { slug: string; title: string }[];
}

function docHref(slug: string): string {
  return slug === "index" ? "/" : `/${slug}`;
}

export function DocsSidebar({
  categories,
  toc = [],
}: {
  categories: SidebarCategory[];
  toc?: TocItem[];
}) {
  const pathname = usePathname();
  const nav = useMobileNav();
  const search = useSearch();
  const open = nav?.open ?? false;
  const registerNav = nav?.registerNav;

  /* Close over `setOpen`, not over `nav`: the context value changes identity
     on every open, and a `close` that changed with it would make the
     route-change effect fire on open and shut the drawer immediately. */
  const setOpen = nav?.setOpen;
  const close = useCallback(() => setOpen?.(false), [setOpen]);

  useEffect(() => registerNav?.(), [registerNav]);
  useDrawerDismissal(open, close, pathname);

  return (
    <aside className={`sidebar${open ? " open" : ""}`}>
      <div className="nav-scrim" onClick={close} aria-hidden="true" />
      <div className="sidebar-inner" id="docs-nav">
        {/* Below 1024px the header keeps only the menu button and the
            wordmark; everything else it used to carry lives here. */}
        <div className="sidebar-chrome">
          <button
            type="button"
            className="sidebar-chrome-search"
            onClick={() => {
              close();
              search?.open();
            }}
          >
            <SearchIcon />
            <span>Search documentation</span>
          </button>
          <div className="sidebar-chrome-row">
            <a href="https://arxiv.org/abs/2607.24625" target="_blank" rel="noreferrer">
              Paper
            </a>
            <GitHubSoon />
            <ThemeToggle />
          </div>
        </div>
        <div>
          <div className="sidebar-category">Playground</div>
          <ul>
            <li>
              <Link href="/chat" aria-current={pathname === "/chat" ? "page" : undefined}>
                Chat with OpenAPPA
              </Link>
            </li>
          </ul>
        </div>
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
        {/* The TOC column is desktop-only, so the drawer carries the page's own
            sections — otherwise they are unreachable below 1280px. */}
        {toc.length > 0 && (
          <div className="sidebar-toc">
            <div className="sidebar-category">On this page</div>
            <ul>
              {toc.map((item) => (
                <li key={item.id}>
                  <a href={`#${item.id}`} className={item.level === 3 ? "depth-3" : ""} onClick={close}>
                    {item.text}
                  </a>
                </li>
              ))}
            </ul>
          </div>
        )}
      </div>
    </aside>
  );
}

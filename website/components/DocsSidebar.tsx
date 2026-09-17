"use client";

import { Fragment, useCallback, useEffect } from "react";
import Link from "next/link";
import { usePathname } from "next/navigation";

import { useDrawerDismissal, useMobileNav } from "@/components/MobileNav";
import { SearchIcon, useSearch } from "@/components/SearchProvider";
import { ThemeToggle } from "@/components/ThemeToggle";
import type { TocItem } from "@/lib/docs";

export interface SidebarCategory {
  name: string;
  docs: { slug: string; title: string; proposal: boolean }[];
}

export const PLAYGROUND_HREF = "/playground";

/* The shell is rendered by the page, not the layout, so every navigation
   unmounts the rail and mounts a fresh one scrolled to the top. Carry the
   offset across those mounts in module state — it outlives the component but
   not the tab, which is exactly the lifetime a reader expects. */
let railScrollTop = 0;

/** Clicking the playground link while already in it starts a fresh session. */
export const NEW_CHAT_EVENT = "appa:new-chat";

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

  /* A ref callback rather than an effect: it runs in the commit phase, before
     the browser paints the remounted rail, so the restored offset never shows
     as a jump back from the top. */
  const railRef = useCallback((el: HTMLDivElement | null) => {
    if (!el) return;
    el.scrollTop = railScrollTop;
    const remember = () => {
      railScrollTop = el.scrollTop;
    };
    el.addEventListener("scroll", remember, { passive: true });
    return () => el.removeEventListener("scroll", remember);
  }, []);

  return (
    <aside className={`sidebar${open ? " open" : ""}`}>
      <div className="nav-scrim" onClick={close} aria-hidden="true" />
      <div className="sidebar-inner" id="docs-nav" ref={railRef}>
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
            <a href="/paper" target="_blank" rel="noreferrer">
              Paper
            </a>
            <a href="https://github.com/archestra-ai/openappa" target="_blank" rel="noreferrer">
              GitHub
            </a>
            <a href="https://discord.gg/B5fmSxHKZ7" target="_blank" rel="noreferrer">
              Discord
            </a>
            <ThemeToggle />
          </div>
        </div>
        {categories.map((category) => (
          <div key={category.name}>
            <div className="sidebar-category">{category.name}</div>
            <ul>
              {category.docs.map((doc) => {
                const href = docHref(doc.slug);
                return (
                  <Fragment key={doc.slug}>
                    <li>
                      <Link href={href} aria-current={pathname === href ? "page" : undefined}>
                        {doc.title}
                        {doc.proposal && (
                          <span className="sidebar-proposal" aria-label="Proposal">
                            🚧
                          </span>
                        )}
                      </Link>
                    </li>
                    {/* The playground sits with the pages that introduce the
                        model, directly under the one that opens it. */}
                    {doc.slug === "index" && (
                      <li>
                        <Link
                          aria-current={pathname === PLAYGROUND_HREF ? "page" : undefined}
                          href={PLAYGROUND_HREF}
                          onClick={(event) => {
                            // Already here: the link cannot navigate, so make it
                            // do the thing it looks like it does — start over.
                            if (pathname === PLAYGROUND_HREF) {
                              event.preventDefault();
                              window.dispatchEvent(new CustomEvent(NEW_CHAT_EVENT));
                            }
                            close();
                          }}
                        >
                          Playground
                        </Link>
                      </li>
                    )}
                  </Fragment>
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

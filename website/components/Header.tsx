"use client";

import { useEffect, useRef } from "react";
import Link from "next/link";

import { DiscordIcon } from "@/components/DiscordIcon";
import { Logo } from "@/components/Logo";
import { MobileNavToggle } from "@/components/MobileNav";
import { SearchIcon, useSearch } from "@/components/SearchProvider";
import { ThemeToggle } from "@/components/ThemeToggle";

export function Header({ fullBleed = false }: { fullBleed?: boolean }) {
  const search = useSearch();
  const headerRef = useRef<HTMLElement>(null);

  /* Everything that has to clear the sticky header — anchor landings, the
     sticky rails, the drawer — reads --header-h. Publishing the measured
     height keeps them exact when the header reflows (font swap, narrow
     breakpoints) instead of drifting from a hardcoded guess. */
  useEffect(() => {
    const el = headerRef.current;
    if (!el) return;
    const publish = () => {
      document.documentElement.style.setProperty("--header-h", `${Math.round(el.getBoundingClientRect().height)}px`);
    };
    publish();
    const observer = new ResizeObserver(publish);
    observer.observe(el);
    return () => observer.disconnect();
  }, []);

  return (
    <>
      {/* Site pages align the header's contents to the centered reading
          column; an app screen (the chat) spans the viewport, so its header
          keeps edge padding to match. */}
      <header className={fullBleed ? "site-header site-header-full" : "site-header"} ref={headerRef}>
        <MobileNavToggle />
        <Link href="/" className="wordmark">
          <Logo height={15} />
        </Link>
        <nav>
          <button
            type="button"
            className="search-trigger-btn"
            onClick={() => search?.open()}
            aria-label="Search documentation"
          >
            <SearchIcon />
            <span>Search...</span>
            <kbd className="header-kbd">⌘K</kbd>
          </button>
          <Link href="/" className="nav-docs">
            Docs
          </Link>
          <a href="https://arxiv.org/abs/2607.24625" target="_blank" rel="noreferrer">
            Paper
          </a>
          <a href="https://github.com/archestra-ai/openappa" target="_blank" rel="noreferrer">
            GitHub
          </a>
          <a
            href="https://discord.gg/B5fmSxHKZ7"
            target="_blank"
            rel="noreferrer"
            className="discord-link"
            aria-label="Join the OpenAPPA Discord"
            title="Join the OpenAPPA Discord"
          >
            <DiscordIcon />
          </a>
          <ThemeToggle />
        </nav>
      </header>
    </>
  );
}

"use client";

import { useEffect, useState } from "react";
import Link from "next/link";

import { Logo } from "@/components/Logo";
import { SearchModal } from "@/components/SearchModal";

export function Header() {
  const [searchOpen, setSearchOpen] = useState(false);

  useEffect(() => {
    const handleKeyDown = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "k") {
        e.preventDefault();
        setSearchOpen((open) => !open);
      } else if (e.key === "/" && document.activeElement?.tagName !== "INPUT" && document.activeElement?.tagName !== "TEXTAREA") {
        e.preventDefault();
        setSearchOpen(true);
      }
    };
    window.addEventListener("keydown", handleKeyDown);
    return () => window.removeEventListener("keydown", handleKeyDown);
  }, []);

  return (
    <>
      <header className="site-header">
        <Link href="/" className="wordmark">
          <Logo height={15} />
        </Link>
        <nav>
          <button
            type="button"
            className="search-trigger-btn"
            onClick={() => setSearchOpen(true)}
            aria-label="Search documentation"
          >
            <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
              <circle cx="11" cy="11" r="8" />
              <line x1="21" y1="21" x2="16.65" y2="16.65" />
            </svg>
            <span>Search...</span>
            <kbd className="header-kbd">⌘K</kbd>
          </button>
          <Link href="/">Docs</Link>
          <a href="https://arxiv.org/abs/2607.24625" target="_blank" rel="noreferrer">
            Paper
          </a>
          <a href="https://github.com/archestra-ai/OpenAPPA" target="_blank" rel="noreferrer">
            GitHub
          </a>
        </nav>
      </header>
      <SearchModal isOpen={searchOpen} onClose={() => setSearchOpen(false)} />
    </>
  );
}

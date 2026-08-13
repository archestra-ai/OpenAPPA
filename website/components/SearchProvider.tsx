"use client";

import { createContext, useCallback, useContext, useEffect, useMemo, useState } from "react";

import { SearchModal } from "@/components/SearchModal";

/* Search is opened from two places that live in different subtrees — the header
   button on wide screens, the drawer item on narrow ones — so the state sits
   above both, next to the modal it controls. The keyboard shortcuts live here
   too: they belong to search, not to the header that used to host them. */

interface SearchState {
  open: () => void;
}

const SearchContext = createContext<SearchState | null>(null);

export function SearchProvider({ children }: { children: React.ReactNode }) {
  const [isOpen, setIsOpen] = useState(false);

  useEffect(() => {
    const handleKeyDown = (e: KeyboardEvent) => {
      const tag = document.activeElement?.tagName;
      if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "k") {
        e.preventDefault();
        setIsOpen((open) => !open);
      } else if (e.key === "/" && tag !== "INPUT" && tag !== "TEXTAREA") {
        e.preventDefault();
        setIsOpen(true);
      }
    };
    window.addEventListener("keydown", handleKeyDown);
    return () => window.removeEventListener("keydown", handleKeyDown);
  }, []);

  const open = useCallback(() => setIsOpen(true), []);
  const value = useMemo(() => ({ open }), [open]);

  return (
    <SearchContext.Provider value={value}>
      {children}
      <SearchModal isOpen={isOpen} onClose={() => setIsOpen(false)} />
    </SearchContext.Provider>
  );
}

/** Null outside the provider so a stray consumer renders instead of throwing. */
export function useSearch(): SearchState | null {
  return useContext(SearchContext);
}

export function SearchIcon() {
  return (
    <svg
      width="14"
      height="14"
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="2"
      aria-hidden="true"
    >
      <circle cx="11" cy="11" r="8" />
      <line x1="21" y1="21" x2="16.65" y2="16.65" />
    </svg>
  );
}

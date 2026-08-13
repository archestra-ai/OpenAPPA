"use client";

import { createContext, useCallback, useContext, useEffect, useMemo, useState } from "react";

/* The docs navigation lives in the left rail on wide screens. Below 1024px the
   rail is a drawer, and its trigger has to sit in the sticky header: a grid
   item cannot be sticky beyond its own row, so a trigger rendered inside the
   rail would scroll away on the first swipe. Header and rail are separate
   subtrees under the root layout, so the open state is shared through this
   context rather than passed down. */

interface MobileNavState {
  open: boolean;
  setOpen: (open: boolean) => void;
  /** The trigger renders only on routes that actually mount a nav rail. */
  hasNav: boolean;
  registerNav: () => () => void;
}

const MobileNavContext = createContext<MobileNavState | null>(null);

export function MobileNavProvider({ children }: { children: React.ReactNode }) {
  const [open, setOpen] = useState(false);
  const [navCount, setNavCount] = useState(0);

  const registerNav = useCallback(() => {
    setNavCount((n) => n + 1);
    return () => setNavCount((n) => n - 1);
  }, []);

  const value = useMemo(
    () => ({ open, setOpen, hasNav: navCount > 0, registerNav }),
    [open, navCount, registerNav],
  );

  return <MobileNavContext.Provider value={value}>{children}</MobileNavContext.Provider>;
}

/** Null outside the provider so a stray <Header /> still renders. */
export function useMobileNav(): MobileNavState | null {
  return useContext(MobileNavContext);
}

export function MobileNavToggle() {
  const nav = useMobileNav();
  if (!nav || !nav.hasNav) return null;

  return (
    <button
      type="button"
      className="nav-toggle"
      aria-expanded={nav.open}
      aria-controls="docs-nav"
      aria-label={nav.open ? "Close navigation" : "Open navigation"}
      onClick={() => nav.setOpen(!nav.open)}
    >
      <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
        {nav.open ? (
          <>
            <line x1="18" y1="6" x2="6" y2="18" />
            <line x1="6" y1="6" x2="18" y2="18" />
          </>
        ) : (
          <>
            <line x1="3" y1="6" x2="21" y2="6" />
            <line x1="3" y1="12" x2="21" y2="12" />
            <line x1="3" y1="18" x2="21" y2="18" />
          </>
        )}
      </svg>
    </button>
  );
}

/** Closes the drawer on Escape, on route change, and once the rail is docked. */
export function useDrawerDismissal(open: boolean, close: () => void, pathname: string) {
  useEffect(() => {
    close();
  }, [pathname, close]);

  useEffect(() => {
    if (!open) return;

    const onKeyDown = (e: KeyboardEvent) => {
      if (e.key === "Escape") close();
    };
    const docked = window.matchMedia("(min-width: 1024px)");
    const onDocked = () => {
      if (docked.matches) close();
    };

    window.addEventListener("keydown", onKeyDown);
    docked.addEventListener("change", onDocked);

    const previousOverflow = document.body.style.overflow;
    document.body.style.overflow = "hidden";

    return () => {
      window.removeEventListener("keydown", onKeyDown);
      docked.removeEventListener("change", onDocked);
      document.body.style.overflow = previousOverflow;
    };
  }, [open, close]);
}

"use client";

import { useEffect, useRef, useState } from "react";

/* An inline-code term carrying its definition in a popover: hover on
   desktop, tap to toggle on touch, Enter when focused, Escape or an
   outside tap to dismiss. */

export function Term({ chip, definition }: { chip: string; definition: string }) {
  const [open, setOpen] = useState(false);
  const wrapRef = useRef<HTMLSpanElement>(null);

  useEffect(() => {
    if (!open) return;
    const onPointerDown = (event: PointerEvent) => {
      if (!wrapRef.current?.contains(event.target as Node)) setOpen(false);
    };
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") setOpen(false);
    };
    document.addEventListener("pointerdown", onPointerDown);
    document.addEventListener("keydown", onKeyDown);
    return () => {
      document.removeEventListener("pointerdown", onPointerDown);
      document.removeEventListener("keydown", onKeyDown);
    };
  }, [open]);

  return (
    <span
      ref={wrapRef}
      className="term"
      onPointerEnter={(event) => {
        if (event.pointerType === "mouse") setOpen(true);
      }}
      onPointerLeave={(event) => {
        if (event.pointerType === "mouse") setOpen(false);
      }}
    >
      <button
        type="button"
        className="term-trigger"
        aria-expanded={open}
        onClick={() => setOpen((current) => !current)}
      >
        <code>{chip}</code>
      </button>
      {open && (
        <span role="tooltip" className="term-popover">
          {definition}
        </span>
      )}
    </span>
  );
}

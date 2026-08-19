"use client";

import { useEffect, useRef, useState } from "react";

/* First-visit prompt: the reader may leave an address, and the team gets one
   notification that they stopped by. Nothing is stored — see
   `app/api/reader-ping/route.ts`, which has the only sink.

   The prompt is dismissable by design. This is an open-source project whose
   docs are the product surface; a wall in front of them would cost more
   readers than the addresses are worth. */

const SEEN_KEY = "openappa-reader-ping";

/* Long enough that the page has painted and the reader has seen what they came
   for, short enough that the prompt is still part of arriving rather than an
   interruption partway through a paragraph. */
const APPEAR_DELAY_MS = 1_400;

type Status =
  | { state: "idle" }
  | { state: "sending" }
  | { state: "sent" }
  | { state: "error"; message: string };

function alreadySeen(): boolean {
  try {
    return localStorage.getItem(SEEN_KEY) !== null;
  } catch {
    // Private-mode storage failures must not turn this into a prompt on every
    // page view; when we cannot remember the reader, we do not ask.
    return true;
  }
}

// Bypasses the "already seen" check for previewing the prompt without
// clearing localStorage — ?reader-ping=1 on any page.
function forcedOpen(): boolean {
  return new URLSearchParams(window.location.search).get("reader-ping") === "1";
}

function remember(outcome: "sent" | "skipped") {
  try {
    localStorage.setItem(SEEN_KEY, outcome);
  } catch {
    /* Nothing to do: the prompt simply may appear again next visit. */
  }
}

export function ReaderPing() {
  const [open, setOpen] = useState(false);
  const [closing, setClosing] = useState(false);
  const [email, setEmail] = useState("");
  const [status, setStatus] = useState<Status>({ state: "idle" });

  const inputRef = useRef<HTMLInputElement>(null);
  const dialogRef = useRef<HTMLDivElement>(null);
  const restoreFocusTo = useRef<Element | null>(null);

  useEffect(() => {
    const forced = forcedOpen();
    if (!forced && alreadySeen()) return;
    // Forced previews skip the arrival delay: whoever added the param wants
    // to see the dialog, not wait for it.
    const timer = setTimeout(() => setOpen(true), forced ? 0 : APPEAR_DELAY_MS);
    return () => clearTimeout(timer);
  }, []);

  useEffect(() => {
    if (!open) return;
    restoreFocusTo.current = document.activeElement;
    // Waits a frame so the entry transition does not fight the focus scroll.
    const timer = setTimeout(() => inputRef.current?.focus(), 60);
    return () => clearTimeout(timer);
  }, [open]);

  function dismiss(outcome: "sent" | "skipped") {
    remember(outcome);
    setClosing(true);
    setTimeout(() => {
      setOpen(false);
      if (restoreFocusTo.current instanceof HTMLElement) restoreFocusTo.current.focus();
    }, 160);
  }

  useEffect(() => {
    if (!open) return;

    function onKeyDown(event: KeyboardEvent) {
      if (event.key === "Escape") {
        event.preventDefault();
        dismiss("skipped");
        return;
      }
      /* A modal must not hand focus back to the page behind it. The dialog has
         few enough controls to cycle them directly rather than pull in a
         focus-trap dependency. */
      if (event.key !== "Tab" || !dialogRef.current) return;
      const focusable = dialogRef.current.querySelectorAll<HTMLElement>(
        'button:not(:disabled), input:not(:disabled), [href], [tabindex]:not([tabindex="-1"])',
      );
      if (focusable.length === 0) return;
      const first = focusable[0];
      const last = focusable[focusable.length - 1];
      if (event.shiftKey && document.activeElement === first) {
        event.preventDefault();
        last.focus();
      } else if (!event.shiftKey && document.activeElement === last) {
        event.preventDefault();
        first.focus();
      }
    }

    document.addEventListener("keydown", onKeyDown);
    return () => document.removeEventListener("keydown", onKeyDown);
  }, [open]);

  async function onSubmit(event: React.FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (status.state === "sending") return;
    setStatus({ state: "sending" });

    try {
      const response = await fetch("/api/reader-ping", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ email, path: window.location.pathname }),
      });

      if (response.ok) {
        setStatus({ state: "sent" });
        setEmail("");
        // Lets the reader actually see the confirmation before it disappears.
        setTimeout(() => dismiss("sent"), 1_500);
        return;
      }

      const result = await response.json().catch(() => null);
      setStatus({
        state: "error",
        message: result?.error ?? "That didn't go through. Please try again.",
      });
    } catch {
      setStatus({ state: "error", message: "Could not reach the team. Please try again." });
    }
  }

  if (!open) return null;

  const sending = status.state === "sending";
  const sent = status.state === "sent";

  /* The backdrop deliberately does not dismiss: a stray click beside the dialog
     should not count as an answer. Escape is the only way out. */
  return (
    <div className={`ping-overlay${closing ? " is-closing" : ""}`}>
      <div
        className="ping-dialog"
        ref={dialogRef}
        role="dialog"
        aria-modal="true"
        aria-labelledby="ping-title"
        aria-describedby="ping-lede"
      >
        {/* Matches the badge beside the wordmark in the header, so the prompt
            reads as part of the project rather than a bolted-on capture form. */}
        <p className="ping-eyebrow">Preview &amp; RFC</p>
        <h2 className="ping-title" id="ping-title">
          Say hello before you read
        </h2>
        <p className="ping-lede" id="ping-lede">
          OpenAPPA is in preview and the model is still open for argument. We would love to know who
          is checking it out.
        </p>

        <form className="ping-form" onSubmit={onSubmit}>
          <label className="ping-label" htmlFor="ping-email">
            Email address
          </label>
          <div className="ping-controls">
            <input
              id="ping-email"
              className="ping-input"
              ref={inputRef}
              type="email"
              name="email"
              value={email}
              onChange={(e) => setEmail(e.target.value)}
              placeholder="you@company.com"
              autoComplete="email"
              required
              disabled={sending || sent}
            />
            <button
              className="ping-button"
              type="submit"
              disabled={sending || sent || email.trim() === ""}
            >
              {sending ? "Sending…" : sent ? "Sent" : "Say hello"}
            </button>
          </div>
        </form>

        {/* Reserves its line so the dialog does not jump when a result lands. */}
        <p
          className={`ping-status${sent ? " ok" : status.state === "error" ? " error" : ""}`}
          role="status"
          aria-live="polite"
        >
          {sent
            ? "Thank you. The team has been notified — and that is the end of it."
            : status.state === "error"
              ? status.message
              : ""}
        </p>
      </div>
    </div>
  );
}

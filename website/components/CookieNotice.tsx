"use client";

import { useEffect, useState } from "react";

import { readConsent, subscribeToConsent, writeConsent } from "@/lib/analytics-consent";
import { isReaderPingPending, READER_PING_RESOLVED_EVENT } from "./ReaderPing";

/* The one question this site asks about measurement. Until it is answered
   PostHog stays opted out (see `Analytics.tsx`), so the notice is a request
   rather than an announcement of something already happening.

   It is a bar, not a modal: the docs are the product surface here, and a reader
   who ignores this should still be able to read every word on the page. That is
   also why there is no close button — dismissing without answering would just
   be "no" wearing a disguise, and "No thanks" already says it plainly. */

/* Long enough that the notice arrives after the page has painted and the reader
   has their bearings. Deliberately longer than ReaderPing's own delay so that
   on a first visit the two are sequenced rather than racing. */
const APPEAR_DELAY_MS = 900;

export function CookieNotice() {
  const [visible, setVisible] = useState(false);
  const [leaving, setLeaving] = useState(false);

  useEffect(() => {
    if (readConsent() !== null) return;

    let timer: ReturnType<typeof setTimeout> | undefined;
    const show = () => {
      timer = setTimeout(() => setVisible(true), APPEAR_DELAY_MS);
    };

    if (isReaderPingPending()) {
      /* Wait our turn. If the reader never answers that prompt they never see
         this one either, which is the correct outcome: they are not being
         measured, and nothing is being stored about them. */
      window.addEventListener(READER_PING_RESOLVED_EVENT, show, { once: true });
      return () => {
        window.removeEventListener(READER_PING_RESOLVED_EVENT, show);
        clearTimeout(timer);
      };
    }

    show();
    return () => clearTimeout(timer);
  }, []);

  /* Answering in one tab should retire the notice in the others too. */
  useEffect(() => {
    return subscribeToConsent(() => {
      if (readConsent() !== null) setVisible(false);
    });
  }, []);

  if (!visible) return null;

  const answer = (decision: "granted" | "denied") => {
    /* Recorded first: the state below is only how this tab stops drawing the
       bar, while the write is what actually settles the question. */
    writeConsent(decision);
    setLeaving(true);
    setTimeout(() => setVisible(false), 160);
  };

  return (
    <div
      className={`cookie-notice${leaving ? " is-closing" : ""}`}
      role="region"
      aria-label="Analytics consent"
    >
      <p className="cookie-notice-text">
        We would like to count visits and record browsing sessions so we know which parts of
        OpenAPPA people actually read, and where they get stuck. Anything you type is masked
        before it leaves the page. No advertising, no third-party sharing.
      </p>
      <div className="cookie-notice-actions">
        <button type="button" className="cookie-notice-skip" onClick={() => answer("denied")}>
          No thanks
        </button>
        <button type="button" className="cookie-notice-accept" onClick={() => answer("granted")}>
          Allow
        </button>
      </div>
    </div>
  );
}

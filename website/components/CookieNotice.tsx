"use client";

import { useEffect, useState } from "react";

import { readConsent, subscribeToConsent, writeConsent } from "@/lib/analytics-consent";

/* The one question this site asks about measurement. Until it is answered
   PostHog stays opted out (see `Analytics.tsx`), so the notice is a request
   rather than an announcement of something already happening.

   It is a bar, not a modal: the docs are the product surface here, and a reader
   who ignores this should still be able to read every word on the page. That is
   also why there is no close button — dismissing without answering would just
   be "no" wearing a disguise, and "Decline" already says it plainly. */

/* Long enough that the notice arrives after the page has painted and the reader
   has their bearings. */
const APPEAR_DELAY_MS = 900;

export function CookieNotice() {
  const [visible, setVisible] = useState(false);
  const [leaving, setLeaving] = useState(false);

  useEffect(() => {
    if (readConsent() !== null) return;

    const timer = setTimeout(() => setVisible(true), APPEAR_DELAY_MS);
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
        This site uses analytics cookies to measure page usage. No analytics data is
        collected until you consent.
      </p>
      <div className="cookie-notice-actions">
        <button type="button" className="cookie-notice-skip" onClick={() => answer("denied")}>
          Decline
        </button>
        <button type="button" className="cookie-notice-accept" onClick={() => answer("granted")}>
          Accept
        </button>
      </div>
    </div>
  );
}

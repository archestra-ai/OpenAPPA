"use client";

import { usePathname, useSearchParams } from "next/navigation";
import { Suspense, useEffect, useState } from "react";

import { readConsent, subscribeToConsent } from "@/lib/analytics-consent";

/* PostHog, started only once the reader has said yes.

   The obvious way to write this is to import the library at the top of the
   file, initialise it on load, and rely on `opt_out_capturing_by_default` to
   hold events back until consent. That is not enough, in two separate ways.

   Initialising fetches remote config and evaluates feature flags straight away,
   and those requests reach PostHog — carrying the reader's IP — whether or not
   capturing is opted out. It also writes the `ph_*` cookie and localStorage
   entry unless persistence is separately opted out of. Any of that happening
   before the notice is answered would make the notice decorative.

   A static import is also not free: the library is around 240 kB of JavaScript,
   and importing it from the root layout means every reader downloads it on
   every page, including the ones who decline. So consent gates the `import`
   itself, not just `init`. A reader who says no, or never answers, never
   fetches the chunk, never has a request made on their behalf, and never has
   anything written to their device.

   There is no React context here on purpose. `posthog-js` is a singleton, so a
   component that wants to record something can await the same module:

     const { default: posthog } = await import("posthog-js");
     posthog.capture("curl_copied", { page: "/contracts" });

   Wrapping the tree in a provider would buy nothing and put a re-render
   boundary around every page. */

type PostHogClient = Awaited<typeof import("posthog-js")>["default"];

/* Events go to PostHog's EU region, reached through the same-origin `/ingest`
   path rewritten in `next.config.ts`. `ui_host` plays no part in ingestion: it
   only tells the toolbar and the "view this in PostHog" links where the app
   lives. */
const UI_HOST = "https://eu.posthog.com";

const CONFIG = {
  api_host: "/ingest",
  ui_host: UI_HOST,
  /* Route changes in the App Router are not page loads, so PostHog's own
     pageview detection would miss every navigation after the first. The
     pageview effect below sends them instead. */
  capture_pageview: false,
  capture_pageleave: true,
  /* Both of these record far more about a reader than counting them requires,
     which is all this site is trying to do. */
  capture_heatmaps: false,
  disable_session_recording: true,
  /* The site drives none of these. Turning them off is not a privacy control —
     consent has already been given by the time we get here — it just avoids
     requests and runtime work for features nothing on the site consumes. */
  advanced_disable_flags: true,
  disable_surveys: true,
  disable_web_experiments: true,
} as const;

/* Module scope, so it survives the remount React performs in development and is
   shared by both effects below. Holding the promise rather than the client is
   what makes concurrent callers safe: the second one waits on the same import
   and `init` instead of racing a second one. */
let client: Promise<PostHogClient> | null = null;

function startPostHog(apiKey: string): Promise<PostHogClient> {
  if (!client) {
    client = import("posthog-js").then(({ default: posthog }) => {
      posthog.init(apiKey, CONFIG);
      return posthog;
    });
  }
  return client;
}

export function Analytics({ apiKey }: { apiKey: string }) {
  /* No key configured — a fork, or a preview deployment without the env var.
     The site must work untouched in that case rather than throwing. */
  if (!apiKey) return null;

  return (
    <Suspense fallback={null}>
      <AnalyticsInner apiKey={apiKey} />
    </Suspense>
  );
}

/* Split out because `useSearchParams` opts its whole component into client-side
   rendering, and Next requires that boundary to be a Suspense child. */
function AnalyticsInner({ apiKey }: { apiKey: string }) {
  const pathname = usePathname();
  const searchParams = useSearchParams();
  const [granted, setGranted] = useState(false);

  useEffect(() => {
    const sync = () => setGranted(readConsent() === "granted");
    sync();
    return subscribeToConsent(sync);
  }, []);

  useEffect(() => {
    if (!granted) {
      /* Withdrawal — the reader cleared their decision, or answered "no" in
         another tab after having agreed in this one. If `client` is null the
         library was never fetched, so there is nothing to switch off. */
      client?.then((ph) => ph.opt_out_capturing()).catch(ignore);
      return;
    }

    /* Covers re-granting after a withdrawal: PostHog remembers the opt-out
       across reloads, so a freshly initialised client can come back silenced. */
    startPostHog(apiKey)
      .then((ph) => {
        if (!ph.has_opted_in_capturing()) ph.opt_in_capturing();
      })
      .catch(ignore);
  }, [granted, apiKey]);

  useEffect(() => {
    if (!granted) return;

    /* Read now rather than inside the callback. The import may still be in
       flight, and by the time it lands the reader could have moved on — the
       pageview belongs to the page that was open when the effect ran. */
    const url = window.location.href;

    startPostHog(apiKey)
      .then((ph) => ph.capture("$pageview", { $current_url: url }))
      .catch(ignore);
  }, [granted, pathname, searchParams, apiKey]);

  return null;
}

/* A blocked or failed chunk fetch means no analytics, which is a perfectly fine
   state for this site to be in. It must never surface to the reader. */
function ignore() {}

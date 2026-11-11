"use client";

import { useEffect, useState } from "react";

import { demoBookingUrl, dismissTrinketVisit, onTrinketVisitChange, readTrinketVisit } from "@/lib/trinket";

/* A thin strip above the header for readers who arrived from a KubeCon
   trinket (lib/trinket), offering the demo the booth talked about. It stays
   while they move around the site, until dismissed or the page reloads. The
   visit is only known after hydration, so nothing renders on the server. */
export function TrinketDemoBanner() {
  const [href, setHref] = useState<string | null>(null);

  useEffect(() => {
    const update = () => {
      const visit = readTrinketVisit();
      setHref(visit && !visit.dismissed ? demoBookingUrl(visit.email) : null);
    };
    update();
    /* SongDeepLink records the visit in an effect that runs after this one
       on the first render, so listen for it rather than read once. */
    return onTrinketVisitChange(update);
  }, []);

  if (!href) return null;

  return (
    <div className="trinket-banner" role="region" aria-label="KubeCon demo">
      <span>
        Got an OpenAPPA trinket at KubeCon?{" "}
        <a href={href} target="_blank" rel="noreferrer">
          Schedule an OpenAPPA / Archestra demo →
        </a>
      </span>
      <button type="button" className="trinket-banner-close" onClick={dismissTrinketVisit} aria-label="Dismiss">
        ×
      </button>
    </div>
  );
}

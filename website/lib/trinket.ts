/* The OpenAPPA trinkets handed out at KubeCon + CloudNativeCon North America
   2026 in Salt Lake City carry an NFC tag pointing at `/nfc`. The booth app
   writes the attendee's email into that link when they gave one
   (`/nfc?email=…`), and reports the trinket to PostHog under that email.

   Arriving here, the parameters are moved out of the address, so a copied or
   shared link does not carry the email and no pageview URL records it. The tap
   itself is reported once, straight away, through `/api/trinket-scan`: the
   server sends TRINKET_SCAN_EVENT to PostHog without anything being stored on
   the reader's device. The email also waits in this tab's sessionStorage, and
   if the reader accepts analytics, `Analytics.tsx` identifies the session by
   it, so pageviews and session replay sit under the same person. A reader who
   declines gets nothing beyond the one tap event, and the stash disappears
   with the tab. */

export const TRINKET_SCAN_EVENT = "kubecon_slc_2026_openappa_trinket_scan";

export type TrinketScan = {
  /* The email the attendee typed at the booth, or null when they skipped it. */
  email: string | null;
  /* The trinket's random code, when the booth app writes one (`?t=`). */
  token: string | null;
  scannedAt: string;
};

const STORAGE_KEY = "openappa-trinket-scan";
const PARAMS = ["email", "t"] as const;

/** Records the trinket scan in `search` and returns `search` without its
    trinket parameters, ready to put back into the address. */
export function takeTrinketParams(search: string): string {
  const params = new URLSearchParams(search);
  const email = normalizeEmail(params.get("email"));
  const token = params.get("t")?.trim() || null;
  for (const name of PARAMS) params.delete(name);

  const scan: TrinketScan = { email, token, scannedAt: new Date().toISOString() };
  try {
    sessionStorage.setItem(STORAGE_KEY, JSON.stringify(scan));
  } catch {
    /* Blocked storage: the scan goes unmeasured, which is acceptable. */
  }

  reportTrinketScan(scan);
  rememberTrinketVisit({ email, dismissed: false });

  const rest = params.toString();
  return rest ? `?${rest}` : "";
}

/* The booking page for the demo the booth offers. */
const DEMO_URL = "https://calendly.com/joey-archestra/kubecon-slc-2026-openappa-archestra-demo";
const VISIT_EVENT = "openappa-trinket-visit";

/** The page load that arrived from a trinket, for the demo banner
    (TrinketDemoBanner). Held in memory only: it lasts while the reader moves
    around the site, and a reload or a later visit to openappa.com starts
    without it, so only a scan shows the banner. */
export type TrinketVisit = { email: string | null; dismissed: boolean };

let currentVisit: TrinketVisit | null = null;

export function readTrinketVisit(): TrinketVisit | null {
  return currentVisit;
}

export function dismissTrinketVisit(): void {
  if (currentVisit) rememberTrinketVisit({ ...currentVisit, dismissed: true });
}

/** Calls `onChange` when this tab records or dismisses a trinket visit. */
export function onTrinketVisitChange(onChange: () => void): () => void {
  window.addEventListener(VISIT_EVENT, onChange);
  return () => window.removeEventListener(VISIT_EVENT, onChange);
}

/** The demo booking page, with the email from the trinket filled in when it
    carried one. */
export function demoBookingUrl(email: string | null): string {
  const url = new URL(DEMO_URL);
  if (email) {
    url.searchParams.set("email", email);
    // The booking form also wants a name; the email stands in for it, so
    // there is nothing left to type.
    url.searchParams.set("name", email);
  }
  url.searchParams.set("utm_source", "openappa.com");
  url.searchParams.set("utm_campaign", "kubecon-slc-2026");
  return url.toString();
}

function rememberTrinketVisit(visit: TrinketVisit): void {
  currentVisit = visit;
  window.dispatchEvent(new Event(VISIT_EVENT));
}

/* Sends the tap to `/api/trinket-scan`. `keepalive` lets the request finish if
   the reader leaves the page at once; a failure only loses the count. */
function reportTrinketScan(scan: TrinketScan): void {
  try {
    void fetch("/api/trinket-scan", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(scan),
      keepalive: true,
    }).catch(() => {});
  } catch {
    /* No fetch: nothing to report with. */
  }
}

/** The scan waiting to be reported, if this tab arrived from a trinket. */
export function readTrinketScan(): TrinketScan | null {
  try {
    const raw = sessionStorage.getItem(STORAGE_KEY);
    if (!raw) return null;
    const parsed = JSON.parse(raw) as Partial<TrinketScan>;
    if (typeof parsed.scannedAt !== "string") return null;
    return {
      email: normalizeEmail(parsed.email ?? null),
      token: typeof parsed.token === "string" ? parsed.token : null,
      scannedAt: parsed.scannedAt,
    };
  } catch {
    return null;
  }
}

/** Forgets the scan, so it is reported once. */
export function clearTrinketScan(): void {
  try {
    sessionStorage.removeItem(STORAGE_KEY);
  } catch {
    /* Nothing stored, nothing to clear. */
  }
}

/** Something that looks like an address, lowercased; anything else is dropped
    rather than used as a PostHog identity. */
export function normalizeEmail(value: string | null | undefined): string | null {
  const email = value?.trim().toLowerCase();
  if (!email || email.length > 254) return null;
  return /^[^\s@]+@[^\s@]+\.[^\s@]+$/.test(email) ? email : null;
}

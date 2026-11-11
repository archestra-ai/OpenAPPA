import { TRINKET_SCAN_EVENT, normalizeEmail } from "@/lib/trinket";

/* Records one tap of a KubeCon trinket (lib/trinket) in PostHog, before and
   regardless of the analytics notice. The tap is the thing the booth handed
   out the trinket for, and the email in it is one the attendee typed into our
   booth app to be followed up with.

   It runs here rather than in the browser so that recording it stores nothing
   on the reader's device and loads no tracking script: the request carries the
   email and trinket code, the server forwards one event, and GeoIP lookup is
   turned off because the address PostHog would see is this server's, not the
   reader's. Everything else about the visit (pageviews, session replay, and
   linking them to this person) still waits for consent in Analytics.tsx. */

export const dynamic = "force-dynamic";

const CAPTURE_URL = "https://eu.i.posthog.com/i/v0/e/";

export async function POST(request: Request) {
  const apiKey = process.env.NEXT_PUBLIC_POSTHOG_KEY;
  if (!apiKey) return new Response(null, { status: 204 });

  let body: { email?: unknown; token?: unknown; scannedAt?: unknown };
  try {
    body = await request.json();
  } catch {
    return new Response(null, { status: 400 });
  }

  const email = normalizeEmail(typeof body.email === "string" ? body.email : null);
  const token = typeof body.token === "string" && /^[A-Za-z0-9]{1,32}$/.test(body.token) ? body.token : null;
  const scannedAt = typeof body.scannedAt === "string" && !Number.isNaN(Date.parse(body.scannedAt))
    ? body.scannedAt
    : new Date().toISOString();

  // Without an email there is no person to attach the tap to: count it
  // without creating one.
  const distinctId = email ?? `trinket-scan:${crypto.randomUUID()}`;

  try {
    const response = await fetch(CAPTURE_URL, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({
        api_key: apiKey,
        event: TRINKET_SCAN_EVENT,
        distinct_id: distinctId,
        timestamp: scannedAt,
        properties: {
          email,
          trinket_token: token,
          $geoip_disable: true,
          $process_person_profile: email !== null,
          ...(email ? { $set: { email } } : {}),
        },
      }),
      signal: AbortSignal.timeout(5000),
    });
    return new Response(null, { status: response.ok ? 204 : 502 });
  } catch {
    return new Response(null, { status: 502 });
  }
}

/* Reader ping — a first-visit address goes to the team's Slack and nowhere else.
   The modal promises the reader that nothing is stored, so this route must keep
   that promise literally: no database, no file, no analytics call, and no log
   line carrying the address in production. The only sink is the webhook. */

const WEBHOOK_URL = process.env.SLACK_WEBHOOK_URL;

/* Deliberately loose — this is a notification, not an identity check. It rejects
   the shapes that are certainly not addresses and lets everything else through,
   because a reader who mistypes their own address is not a case worth policing. */
const EMAIL = /^[^\s@]+@[^\s@]+\.[^\s@]+$/;
const MAX_EMAIL_LENGTH = 254; // RFC 5321 §4.5.3.1

/* The endpoint is public and unauthenticated, so without a cap it is a free
   relay into the team's Slack. Per-instance and in-memory: serverless spreads
   requests across instances, so this thins a flood rather than stopping one.
   It is the cheap half of the defence — the webhook's own rate limit is the
   other half. */
const WINDOW_MS = 60_000;
const MAX_PER_WINDOW = 5;
const hits = new Map<string, number[]>();

function rateLimited(key: string): boolean {
  const now = Date.now();
  const recent = (hits.get(key) ?? []).filter((t) => now - t < WINDOW_MS);
  recent.push(now);
  hits.set(key, recent);

  // Unbounded growth would be the real leak; drop keys that have gone quiet.
  if (hits.size > 5_000) {
    for (const [k, times] of hits) {
      if (times.every((t) => now - t >= WINDOW_MS)) hits.delete(k);
    }
  }
  return recent.length > MAX_PER_WINDOW;
}

/* Slack renders `&`, `<` and `>` as markup. An address is attacker-controlled
   text, so it is escaped before it reaches a message body. */
function escapeSlack(text: string): string {
  return text.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");
}

export async function POST(request: Request) {
  const ip =
    request.headers.get("x-forwarded-for")?.split(",")[0]?.trim() ||
    request.headers.get("x-real-ip") ||
    "unknown";

  if (rateLimited(ip)) {
    return Response.json({ error: "Too many requests. Try again in a minute." }, { status: 429 });
  }

  let email: unknown;
  let path: unknown;
  try {
    const body = await request.json();
    email = body?.email;
    path = body?.path;
  } catch {
    return Response.json({ error: "Malformed request." }, { status: 400 });
  }

  if (typeof email !== "string" || email.length > MAX_EMAIL_LENGTH || !EMAIL.test(email.trim())) {
    return Response.json({ error: "That does not look like an email address." }, { status: 400 });
  }

  const address = email.trim();
  // Only ever a path from our own site; truncated so it cannot pad the message.
  const page = typeof path === "string" ? path.slice(0, 120) : "/";

  if (!WEBHOOK_URL) {
    /* In development the address goes to the developer's own terminal, which is
       the closest thing to "the team was notified" that a laptop can offer. In
       production a missing webhook must fail loudly: silently accepting the
       address would tell the reader their address was delivered when it was
       dropped, and printing it would store it in the platform's log retention —
       breaking the promise the modal makes. */
    if (process.env.NODE_ENV === "development") {
      console.info(`[reader-ping] ${address} opened ${page}`);
      return new Response(null, { status: 204 });
    }
    return Response.json({ error: "Notifications are not configured." }, { status: 503 });
  }

  try {
    const slack = await fetch(WEBHOOK_URL, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({
        text: `:open_book: *${escapeSlack(address)}* opened \`${escapeSlack(page)}\``,
      }),
    });
    if (!slack.ok) {
      return Response.json({ error: "Could not reach the team right now." }, { status: 502 });
    }
  } catch {
    return Response.json({ error: "Could not reach the team right now." }, { status: 502 });
  }

  // 204: there is deliberately nothing to return, because nothing was kept.
  return new Response(null, { status: 204 });
}

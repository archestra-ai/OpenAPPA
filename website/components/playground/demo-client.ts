// Client for the appa-demo service: session lifecycle, policy validation, and
// the per-turn SSE stream. `EventSource` is GET-only and cannot set headers,
// so a turn is a POST whose body is read as a stream and split on SSE frames.

export const DEMO_URL = process.env.NEXT_PUBLIC_APPA_DEMO_URL ?? "";

/** Events the service emits for one turn, mirroring `appa_demo::events::WireEvent`. */
export type DemoEvent =
  | { type: "says"; trajectory: string; text: string }
  | { type: "tool_proposed"; trajectory: string; call_id: string; tool: string; arguments: Record<string, unknown> }
  | { type: "blocked"; trajectory: string; call_id: string; text: string }
  | { type: "tool_closed"; trajectory: string; outcome: string; effects: string[] }
  | { type: "tool_result"; trajectory: string; body: string }
  | { type: "label"; trajectory: string; trust: string; audience: string }
  | { type: "remedy"; trajectory: string; text: string }
  | { type: "sanitized"; trajectory: string; sanitizer: string }
  | { type: "fork"; parent: string; child: string }
  | { type: "merge"; trajectory: string }
  | { type: "approval_requested"; id: string; tool: string; detail: Record<string, unknown> }
  | { type: "approval_resolved"; id: string; approved: boolean; expired: boolean }
  | { type: "answer"; text: string }
  | { type: "stopped"; text: string }
  | { type: "error"; message: string };

export type SessionInfo = {
  session: string;
  tools: number;
  trust: string;
  audience: string;
};

export type PolicyCheck =
  | {
      ok: true;
      tools: number;
      unconstrained?: string[];
      ignored?: string[];
      /** Where a turn enters under the submitted policy, read off the loader. */
      boundary?: { trust: string; audience: string };
    }
  | { ok: false; error: string };

/** What a fresh playground starts from: the shipped policy and the world. */
export type Preset = {
  policy: string;
  systems: { id: string; tools: string[] }[];
};

async function failure(response: Response): Promise<string> {
  try {
    const body = (await response.json()) as { error?: string };
    if (body.error) return body.error;
  } catch {
    // fall through to the status line
  }
  return `${response.status} ${response.statusText}`;
}

/** How long the bootstrap waits before calling the service down. */
const BOOTSTRAP_TIMEOUT_MS = 4000;

/**
 * Bootstrap: fetch what a fresh playground starts from. This doubles as the
 * health probe — an answer means the service is up and the card has its
 * starting policy in the same round trip. `null` means down.
 */
export async function fetchPreset(): Promise<Preset | null> {
  if (!DEMO_URL) return null;
  // Bounded: a hung request must resolve to "down", never leave the card
  // sitting on "connecting" forever.
  const abort = new AbortController();
  const timer = setTimeout(() => abort.abort(), BOOTSTRAP_TIMEOUT_MS);
  try {
    const response = await fetch(`${DEMO_URL}/preset`, { cache: "no-store", signal: abort.signal });
    if (!response.ok) return null;
    return (await response.json()) as Preset;
  } catch {
    return null;
  } finally {
    clearTimeout(timer);
  }
}

export async function checkPolicy(policy: string, systems: string[], signal?: AbortSignal): Promise<PolicyCheck> {
  const response = await fetch(`${DEMO_URL}/policy/check`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ policy, systems }),
    signal,
  });
  if (!response.ok) return { ok: false, error: await failure(response) };
  return (await response.json()) as PolicyCheck;
}

export async function createSession(policy: string, systems: string[], model: string): Promise<SessionInfo> {
  const response = await fetch(`${DEMO_URL}/session`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ policy, systems, model }),
  });
  if (!response.ok) throw new Error(await failure(response));
  return (await response.json()) as SessionInfo;
}

/** The visitor's ruling on a parked human-approval request. */
export async function respondApproval(session: string, approval: string, approve: boolean): Promise<void> {
  const response = await fetch(`${DEMO_URL}/session/${session}/approval/${approval}`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ approve }),
  });
  if (!response.ok) throw new Error(await failure(response));
}

export function deleteSession(session: string): void {
  // Best effort on New chat / unload; the service expires idle sessions anyway.
  void fetch(`${DEMO_URL}/session/${session}`, { method: "DELETE", keepalive: true }).catch(() => {});
}

/**
 * Drive one turn, invoking `onEvent` as each event arrives. Resolves when the
 * stream ends; aborting `signal` hangs up, which cancels the turn server-side.
 */
export async function streamTurn(
  session: string,
  text: string,
  apiKey: string,
  onEvent: (event: DemoEvent) => void,
  signal?: AbortSignal,
): Promise<void> {
  const response = await fetch(`${DEMO_URL}/session/${session}/message`, {
    method: "POST",
    headers: { "content-type": "application/json", authorization: `Bearer ${apiKey}` },
    body: JSON.stringify({ text }),
    signal,
  });
  if (!response.ok) throw new Error(await failure(response));
  if (!response.body) throw new Error("the service returned no stream");

  const reader = response.body.getReader();
  const decoder = new TextDecoder();
  let buffer = "";

  for (;;) {
    const { done, value } = await reader.read();
    if (done) break;
    buffer += decoder.decode(value, { stream: true });

    let cut = buffer.indexOf("\n\n");
    while (cut !== -1) {
      const frame = buffer.slice(0, cut);
      buffer = buffer.slice(cut + 2);
      const payload = frame
        .split("\n")
        .filter((line) => line.startsWith("data:"))
        .map((line) => line.slice(5).trim())
        .join("");
      if (payload) {
        try {
          onEvent(JSON.parse(payload) as DemoEvent);
        } catch {
          // A frame we cannot parse is not worth killing the turn over.
        }
      }
      cut = buffer.indexOf("\n\n");
    }
  }
}

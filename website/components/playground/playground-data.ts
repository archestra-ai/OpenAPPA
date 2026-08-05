// Website-owned content for the /landing2 chat playground.
//
// What is *not* here: the starting policy, the systems, their tools, and the
// boundary label. Those are facts about the running service — it ships the
// policy, defines the systems, and loads the policy that decides the boundary
// — so the client fetches them (`/preset`, `/policy/check`) rather than
// keeping a copy that nothing keeps in step. Everything the chat shows comes
// from a live run; when the service is unavailable the card says so.

/** Human copy for a system id the service reports. Unknown ids fall back to
 *  the id itself, so a new system server-side degrades rather than vanishes. */
export const SYSTEM_COPY: Record<string, { label: string; blurb: string }> = {
  crm: {
    label: "CRM",
    blurb: "Customer accounts: contacts, contract values, and private account notes.",
  },
  github: {
    label: "GitHub",
    blurb: "The public issue tracker. Anyone can open an issue, including an attacker.",
  },
  email: {
    label: "Email",
    blurb: "Outbound mail. Anything sent leaves the org.",
  },
  finance: {
    label: "Finance",
    blurb: "Invoices and transfers. Moving money always needs a human sign-off.",
  },
  meetings: {
    label: "Meeting recorder",
    blurb: "Recorded meetings and their transcripts. Retrieval only.",
  },
};

/** A system as the service reports it, dressed with local copy. */
export type PlaygroundSystem = {
  id: string;
  label: string;
  blurb: string;
  tools: string[];
};

export function describeSystem(system: { id: string; tools: string[] }): PlaygroundSystem {
  const copy = SYSTEM_COPY[system.id];
  return {
    id: system.id,
    label: copy?.label ?? system.id,
    blurb: copy?.blurb ?? "",
    tools: system.tools,
  };
}

// Model ids are OpenRouter's, and the service passes whatever it is given
// straight through, so the menu is the website's to choose.
export const PLAYGROUND_MODELS = [
  { id: "openai/gpt-4o", label: "gpt-4o" },
  { id: "openai/gpt-5.6-luna", label: "gpt-5.6-luna" },
  { id: "google/gemini-3.5-flash-lite", label: "gemini-3.5-flash-lite" },
  { id: "qwen/qwen-3.6-35b", label: "qwen-3.6-35b" },
];

export type LabelState = { trust: string; audience: string };

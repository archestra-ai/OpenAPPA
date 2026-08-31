// Website-owned content for the /playground chat.
//
// What is *not* here: the starting policy, the systems, their tools, and the
// boundary label. Those are facts about the running service — it ships the
// policy, defines the systems, and loads the policy that decides the boundary
// — so the client fetches them (`/preset`, `/policy/check`) rather than
// keeping a copy that nothing keeps in step. Everything the chat shows comes
// from a live run; when the service is unavailable the card says so.

// The model id is OpenRouter's, and the service passes it straight through.
// Pinned to the one model the demo is tuned for — the plain (non-reasoning)
// slug; the runtime never sends a `reasoning` field, so no reasoning is
// requested. Not selectable in the UI.
export const PLAYGROUND_MODEL = { id: "openai/gpt-5.6-luna", label: "gpt-5.6-luna" };

export type LabelState = { trust: string; audience: string };

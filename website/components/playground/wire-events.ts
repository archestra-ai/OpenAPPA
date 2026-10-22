// Generated from `WireEvent` in website-chat-playground/src/events.rs. Do not edit.
// Regenerate with `APPA_BLESS=1 cargo test -p website-chat-playground --lib wire_events`.

/** One event of a turn's stream. */
export type WireEvent =
  | { type: "says"; text: string; trajectory: string }
  | { type: "tool_proposed"; arguments: unknown; call_id: string; tool: string; trajectory: string }
  /** A held call. `text` is what the model read; `reasons` and `options` say the same for the person watching, without the instructions written for the model. */
  | { type: "blocked"; call_id: string; options?: BlockOption[]; reasons?: string[]; text: string; trajectory: string }
  | { type: "tool_closed"; call_id: string; effects: string[]; outcome: string; trajectory: string }
  /** The body the model read as the answer to `call_id`. */
  | { type: "tool_result"; body: string; call_id: string; trajectory: string }
  /** The remedy the control call `call_id` chose was carried out; `value` is what crossed in place of the held one, when it delivered one. */
  | { type: "offer_taken"; call_id: string; detail: string; trajectory: string; value: string | null }
  | { type: "label"; audience: string; trajectory: string; trust: string }
  | { type: "remedy"; text: string; trajectory: string }
  /** A sanitizer's derivation crossed in place of a raw value. `call_id` names the call whose result it rewrote; `None` when it rewrote something else, such as a call's arguments before its dispatch. */
  | { type: "sanitized"; call_id: string | null; sanitizer: string; trajectory: string }
  | { type: "fork"; child: string; parent: string }
  | { type: "merge"; trajectory: string }
  | { type: "approval_requested"; detail: unknown; id: string; tool: string }
  | { type: "approval_resolved"; approved: boolean; expired: boolean; id: string }
  | { type: "answer"; text: string }
  | { type: "stopped"; text: string }
  | { type: "error"; message: string };

/** One remedy a block offers, keyed by the `offer_id` the model passes to take it. */
export type BlockOption = { offer_id: string; text: string };

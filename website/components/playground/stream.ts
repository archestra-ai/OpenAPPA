// The playground's reading of one turn stream: how each event the
// appa-demo service sends changes the chat. A pure function of the stream so
// far and the next event, so a recorded stream can be replayed in a test.

import type { BlockOption, DemoEvent } from "./demo-client";
import type { LabelState } from "./playground-data";

export type ThreadItem = { lab?: LabelState } & (
  | { id: number; t: "user"; text: string }
  | { id: number; t: "text"; text: string; traj?: string }
  | { id: number; t: "note"; text: string; traj?: string }
  | {
      id: number;
      t: "tool";
      callId: string;
      name: string;
      args: Record<string, unknown>;
      state: "running" | "done" | "blocked";
      output?: string;
      /** The result body the model read, verbatim. */
      result?: string;
      blocked?: string;
      /** Set when the engine refused this protocol call outright. */
      refused?: boolean;
      /** Set when the raw result was withheld and a sanitizer's derivation admitted. */
      sanitizedBy?: string;
      /** Set when the call ran under an authority's ruling. */
      approvedBy?: string;
      /**
       * A re-run: the held call, run again after its remedy. The original
       * call keeps the block; the re-run carries the outcome.
       */
      echo?: boolean;
      /** Which trajectory this happened on; child ids render as a branch. */
      traj?: string;
    }
  | {
      id: number;
      t: "approval";
      /** The service-side approval id the answer posts back to. */
      approvalId: string;
      tool: string;
      /** The authority the policy consulted, named in the request. */
      authority?: string;
      detail: string;
      state: "pending" | "approved" | "denied" | "expired";
    }
  /**
   * APPA's turn on a held call. `text` is what the model read; `reasons` and
   * `options` are the same ruling written for the person watching.
   */
  | {
      id: number;
      t: "verdict";
      text: string;
      reasons?: string[];
      options?: BlockOption[];
      traj?: string;
      tool?: string;
    }
  /** The root label moved: a marker row that recolors the rail below it. */
  | { id: number; t: "shift"; from: LabelState; to: LabelState; cause?: string }
  /** Where the session entered: the label's opening row in the stream. */
  | { id: number; t: "entry"; label: LabelState }
  /** The closing recap of a demo case: what the path just showed and why. */
  | { id: number; t: "authors"; text: string }
  | { id: number; t: "rule" }
);

/** What the stream has built so far. */
export type StreamState = {
  thread: ThreadItem[];
  /** The root trajectory's current label; every new item is stamped with it. */
  label: LabelState | null;
  /** Trajectories forked off the root; their items render as a branch. */
  children: ReadonlySet<string>;
};

/**
 * The harness's own tools (`appa_runtime::tool`). Feedback on these is
 * protocol dialogue — an acknowledgment, a cost statement, a stale-offer
 * notice — never a policy ruling on a flow; rulings land on the blocked
 * call itself. So these calls close calmly instead of styling as errors.
 */
/**
 * The runtime's control tool under its canonical id. The service reports the
 * call the runtime saw, and the runtime translates the model's alias to this.
 */
const CONTROL_TOOL = "appa/execute_remedy_plan";

export const PROTOCOL_TOOLS = new Set(["execute_remedy_plan", CONTROL_TOOL, "fork", "submit_result"]);

/** The runtime's control call, under the alias the model calls or its canonical id. */
export function isRemedyCall(name: string): boolean {
  return name === "execute_remedy_plan" || name === CONTROL_TOOL;
}

/** The offer a remedy call takes. */
export function offerOf(args: Record<string, unknown>): string | undefined {
  return typeof args.offer_id === "string" ? args.offer_id : undefined;
}

/**
 * A ruling arrives as the model sees it: a sentence naming what failed and
 * what would fix it, then the machine-readable gaps and remedy plans. The ruling
 * leads with the sentence and keeps the payload as scrollable detail.
 *
 * Both kinds are the engine doing its job, never a malfunction, so neither
 * wears error styling. They differ in what they ask of the agent: a
 * `narrowing` is a price — the call's own contract would drop the session
 * label, and dispatch waits for an acceptance or a fork — while a `refusal`
 * is a gate — the label does not meet the tool's requirements, and the
 * ruling names the remedies. For a narrowing the ruling names the concrete
 * consequence per dimension: a trust drop taints the session, an audience
 * shrink makes it confidential.
 */
/** One executable plan the block offers, with everything its option line needs. */
export type ParsedPlan = {
  id: string;
  /** The authorities the plan consults, each with the operator's hint. */
  rulings: { authority: string; hint?: string }[];
  /** The output sanitizer the plan binds, with its hint. */
  sanitize?: { sanitizer: string; hint?: string };
  /** Whether executing the plan accepts the block's narrowing. */
  accepts: boolean;
};

export type Ruling = { plans: ParsedPlan[]; summary: string; detail?: string } & (
  | { kind: "refusal" }
  | {
      kind: "narrowing";
      dimension: "trust" | "audience" | "both";
      /** The readers left, when the shrunk audience is a concrete set. */
      readers?: string[];
      /** The target trust rank — an index into the policy's trust chain. */
      toTrustRank?: number;
    }
);

/** The executable entries of `remedy_plans`; id-less redispatch advice stays in the payload. */
function parsePlans(plans: unknown): ParsedPlan[] {
  if (!Array.isArray(plans)) return [];
  return plans.flatMap((plan) => {
    const entry = plan as {
      plan_id?: unknown;
      rulings?: { authority?: unknown; hint?: unknown }[];
      sanitizes?: { sanitizer?: unknown; hint?: unknown };
      accepts_narrowing?: unknown;
    };
    if (typeof entry.plan_id !== "string") return [];
    const rulings = (Array.isArray(entry.rulings) ? entry.rulings : []).flatMap((ruling) =>
      typeof ruling.authority === "string"
        ? [
            {
              authority: ruling.authority,
              hint: typeof ruling.hint === "string" ? ruling.hint : undefined,
            },
          ]
        : [],
    );
    const sanitize =
      typeof entry.sanitizes?.sanitizer === "string"
        ? {
            sanitizer: entry.sanitizes.sanitizer,
            hint: typeof entry.sanitizes.hint === "string" ? entry.sanitizes.hint : undefined,
          }
        : undefined;
    return [
      {
        id: entry.plan_id,
        rulings,
        sanitize,
        accepts: Boolean(entry.accepts_narrowing),
      },
    ];
  });
}

export function splitRuling(text: string): Ruling {
  const start = text.indexOf("\n{");
  if (start === -1) return { kind: "refusal", summary: text, plans: [] };
  const summary = text.slice(0, start).trim();
  const detail = text.slice(start + 1);
  try {
    const parsed = JSON.parse(detail) as {
      narrowing?: {
        from?: Record<string, unknown>;
        to?: Record<string, unknown>;
      };
      requirement_gaps?: unknown[];
      remedy_plans?: unknown;
    };
    const pretty = JSON.stringify(parsed, null, 2);
    const plans = parsePlans(parsed.remedy_plans);
    const narrowing = parsed.narrowing;
    if (!narrowing || parsed.requirement_gaps?.length) return { kind: "refusal", summary, detail: pretty, plans };
    const moved = (dim: string) => JSON.stringify(narrowing.from?.[dim]) !== JSON.stringify(narrowing.to?.[dim]);
    const dimension = moved("trust") && moved("audience") ? "both" : moved("audience") ? "audience" : "trust";
    // A concrete reader set arrives as `{"Restricted": [...]}` and a trust rank
    // as a plain chain index. Any other shape (e.g. `"Public"`) makes these
    // sentences fall back to "fewer readers" and "less trusted" instead of
    // naming the real thing.
    const audience = narrowing.to?.audience as { Restricted?: unknown } | undefined;
    const restricted = audience?.Restricted;
    const readers = Array.isArray(restricted) ? restricted.map(String) : undefined;
    const trustValue = narrowing.to?.trust;
    const toTrustRank = typeof trustValue === "number" ? trustValue : undefined;
    return {
      kind: "narrowing",
      dimension,
      readers,
      toTrustRank,
      summary,
      detail: pretty,
      plans,
    };
  } catch {
    return { kind: "refusal", summary, detail, plans: [] };
  }
}

/**
 * The chat after one more event. `nextId` mints the id of every item the
 * event adds.
 */
export function applyEvent(state: StreamState, event: DemoEvent, nextId: () => number): StreamState {
  let { thread, label, children } = state;
  const update = (change: (prev: ThreadItem[]) => ThreadItem[]) => {
    thread = change(thread);
  };
  // Every pushed item is stamped with the root label at that moment — the
  // rail beside the thread is this history, not a live value.
  const push = (item: ThreadItem) => {
    thread = [...thread, { ...item, lab: item.lab ?? label ?? undefined }];
  };

  /**
   * Route a remedy's outcome to a re-run: the held call keeps its block, and
   * a second entry for the same call — appended after the APPA and
   * agent turns — carries what the remedy produced. Created on the first
   * outcome event of the batch, patched by the rest.
   */
  const resolveHeld = (
    prev: ThreadItem[],
    patch: Partial<Extract<ThreadItem, { t: "tool" }>>,
    sanitizer?: string,
    known?: number,
  ): ThreadItem[] => {
    /* Which held call this outcome belongs to. A delivered value names its
       remedy, and so its held call (`known`); a sanitizer or an approval
       arrives from the audit with no call id, and the agent can have several
       calls held at once — taking "the last held call" then routed every
       outcome to the same call, so one re-run was overwritten and
       the other never appeared.

       A sanitizer names itself, and only one held ruling offered it, so that
       is the reliable match. Failing that, outcomes arrive in the order the
       remedies were taken, so the oldest still-unresolved call is next. */
    const resolved = (item: Extract<ThreadItem, { t: "tool" }>) =>
      prev.some((entry) => entry.t === "tool" && entry.echo && entry.callId === `${item.callId}+resolved`);
    const isHeld = (item: ThreadItem): item is Extract<ThreadItem, { t: "tool" }> =>
      item.t === "tool" && item.state === "blocked" && !PROTOCOL_TOOLS.has(item.name);
    const offers = (item: Extract<ThreadItem, { t: "tool" }>) =>
      Boolean(
        sanitizer &&
        item.blocked &&
        splitRuling(item.blocked).plans.some((plan) => plan.sanitize?.sanitizer === sanitizer),
      );

    // The sanitizer match ignores whether the call already has a resolution
    // re-run: the outcome batch opens that re-run, and this event patches it.
    let heldAt = known ?? (sanitizer ? prev.findIndex((item) => isHeld(item) && offers(item)) : -1);
    if (heldAt === -1) heldAt = prev.findIndex((item) => isHeld(item) && !resolved(item));
    if (heldAt === -1) heldAt = prev.findLastIndex(isHeld);
    if (heldAt === -1) return prev;
    const held = prev[heldAt] as Extract<ThreadItem, { t: "tool" }>;
    // The echo belongs to this call, not merely to a call of the same name.
    const echoAt = prev.findIndex(
      (item) => item.t === "tool" && Boolean(item.echo) && item.callId === `${held.callId}+resolved`,
    );
    if (echoAt !== -1)
      return prev.map((item, at) => (at === echoAt && item.t === "tool" ? { ...item, ...patch } : item));
    return [
      ...prev,
      {
        id: nextId(),
        t: "tool",
        echo: true,
        callId: `${held.callId}+resolved`,
        name: held.name,
        args: held.args,
        state: "done",
        output: "ran",
        traj: held.traj,
        lab: label ?? undefined,
        ...patch,
      },
    ];
  };

  switch (event.type) {
    case "says":
      push({
        id: nextId(),
        t: "text",
        text: event.text,
        traj: event.trajectory,
      });
      break;
    case "tool_proposed": {
      // Light the contract the engine is about to check. The harness's own
      // tools have no contract in the editor.
      const item: ThreadItem = {
        id: nextId(),
        t: "tool",
        callId: event.call_id,
        name: event.tool,
        args: event.arguments,
        state: "running",
        traj: event.trajectory,
        lab: label ?? undefined,
      };
      update((prev) => {
        /* A remedy answers one ruling, so it belongs under that ruling. Plan
           ids are unique across a batch, so the offer identifies its block
           exactly — appending instead filed every remedy under whichever
           ruling happened to be last. */
        const planId = isRemedyCall(event.tool) ? offerOf(event.arguments) : undefined;
        if (planId !== undefined) {
          const verdictAt = prev.findIndex(
            (entry) =>
              entry.t === "verdict" &&
              (entry.options?.some((option) => option.offer_id === planId) ||
                splitRuling(entry.text).plans.some((plan) => plan.id === planId)),
          );
          if (verdictAt !== -1) {
            // Past any remedy already filed under the same ruling.
            let after = verdictAt + 1;
            while (after < prev.length) {
              const entry = prev[after];
              if (entry.t !== "tool" || !PROTOCOL_TOOLS.has(entry.name)) break;
              after++;
            }
            const next = [...prev];
            next.splice(after, 0, item);
            return next;
          }
        }
        // The agent's remedy choice precedes the approval it triggers, but
        // the elicitation can hit the wire first: slot a protocol call in
        // front of any still-pending approval so the screen keeps the
        // causal order — choose, then ask the human.
        let at = prev.length;
        if (PROTOCOL_TOOLS.has(event.tool))
          while (at > 0) {
            const before = prev[at - 1];
            if (before.t === "approval" && before.state === "pending") at--;
            else break;
          }
        return [...prev.slice(0, at), item, ...prev.slice(at)];
      });
      break;
    }
    case "blocked":
      // The runtime has one channel for text fed back to the model on a
      // call, and it carries harness acknowledgments as well as rulings —
      // "result submitted to the parent" arrives the same way a refusal
      // does. Feedback on the harness's own protocol tools (submit_result,
      // fork) is an outcome, not a ruling, so it closes the call calmly
      // instead of wearing the error styling.
      update((prev) => {
        // A held output is blocked after its dispatch closed, and the close
        // can arrive first: the call may already read "done".
        const index = prev.findIndex(
          (item) => item.t === "tool" && item.callId === event.call_id && item.state !== "blocked",
        );
        if (index === -1) return prev;
        const item = prev[index] as Extract<ThreadItem, { t: "tool" }>;
        // A block on a protocol call is the engine refusing the agent's own
        // move — the call has to say so, or a rejected remedy reads exactly
        // like one that worked.
        if (PROTOCOL_TOOLS.has(item.name))
          return prev.map((entry, at) =>
            at === index
              ? {
                  ...item,
                  state: "done" as const,
                  output: event.text,
                  refused: true,
                }
              : entry,
          );
        // The call records the hold; the ruling itself is APPA's own turn.
        const next = prev.map((entry, at) =>
          at === index ? { ...item, state: "blocked" as const, blocked: event.text } : entry,
        );
        /* File the ruling directly under the call it answers, not at the
           end of the stream. The agent proposes calls in batches, so the
           engine's rulings arrive together after them — appending left a
           run of calls followed by a run of rulings, with nothing saying
           which answered which. The ruling names its own tool for the same
           reason: it is matched by `call_id`, never by proximity. */
        next.splice(index + 1, 0, {
          id: nextId(),
          t: "verdict",
          text: event.text,
          reasons: event.reasons,
          options: event.options,
          tool: item.name,
          traj: item.traj,
          lab: label ?? undefined,
        });
        return next;
      });
      break;
    case "tool_result":
      // The body the model read, on the call that read it. The result and
      // the dispatch's close arrive in either order; either one ends the call.
      update((prev) =>
        prev.map((item) =>
          item.t === "tool" && item.callId === event.call_id
            ? { ...item, result: event.body, state: item.state === "running" ? "done" : item.state }
            : item,
        ),
      );
      break;
    case "tool_closed":
      update((prev) =>
        prev.map((item) =>
          item.t === "tool" && item.callId === event.call_id
            ? {
                ...item,
                state: item.state === "blocked" ? item.state : "done",
                output:
                  event.outcome === "ran"
                    ? event.effects.length
                      ? `ran, committing [${event.effects.join(", ")}]`
                      : "ran"
                    : event.outcome,
              }
            : item,
        ),
      );
      break;
    case "offer_taken":
      // The remedy call is answered. A remedy that delivered a value
      // resolves the held call that offered it: the value belongs on the
      // call the visitor watched get held, not on the protocol step.
      update((prev) => {
        const remedy = prev.find((item) => item.t === "tool" && item.callId === event.call_id);
        const next = prev.map((item) =>
          item === remedy && item.t === "tool" ? { ...item, state: "done" as const, output: event.detail } : item,
        );
        const offer = remedy?.t === "tool" ? offerOf(remedy.args) : undefined;
        if (event.value === null || offer === undefined) return next;
        // The ruling that offered it is filed directly under its held call.
        const verdictAt = next.findIndex(
          (item) => item.t === "verdict" && Boolean(item.options?.some((option) => option.offer_id === offer)),
        );
        const heldAt =
          verdictAt !== -1
            ? next.findLastIndex(
                (item, at) => at < verdictAt && item.t === "tool" && item.state === "blocked",
              )
            : next.findIndex(
                (item) =>
                  item.t === "tool" && item.state === "blocked" && Boolean(item.blocked?.includes(`"${offer}"`)),
              );
        return heldAt === -1 ? next : resolveHeld(next, { result: event.value }, undefined, heldAt);
      });
      break;
    case "label": {
      const next = { trust: event.trust, audience: event.audience };
      // A child's fold is its branch's own story: a line inside the branch
      // block, never a move on the root rail.
      if (children.has(event.trajectory)) {
        push({
          id: nextId(),
          t: "note",
          text: `branch label → trust: ${event.trust} · audience: ${event.audience}`,
          traj: event.trajectory,
        });
        break;
      }
      const from = label;
      if (from && (from.trust !== next.trust || from.audience !== next.audience)) {
        const shift: ThreadItem = {
          id: nextId(),
          t: "shift",
          from,
          to: next,
          lab: next,
        };
        /* The service reports the new label after the call's result, but on
           an accepted narrowing the label moved *before* dispatch — the
           acceptance is what paid for the call. Rendering the
           wire order puts the move after the call it authorised, which
           reads as the wrong causal story. So the move is filed ahead of
           the re-runs it paid for.

           Scoped to those re-runs on purpose: when a narrowing is not
           accepted but simply admitted with the result, the label really
           does move after the call, and that order is left alone. */
        update((prev) => {
          let at = prev.length;
          while (at > 0) {
            const above = prev[at - 1];
            if (above.t !== "tool" || !above.echo) break;
            at--;
          }
          const paid = prev[at];
          const caused = paid && paid.t === "tool" ? paid.name : undefined;
          return [...prev.slice(0, at), { ...shift, cause: caused }, ...prev.slice(at)];
        });
      }
      label = next;
      break;
    }
    case "approval_requested": {
      // The policy pane stays where the reader put it: jumping it mid-run
      // moved the ground under anyone reading, and the stream already names
      // every rule as a reference they can follow when they choose to.
      const authority = (event.detail as { authority?: string }).authority;
      push({
        id: nextId(),
        t: "approval",
        approvalId: event.id,
        tool: event.tool,
        authority,
        detail: JSON.stringify(event.detail, null, 2),
        state: "pending",
      });
      break;
    }
    case "approval_resolved":
      update((prev) => {
        // The paused call resumes under this ruling: say so on the call.
        const resolved = prev.find((item) => item.t === "approval" && item.approvalId === event.id);
        const authority = resolved?.t === "approval" ? resolved.authority : undefined;
        const running = event.approved
          ? prev.findLastIndex(
              (item) =>
                item.t === "tool" &&
                item.state === "running" &&
                resolved?.t === "approval" &&
                item.name === resolved.tool,
            )
          : -1;
        return prev.map((item, at) => {
          if (item.t === "approval" && item.approvalId === event.id)
            return {
              ...item,
              state: event.expired ? "expired" : event.approved ? "approved" : "denied",
            };
          if (at === running && item.t === "tool" && authority) return { ...item, approvedBy: authority };
          return item;
        });
      });
      break;
    case "remedy": {
      // "narrowing accepted" would repeat what the label separator (root)
      // or the branch-label note (child) already says: drop it. A ruling
      // lands on the call it approved instead of floating as a line.
      const approved = event.text.match(/^approved by (.+)$/);
      if (approved) {
        update((prev) => {
          const running = prev.findLastIndex(
            (item) => item.t === "tool" && item.state === "running" && !PROTOCOL_TOOLS.has(item.name),
          );
          if (running !== -1)
            return prev.map((item, at) =>
              at === running && item.t === "tool" ? { ...item, approvedBy: approved[1] } : item,
            );
          return resolveHeld(prev, { approvedBy: approved[1] });
        });
        break;
      }
      if (event.text.startsWith("narrowing accepted:")) break;
      push({
        id: nextId(),
        t: "note",
        text: event.text,
        traj: event.trajectory,
      });
      break;
    }
    case "sanitized":
      // Mark the call whose result was replaced: a running world call
      // completes in place; a held call resolves onto its re-run.
      update((prev) => {
        const running = prev.findLastIndex(
          (item) => item.t === "tool" && item.state === "running" && !PROTOCOL_TOOLS.has(item.name),
        );
        if (running !== -1)
          return prev.map((item, at) =>
            at === running && item.t === "tool" ? { ...item, sanitizedBy: event.sanitizer } : item,
          );
        return resolveHeld(prev, { sanitizedBy: event.sanitizer }, event.sanitizer);
      });
      break;
    case "merge":
      // A return is its own checked crossing: the branch confined what the
      // child *kept*, not what it hands back. If the returned value is
      // restricted, the label event right after shows the parent paying for
      // it — so say what crossed, not that it was free.
      push({
        id: nextId(),
        t: "note",
        text: "child returned a value — checked at the merge",
      });
      break;
    case "fork":
      // No note: the branch block's own header marks the fork.
      children = new Set(children).add(event.child);
      break;
    case "answer":
      // The final answer is already in the log as the last assistant message;
      // the outcome repeats it, so only append when the loop ended some other way.
      update((prev) => {
        const lastText = [...prev].reverse().find((item) => item.t === "text");
        if (lastText && lastText.t === "text" && lastText.text.trim() === event.text.trim()) return prev;
        return [...prev, { id: nextId(), t: "text", text: event.text }];
      });
      break;
    case "stopped":
      push({ id: nextId(), t: "note", text: event.text });
      break;
    case "error":
      push({ id: nextId(), t: "note", text: event.message });
      break;
  }
  return { thread, label, children };
}

"use client";

// The /landing2 chat playground: the landing demo card's chrome around a real
// chat UI (AI Elements), driven by the appa-demo service — the visitor's own
// OpenRouter key, the policy in the editor actually enforced, any prompt.
//
// Everything shown is a live run. There is no canned transcript: when the
// service is unreachable the card says so and the composer is disabled, rather
// than replaying something that only looks live.

import { useCallback, useEffect, useMemo, useRef, useState } from "react";

import {
  Conversation,
  ConversationContent,
  ConversationEmptyState,
  ConversationScrollButton,
} from "@/components/ai-elements/conversation";
import { Message, MessageContent, MessageResponse } from "@/components/ai-elements/message";
import {
  PromptInput,
  type PromptInputMessage,
  PromptInputSubmit,
  PromptInputTextarea,
} from "@/components/ai-elements/prompt-input";
import { Tool, ToolContent, ToolHeader, ToolInput, ToolOutput } from "@/components/ai-elements/tool";

import {
  DEMO_URL,
  type DemoEvent,
  checkPolicy,
  createSession,
  deleteSession,
  fetchPreset,
  respondApproval,
  streamTurn,
} from "./demo-client";
import {
  type LabelState,
  PLAYGROUND_MODELS,
  type PlaygroundSystem,
  describeSystem,
} from "./playground-data";
import { PolicyEditor, findBlock } from "./PolicyEditor";
import { registerPixelMarks } from "@/app/landing/pixel-marks";

declare module "react" {
  namespace JSX {
    interface IntrinsicElements {
      "appa-mark": React.DetailedHTMLProps<React.HTMLAttributes<HTMLElement>, HTMLElement> & {
        size?: number | string;
      };
    }
  }
}

type ThreadItem =
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
      delta?: string;
      /** Set when the raw result was withheld and a sanitizer's derivation admitted. */
      sanitizedBy?: string;
      /** Which trajectory this happened on; child ids render as a branch. */
      traj?: string;
    }
  | {
      id: number;
      t: "approval";
      /** The service-side approval id the answer posts back to. */
      approvalId: string;
      tool: string;
      detail: string;
      state: "pending" | "approved" | "denied" | "expired";
    }
  | { id: number; t: "rule" };

type Mode = "probing" | "live" | "down";

const KEY_STORAGE = "appa-playground-openrouter-key";

/**
 * The harness's own tools (`appa_runtime::tool`). Feedback on these is
 * protocol dialogue — an acknowledgment, a cost statement, a stale-offer
 * notice — never a policy ruling on a flow; rulings land on the blocked
 * tool's own card. So their cards close calmly instead of styling as errors.
 */
const PROTOCOL_TOOLS = new Set(["execute_remedy_plan", "fork", "submit_result"]);

/**
 * A ruling arrives as the model sees it: a sentence naming what failed and
 * what would fix it, then the machine-readable gaps and remedy plans. The card
 * leads with the sentence and keeps the payload as scrollable detail.
 *
 * Both kinds are the engine doing its job, never a malfunction, so neither
 * wears error styling. They differ in what they ask of the agent: a
 * `narrowing` is a price — the call's own contract would drop the session
 * label, and dispatch waits for an acceptance or a fork — while a `refusal`
 * is a gate — the label does not meet the tool's requirements, and the
 * ruling names the remedies. For a narrowing the card names the concrete
 * consequence per dimension: a trust drop taints the session, an audience
 * shrink makes it confidential.
 */
/** One sanitizer the block offers, with the operator's account of what it drops. */
type SanitizeOffer = { sanitizer: string; hint?: string; free: boolean };

type Ruling = { offers: SanitizeOffer[]; summary: string; detail?: string } & (
  | { kind: "refusal" }
  | {
      kind: "narrowing";
      dimension: "trust" | "audience" | "both";
      /** The readers left, when the shrunk audience is a concrete set. */
      readers?: string[];
    }
);

/**
 * The sanitize plans the block offers. `free` marks the one whose
 * relabel clears the narrowing outright — it accepts nothing, so taking it costs
 * the session no label at all.
 */
function sanitizeOffers(plans: unknown): SanitizeOffer[] {
  if (!Array.isArray(plans)) return [];
  return plans.flatMap((plan) => {
    const entry = plan as { sanitizes?: { sanitizer?: unknown; hint?: unknown }; accepts_narrowing?: unknown };
    const name = entry.sanitizes?.sanitizer;
    if (typeof name !== "string") return [];
    const hint = entry.sanitizes?.hint;
    return [{ sanitizer: name, hint: typeof hint === "string" ? hint : undefined, free: !entry.accepts_narrowing }];
  });
}

function splitRuling(text: string): Ruling {
  const start = text.indexOf("\n{");
  if (start === -1) return { kind: "refusal", summary: text, offers: [] };
  const summary = text.slice(0, start).trim();
  const detail = text.slice(start + 1);
  try {
    const parsed = JSON.parse(detail) as {
      narrowing?: { from?: Record<string, unknown>; to?: Record<string, unknown> };
      requirement_gaps?: unknown[];
      remedy_plans?: unknown;
    };
    const pretty = JSON.stringify(parsed, null, 2);
    const offers = sanitizeOffers(parsed.remedy_plans);
    const narrowing = parsed.narrowing;
    if (!narrowing || parsed.requirement_gaps?.length) return { kind: "refusal", summary, detail: pretty, offers };
    const moved = (dim: string) => JSON.stringify(narrowing.from?.[dim]) !== JSON.stringify(narrowing.to?.[dim]);
    const dimension = moved("trust") && moved("audience") ? "both" : moved("audience") ? "audience" : "trust";
    // `{"Known": {"Restricted": ["crm"]}}` — the engine's wire shape for a
    // concrete reader set.
    const known = (narrowing.to?.audience as { Known?: { Restricted?: unknown } } | undefined)?.Known;
    const readers = Array.isArray(known?.Restricted) ? known.Restricted.map(String) : undefined;
    return { kind: "narrowing", dimension, readers, summary, detail: pretty, offers };
  } catch {
    return { kind: "refusal", summary, detail, offers: [] };
  }
}

function rulingCaption(ruling: Ruling): string {
  if (ruling.kind === "refusal") return "The policy does not allow this flow.";
  const audience = ruling.readers?.length
    ? `may only reach ${ruling.readers.join(", ")}`
    : "may reach fewer readers";
  switch (ruling.dimension) {
    case "trust":
      return "This source is untrusted. Reading it lowers the session's trust — everything it produces from here on counts as untrusted, permanently.";
    case "audience":
      return `This data is confidential. After reading it, whatever the session produces ${audience} — permanently.`;
    case "both":
      return `This source is untrusted and confidential. Reading it lowers the session's trust, and its output ${audience} — permanently.`;
  }
}

function rulingBadge(ruling: Ruling): string {
  if (ruling.kind === "refusal") return "blocked by policy";
  switch (ruling.dimension) {
    case "trust":
      return "lowers the trust";
    case "audience":
      return "narrows the audience";
    case "both":
      return "lowers the trust · narrows the audience";
  }
}

/**
 * Split the thread into runs by trajectory: root activity renders inline,
 * and each consecutive run of child-trajectory items becomes one collapsed
 * branch block — the fork happens off to the side, visually as in the model.
 */
function segmentThread(
  thread: ThreadItem[],
  childIds: ReadonlySet<string>,
): { child: string | null; items: ThreadItem[] }[] {
  const segments: { child: string | null; items: ThreadItem[] }[] = [];
  for (const item of thread) {
    const traj = "traj" in item ? item.traj : undefined;
    const child = traj && childIds.has(traj) ? traj : null;
    const last = segments[segments.length - 1];
    if (last && last.child === child) last.items.push(item);
    else segments.push({ child, items: [item] });
  }
  return segments;
}

// Toolbar chrome, matching the site's style strings.
const chrome = {
  bar: {
    display: "flex",
    alignItems: "center",
    gap: "0.5rem",
    padding: "0.7rem 1rem",
    borderBottom: "1px solid var(--border-weak)",
    flexWrap: "wrap",
  } as React.CSSProperties,
  title: { fontSize: 12.5, color: "var(--icon)" } as React.CSSProperties,
  barBtn: {
    font: "inherit",
    fontSize: 12,
    padding: "0.25rem 0.7rem",
    borderRadius: 5,
    border: "1px solid var(--border-weak)",
    background: "transparent",
    color: "var(--text-weak)",
    cursor: "pointer",
  } as React.CSSProperties,
};

const pill = (bg: string, fg: string): React.CSSProperties => ({
  fontSize: 10.5,
  letterSpacing: "0.07em",
  textTransform: "uppercase",
  padding: "0.12rem 0.55rem",
  borderRadius: 999,
  whiteSpace: "nowrap",
  background: bg,
  color: fg,
});

function LabelPills({ label, boundary }: { label: LabelState; boundary: LabelState | null }) {
  // Green while trust still sits where the turn entered; amber once it has
  // dropped. The boundary comes from the loader's reading of the policy in the
  // editor, so this stays true when a visitor renames the ranks.
  const clean = label.trust === boundary?.trust;
  return (
    <span style={{ display: "flex", gap: "0.4rem", alignItems: "center" }}>
      <span style={pill(clean ? "#1f6b46" : "#8c5f1f", clean ? "#eafff2" : "#fff4e0")}>trust: {label.trust}</span>
      <span style={pill("#3f3168", "#efe9ff")}>audience: {label.audience}</span>
    </span>
  );
}

/**
 * The live playground: fills its container — the /chat route — with the chat
 * as the main surface and the policy pane as a full-height right sidebar.
 */
export function ChatPlayground() {
  const [mode, setMode] = useState<Mode>(DEMO_URL ? "probing" : "down");
  const [thread, setThread] = useState<ThreadItem[]>([]);
  // Trajectories forked off the root this session; their items render as a
  // collapsed branch instead of inline root activity.
  const [childIds, setChildIds] = useState<ReadonlySet<string>>(new Set());
  // Null until the loader reports where a turn enters under the current
  // policy; there is nothing honest to show before that.
  const [boundary, setBoundary] = useState<LabelState | null>(null);
  const [label, setLabel] = useState<LabelState | null>(null);
  const [busy, setBusy] = useState(false);
  const [input, setInput] = useState("");
  const [turns, setTurns] = useState(0);

  // Two independent inputs: which systems exist (so which tools the agent has)
  // and what the policy allows those tools to do. Both arrive with the preset.
  const [pane, setPane] = useState<"tools" | "policy">("policy");
  const [editorMax, setEditorMax] = useState(false);
  // Mobile only: the pane lives in a right-side drawer, closed by default.
  const [panelOpen, setPanelOpen] = useState(false);
  // The contract or authority the engine is acting on right now, lit in the
  // editor. A focus holds until the next acting block replaces it; when the
  // turn settles (`settled`), it lingers briefly and fades.
  const [focus, setFocus] = useState<{ name: string; at: number; settled?: boolean } | null>(null);
  const [catalog, setCatalog] = useState<PlaygroundSystem[]>([]);
  const [presetPolicy, setPresetPolicy] = useState("");
  const [systems, setSystems] = useState<string[]>([]);
  const [policyText, setPolicyText] = useState("");
  const [policyStatus, setPolicyStatus] = useState<{ ok: boolean; text: string } | null>(null);

  const [apiKey, setApiKey] = useState("");
  const [model, setModel] = useState(PLAYGROUND_MODELS[0].id);

  const sessionRef = useRef<string | null>(null);
  const abortRef = useRef<AbortController | null>(null);
  const runRef = useRef(0);
  const idRef = useRef(0);
  // The header pills — flight target for a label change.
  const pillsRef = useRef<HTMLSpanElement>(null);

  useEffect(() => {
    registerPixelMarks();
    setApiKey(window.localStorage.getItem(KEY_STORAGE) ?? "");
    let cancelled = false;
    // One round trip: an answer means the service is up and brings what the
    // playground starts from — the shipped policy and the world's systems.
    void fetchPreset().then((preset) => {
      if (cancelled) return;
      if (!preset) {
        setMode("down");
        return;
      }
      setCatalog(preset.systems.map(describeSystem));
      setSystems(preset.systems.map((system) => system.id));
      setPresetPolicy(preset.policy);
      setPolicyText(preset.policy);
      setMode("live");
    });
    return () => {
      cancelled = true;
      runRef.current += 1;
      abortRef.current?.abort();
      if (sessionRef.current) deleteSession(sessionRef.current);
    };
  }, []);

  // Validate the editor's policy against the real loader, on a debounce.
  useEffect(() => {
    if (mode !== "live") return;
    const controller = new AbortController();
    const timer = setTimeout(() => {
      void checkPolicy(policyText, systems, controller.signal)
        .then((result) => {
          if (!result.ok) {
            setPolicyStatus({ ok: false, text: result.error });
            return;
          }
          if (result.boundary) {
            setBoundary(result.boundary);
            boundaryRef.current = result.boundary;
            // With no session running the label simply *is* the boundary, so
            // editing `[boundary]` moves the pills. Once a session exists its
            // live label owns them until New chat.
            if (!sessionRef.current) setLabel(result.boundary);
          }
          const notes = [`${result.tools} tools`];
          if (result.unconstrained?.length) notes.push(`${result.unconstrained.length} unconstrained`);
          if (result.ignored?.length) notes.push(`ignoring ${result.ignored.join(", ")} — system off`);
          setPolicyStatus({ ok: true, text: `loads clean · ${notes.join(" · ")}` });
        })
        .catch(() => {});
    }, 500);
    return () => {
      clearTimeout(timer);
      controller.abort();
    };
  }, [policyText, systems, mode]);

  // The lit block holds while its action is in flight; once the turn
  // settles it lingers a moment and fades.
  useEffect(() => {
    if (!focus?.settled) return;
    const timer = setTimeout(() => setFocus(null), 4000);
    return () => clearTimeout(timer);
  }, [focus]);

  // Esc leaves the full-screen editor.
  useEffect(() => {
    if (!editorMax) return;
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape") setEditorMax(false);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [editorMax]);

  const nextId = () => ++idRef.current;
  const push = (item: ThreadItem) => setThread((prev) => [...prev, item]);

  // Mirror of `boundary` readable from stable callbacks.
  const boundaryRef = useRef<LabelState | null>(null);

  /**
   * Animate a label change: a clone of the pills lifts off from the card
   * that caused it and flies to the header, which updates on landing. The
   * flight is pure decoration — reduced motion (or a missing card) gets the
   * plain update.
   */
  const flyLabel = useCallback((sourceId: number | null, next: LabelState) => {
    const land = () => {
      setLabel(next);
      pillsRef.current?.animate(
        [{ transform: "scale(1)" }, { transform: "scale(1.14)" }, { transform: "scale(1)" }],
        { duration: 260, easing: "ease-out" },
      );
    };
    const source = sourceId !== null ? document.querySelector(`[data-item-id="${sourceId}"]`) : null;
    const target = pillsRef.current;
    if (!source || !target || window.matchMedia("(prefers-reduced-motion: reduce)").matches) {
      setLabel(next);
      return;
    }

    const pillCss = (bg: string, fg: string) =>
      `font-size:10.5px;letter-spacing:0.07em;text-transform:uppercase;padding:0.12rem 0.55rem;` +
      `border-radius:999px;white-space:nowrap;background:${bg};color:${fg};`;
    const clean = next.trust === boundaryRef.current?.trust;
    const flyer = document.createElement("div");
    flyer.style.cssText = "position:fixed;z-index:60;display:flex;gap:0.4rem;pointer-events:none;font-family:inherit;";
    const trust = document.createElement("span");
    trust.style.cssText = pillCss(clean ? "#1f6b46" : "#8c5f1f", clean ? "#eafff2" : "#fff4e0");
    trust.textContent = `trust: ${next.trust}`;
    const audience = document.createElement("span");
    audience.style.cssText = pillCss("#3f3168", "#efe9ff");
    audience.textContent = `audience: ${next.audience}`;
    flyer.append(trust, audience);
    document.body.append(flyer);

    const from = source.getBoundingClientRect();
    const to = target.getBoundingClientRect();
    const width = flyer.getBoundingClientRect().width;
    const startLeft = Math.max(8, Math.min(from.right - width - 12, window.innerWidth - width - 8));
    const startTop = from.top + 10;
    flyer.style.left = `${startLeft}px`;
    flyer.style.top = `${startTop}px`;

    const animation = flyer.animate(
      [
        { transform: "translate(0, 0) scale(1)", opacity: 0.95 },
        { transform: `translate(${to.left - startLeft}px, ${to.top - startTop}px) scale(0.92)`, opacity: 0.55 },
      ],
      { duration: 650, easing: "cubic-bezier(0.3, 0.7, 0.2, 1)" },
    );
    let landed = false;
    const done = () => {
      if (landed) return;
      landed = true;
      flyer.remove();
      land();
    };
    animation.onfinish = done;
    animation.oncancel = done;
    // Belt and braces: never leave the pills stale if the animation dies.
    setTimeout(done, 900);
  }, []);

  const resetChat = useCallback(() => {
    runRef.current += 1;
    abortRef.current?.abort();
    if (sessionRef.current) deleteSession(sessionRef.current);
    sessionRef.current = null;
    setThread([]);
    setChildIds(new Set());
    setLabel(boundary);
    setBusy(false);
    setTurns(0);
    setInput("");
  }, [boundary]);

  // ---- live path ----------------------------------------------------------

  const applyEvent = useCallback((event: DemoEvent) => {
    switch (event.type) {
      case "says":
        push({ id: nextId(), t: "text", text: event.text, traj: event.trajectory });
        break;
      case "tool_proposed":
        // Light the contract the engine is about to check. The harness's own
        // tools have no contract in the editor.
        if (!PROTOCOL_TOOLS.has(event.tool)) setFocus({ name: event.tool, at: Date.now() });
        push({
          id: nextId(),
          t: "tool",
          callId: event.call_id,
          name: event.tool,
          args: event.arguments,
          state: "running",
          traj: event.trajectory,
        });
        break;
      case "blocked":
        // The runtime has one channel for text fed back to the model on a
        // call, and it carries harness acknowledgments as well as rulings —
        // "result submitted to the parent" arrives the same way a refusal
        // does. Feedback on the harness's own protocol tools (submit_result,
        // fork) is an outcome, not a ruling, so it closes the card calmly
        // instead of wearing the error styling.
        setThread((prev) =>
          prev.map((item) => {
            if (item.t !== "tool" || item.callId !== event.call_id || item.state !== "running") return item;
            return PROTOCOL_TOOLS.has(item.name)
              ? { ...item, state: "done" as const, output: event.text }
              : { ...item, state: "blocked" as const, blocked: event.text };
          }),
        );
        break;
      case "tool_result":
        // The admission and the close land in the same batch, in either
        // order: attach to the oldest running card, else the newest done
        // card still missing its body.
        setThread((prev) => {
          const running = prev.findIndex((item) => item.t === "tool" && item.state === "running");
          const at =
            running !== -1
              ? running
              : prev.length -
                1 -
                [...prev].reverse().findIndex((item) => item.t === "tool" && item.state === "done" && !item.result);
          const item = prev[at];
          if (!item || item.t !== "tool") return prev;
          const next = [...prev];
          next[at] = { ...item, result: event.body };
          return next;
        });
        break;
      case "tool_closed":
        setThread((prev) => {
          const index = prev.findIndex((item) => item.t === "tool" && item.state === "running");
          if (index === -1) return prev;
          const next = [...prev];
          const item = next[index] as Extract<ThreadItem, { t: "tool" }>;
          next[index] = {
            ...item,
            state: "done",
            output:
              event.outcome === "ran"
                ? event.effects.length
                  ? `ran, committing [${event.effects.join(", ")}]`
                  : "ran"
                : event.outcome,
          };
          return next;
        });
        break;
      case "label": {
        // Attach the delta pill to the card that caused the change, and
        // remember that card as the launch pad for the flight.
        let sourceId: number | null = null;
        setThread((prev) => {
          const index = [...prev].reverse().findIndex((item) => item.t === "tool" && item.state === "done");
          if (index === -1) return prev;
          const at = prev.length - 1 - index;
          const item = prev[at] as Extract<ThreadItem, { t: "tool" }>;
          sourceId = item.id;
          if (item.delta) return prev;
          const next = [...prev];
          next[at] = { ...item, delta: `trust: ${event.trust} · audience: ${event.audience}` };
          return next;
        });
        // Defer to after the state flush so the card (and its rect) exist.
        const next = { trust: event.trust, audience: event.audience };
        setTimeout(() => flyLabel(sourceId, next), 30);
        break;
      }
      case "approval_requested": {
        // The authority consulted is named in the request; light its block.
        const authority = (event.detail as { authority?: string }).authority;
        if (authority) setFocus({ name: authority, at: Date.now() });
        push({
          id: nextId(),
          t: "approval",
          approvalId: event.id,
          tool: event.tool,
          detail: JSON.stringify(event.detail, null, 2),
          state: "pending",
        });
        break;
      }
      case "approval_resolved":
        setThread((prev) =>
          prev.map((item) =>
            item.t === "approval" && item.approvalId === event.id
              ? { ...item, state: event.expired ? "expired" : event.approved ? "approved" : "denied" }
              : item,
          ),
        );
        break;
      case "remedy":
        push({ id: nextId(), t: "note", text: event.text, traj: event.trajectory });
        break;
      case "sanitized":
        // Mark the call whose result was replaced. The `tool_result` that follows
        // carries the derivation, so the card shows what the assistant actually read
        // and says where it came from — no separate line in the flow.
        setThread((prev) => {
          const index = prev.findLastIndex((item) => item.t === "tool" && item.state === "running");
          if (index === -1) return prev;
          return prev.map((item, at) =>
            at === index && item.t === "tool" ? { ...item, sanitizedBy: event.sanitizer } : item,
          );
        });
        break;
      case "merge":
        // A return is its own checked crossing: the branch confined what the
        // child *kept*, not what it hands back. If the returned value is
        // restricted, the label event right after shows the parent paying for
        // it — so say what crossed, not that it was free.
        push({ id: nextId(), t: "note", text: "child returned a value — checked at the merge" });
        break;
      case "fork":
        // No note: the branch block's own header marks the fork.
        setChildIds((prev) => new Set(prev).add(event.child));
        break;
      case "answer":
        // The final answer is already in the log as the last assistant message;
        // the outcome repeats it, so only append when the loop ended some other way.
        setThread((prev) => {
          const lastText = [...prev].reverse().find((item) => item.t === "text");
          if (lastText && lastText.t === "text" && lastText.text.trim() === event.text.trim()) return prev;
          return [...prev, { id: nextId(), t: "text", text: event.text }];
        });
        setFocus((prev) => (prev ? { ...prev, settled: true } : prev));
        break;
      case "stopped":
        push({ id: nextId(), t: "note", text: event.text });
        setFocus((prev) => (prev ? { ...prev, settled: true } : prev));
        break;
      case "error":
        push({ id: nextId(), t: "note", text: event.message });
        setFocus((prev) => (prev ? { ...prev, settled: true } : prev));
        break;
    }
  }, []);

  const sendLive = useCallback(
    async (text: string) => {
      const run = ++runRef.current;
      setBusy(true);
      setFocus(null); // a new turn starts with no block acting
      if (turns > 0) push({ id: nextId(), t: "rule" });
      push({ id: nextId(), t: "user", text });
      setTurns((count) => count + 1);

      try {
        if (!sessionRef.current) {
          const info = await createSession(policyText, systems, model);
          if (runRef.current !== run) return;
          sessionRef.current = info.session;
          setLabel({ trust: info.trust, audience: info.audience });
        }
        const controller = new AbortController();
        abortRef.current = controller;
        await streamTurn(
          sessionRef.current,
          text,
          apiKey,
          (event) => {
            if (runRef.current === run) applyEvent(event);
          },
          controller.signal,
        );
      } catch (error) {
        if (runRef.current === run && (error as Error).name !== "AbortError") {
          push({ id: nextId(), t: "note", text: (error as Error).message });
        }
      } finally {
        if (runRef.current === run) setBusy(false);
      }
    },
    [apiKey, applyEvent, model, policyText, systems, turns],
  );

  const send = useCallback(
    (text: string) => {
      const trimmed = text.trim();
      if (!trimmed || busy || mode !== "live") return;
      setInput("");
      void sendLive(trimmed);
    },
    [busy, mode, sendLive],
  );

  const stop = useCallback(() => {
    abortRef.current?.abort();
    setBusy(false);
  }, []);

  const onSubmit = (message: PromptInputMessage) => send(message.text);

  const live = mode === "live";
  const needsKey = live && !apiKey.trim();

  // The line range the engine is acting on, located in the visitor's own
  // policy text — absent when the focused name has no block there.
  const highlight = useMemo(() => (focus ? findBlock(policyText, focus.name) : null), [focus, policyText]);

  // Post the visitor's ruling; the resolved state arrives back on the stream.
  const answerApproval = async (approvalId: string, approve: boolean) => {
    if (!sessionRef.current) return;
    try {
      await respondApproval(sessionRef.current, approvalId, approve);
    } catch (error) {
      push({ id: nextId(), t: "note", text: (error as Error).message });
    }
  };

  const renderItem = (item: ThreadItem) => {
    if (item.t === "user")
      return (
        <Message from="user" key={item.id}>
          <MessageContent>{item.text}</MessageContent>
        </Message>
      );
    if (item.t === "text")
      return (
        <Message from="assistant" key={item.id}>
          <MessageContent>
            <MessageResponse>{item.text}</MessageResponse>
          </MessageContent>
        </Message>
      );
    if (item.t === "note")
      return (
        <div key={item.id} className="font-mono text-[11px] text-[var(--icon)]">
          ⎿ {item.text}
        </div>
      );
    if (item.t === "rule") return <div className="border-t border-dashed border-[var(--border-weak)]" key={item.id} />;
    if (item.t === "approval") {
      const verdict =
        item.state === "approved"
          ? { text: "approved", bg: "var(--accent-bg)", fg: "var(--accent)" }
          : item.state === "denied"
            ? { text: "denied", bg: "var(--danger-bg)", fg: "var(--danger)" }
            : item.state === "expired"
              ? { text: "expired — abstained", bg: "var(--bg-weak-hover)", fg: "var(--icon)" }
              : null;
      return (
        <div
          className="w-full min-w-0 max-w-[95%] rounded-md border border-[var(--warn)] bg-[var(--warn-bg)] p-3"
          key={item.id}
        >
          <div className="flex flex-wrap items-center gap-2">
            <span style={pill("var(--warn)", "var(--warn-bg)")}>human sign-off</span>
            <span className="font-mono text-xs text-[var(--text-strong)]">{item.tool}</span>
            {verdict && <span style={pill(verdict.bg, verdict.fg)}>{verdict.text}</span>}
          </div>
          <p className="m-0 mt-2 text-[11.5px] leading-relaxed text-[var(--warn)]">
            This call needs your approval. No answer within the window keeps it blocked.
          </p>
          {item.state === "pending" && (
            <div className="mt-2 flex gap-2">
              <button
                className="rounded-md bg-[var(--accent)] px-3 py-1 font-mono text-[11px] text-[var(--accent-bg)]"
                onClick={() => void answerApproval(item.approvalId, true)}
                type="button"
              >
                Approve
              </button>
              <button
                className="rounded-md bg-[var(--danger)] px-3 py-1 font-mono text-[11px] text-[var(--danger-bg)]"
                onClick={() => void answerApproval(item.approvalId, false)}
                type="button"
              >
                Deny
              </button>
            </div>
          )}
          <details className="mt-2">
            <summary className="cursor-pointer font-mono text-[10.5px] text-[var(--warn)]">
              what the authority sees
            </summary>
            <pre className="m-0 mt-1 max-h-40 overflow-auto rounded-md bg-[var(--bg)] p-2 font-mono text-[11px] leading-relaxed whitespace-pre-wrap break-words">
              {item.detail}
            </pre>
          </details>
        </div>
      );
    }
    // The engine's own remedy step is not a tool call on the world, so it
    // does not wear a tool card: a small APPA mark in the flow, expandable
    // to the plan it executed and what came back.
    if (item.name === "execute_remedy_plan") {
      return (
        <details className="w-full min-w-0 max-w-[95%]" data-item-id={item.id} key={item.id}>
          <summary
            className="flex cursor-pointer list-none items-center gap-2 [&::-webkit-details-marker]:hidden"
            title="APPA remedy step — click to expand"
          >
            <span
              className={`flex size-7 shrink-0 items-center justify-center rounded-full border border-[var(--accent-border)] bg-[var(--accent-bg)] ${
                item.state === "running" ? "animate-pulse" : ""
              }`}
            >
              <appa-mark size={17} />
            </span>
            <span className="font-mono text-[10.5px] tracking-widest text-[var(--icon)] uppercase">
              appa · remedy {item.state === "running" ? "· executing…" : ""}
            </span>
          </summary>
          <div className="mt-2 ml-9 rounded-md border border-[var(--border-weak)] bg-[var(--bg-weak)] p-2 font-mono text-[11px] leading-relaxed">
            <div className="break-words">execute_remedy_plan {JSON.stringify(item.args)}</div>
            {(item.output ?? item.blocked) && (
              <div className="mt-1 break-words whitespace-pre-wrap text-[var(--text-weak)]">
                ⎿ {item.output ?? item.blocked}
              </div>
            )}
          </div>
        </details>
      );
    }
    const state =
      item.state === "running"
        ? ("input-available" as const)
        : item.state === "blocked"
          ? ("output-error" as const)
          : ("output-available" as const);
    const ruling = item.state === "blocked" && item.blocked ? splitRuling(item.blocked) : null;
    // A ruling is the engine mediating — the thing the demo exists to show —
    // so it gets verdict styling, never the error slot. Red stays reserved
    // for service failures.
    const verdict = ruling
      ? ruling.kind === "narrowing"
        ? { badge: rulingBadge(ruling), bg: "var(--warn-bg)", fg: "var(--warn)" }
        : { badge: rulingBadge(ruling), bg: "var(--danger-bg)", fg: "var(--danger)" }
      : null;
    return (
      <div className="w-full min-w-0 max-w-[95%]" data-item-id={item.id} key={item.id}>
        <Tool className="w-full">
        <ToolHeader
          badge={verdict ? <span style={pill(verdict.bg, verdict.fg)}>{verdict.badge}</span> : undefined}
          state={state}
          type={`tool-${item.name}` as `tool-${string}`}
        />
        <ToolContent>
          <ToolInput input={item.args} />
          {ruling && verdict ? (
            <div className="space-y-2">
              <h4 className="font-medium text-muted-foreground text-xs uppercase tracking-wide">APPA</h4>
              <p className="m-0 text-[11.5px] leading-relaxed text-[var(--text-weak)]">{rulingCaption(ruling)}</p>
              <div className="overflow-x-auto rounded-md text-xs" style={{ background: verdict.bg, color: verdict.fg }}>
                <div className="whitespace-pre-wrap break-words p-2">{ruling.summary}</div>
                {ruling.detail && (
                  <pre className="m-0 max-h-40 overflow-auto p-2 font-mono text-[11px] leading-relaxed">
                    {ruling.detail}
                  </pre>
                )}
              </div>
              {ruling.offers.length > 0 && (
                <div className="space-y-1.5 rounded-md bg-[var(--bg-weak)] p-2">
                  <div className="font-medium text-[10.5px] uppercase tracking-wide text-[var(--text-weak)]">
                    Or clean the result first
                  </div>
                  {ruling.offers.map((offer) => (
                    <div className="text-[11.5px] leading-relaxed" key={offer.sanitizer}>
                      <span className="font-mono text-[var(--text-strong)]">{offer.sanitizer}</span>
                      {offer.free && (
                        <span className="ml-1.5" style={pill("var(--accent-bg)", "var(--accent)")}>
                          costs nothing
                        </span>
                      )}
                      {offer.hint && <div className="text-[var(--text-weak)]">{offer.hint}</div>}
                    </div>
                  ))}
                </div>
              )}
            </div>
          ) : (
            <ToolOutput
              errorText={undefined}
              output={
                item.state === "done" && item.output ? (
                  <div className="p-2 font-mono text-xs">
                    <div className="flex flex-wrap items-center gap-2">
                      <span>⎿ {item.output}</span>
                      {item.delta && <span style={pill("var(--warn-bg)", "var(--warn)")}>{item.delta}</span>}
                      {item.sanitizedBy && (
                        <span style={pill("var(--accent-bg)", "var(--accent)")}>
                          cleaned by {item.sanitizedBy}
                        </span>
                      )}
                    </div>
                    {item.sanitizedBy && (
                      <div className="mt-1 font-sans text-[11px] text-[var(--text-weak)]">
                        The assistant never saw the raw result — only this.
                      </div>
                    )}
                    {item.result && (
                      <pre className="m-0 mt-2 max-h-40 overflow-auto whitespace-pre-wrap break-words rounded-md bg-[var(--bg-weak)] p-2 text-[11px] leading-relaxed">
                        {item.result}
                      </pre>
                    )}
                  </div>
                ) : undefined
              }
            />
          )}
        </ToolContent>
        </Tool>
      </div>
    );
  };

  // The whole right-hand pane, rendered into the desktop sidebar or the
  // mobile drawer — one JSX value so the two never drift.
  const policyPane = (
    <>
          <div className="flex items-center gap-1 px-3 pt-3 pb-2">
            {(["tools", "policy"] as const).map((tab) => (
              <button
                className={
                  pane === tab
                    ? "rounded-md bg-[var(--bg-weak)] px-2.5 py-1 font-mono text-[11px] text-[var(--text-strong)]"
                    : "rounded-md px-2.5 py-1 font-mono text-[11px] text-[var(--text-weak)] hover:text-[var(--text-strong)]"
                }
                key={tab}
                onClick={() => setPane(tab)}
                type="button"
              >
                {tab === "tools" ? `Tools · ${systems.length}/${catalog.length}` : "Policy"}
              </button>
            ))}
            {pane === "policy" && (
              <span className="ml-auto flex items-center gap-2.5">
                {presetPolicy && policyText !== presetPolicy && (
                  <button
                    className="font-mono text-[11px] text-[var(--text-weak)] underline underline-offset-2 hover:text-[var(--text-strong)]"
                    onClick={() => setPolicyText(presetPolicy)}
                    type="button"
                  >
                    reset
                  </button>
                )}
                <button
                  className="font-mono text-[11px] text-[var(--text-weak)] hover:text-[var(--text-strong)]"
                  onClick={() => setEditorMax(true)}
                  title="Edit full screen"
                  type="button"
                >
                  ⤢ full screen
                </button>
              </span>
            )}
          </div>

          {pane === "tools" ? (
            <div className="mx-3 flex-1 overflow-y-auto rounded-md border border-[var(--border-weak)] bg-[var(--bg-weak)] p-2">
              <p className="m-0 px-1 pb-2 text-[11px] leading-relaxed text-[var(--icon)]">
                Which systems the assistant has. Turning one on is what makes its tools exist — the policy decides what
                they may do.
              </p>
              {catalog.map((system) => {
                const on = systems.includes(system.id);
                return (
                  <label
                    className="flex cursor-pointer items-start gap-2 rounded-md px-1 py-1.5 hover:bg-[var(--bg-weak-hover)]"
                    key={system.id}
                  >
                    <input
                      checked={on}
                      className="mt-0.5 accent-[var(--accent)]"
                      onChange={() =>
                        setSystems((prev) =>
                          prev.includes(system.id) ? prev.filter((id) => id !== system.id) : [...prev, system.id],
                        )
                      }
                      type="checkbox"
                    />
                    <span className="min-w-0">
                      <span className="block text-[12px] text-[var(--text-strong)]">{system.label}</span>
                      <span className="block text-[11px] leading-snug text-[var(--text-weak)]">{system.blurb}</span>
                      <span className="mt-0.5 block truncate font-mono text-[10px] text-[var(--icon)]">
                        {system.tools.join(" · ")}
                      </span>
                    </span>
                  </label>
                );
              })}
            </div>
          ) : (
            <PolicyEditor className="mx-3 min-h-[12rem] flex-1" highlight={highlight} onChange={setPolicyText} value={policyText} />
          )}
          <div className="flex flex-col gap-2 p-3">
            <p
              className="m-0 text-[11.5px] leading-relaxed"
              style={{ color: policyStatus && !policyStatus.ok ? "var(--danger)" : "var(--icon)" }}
            >
              {policyStatus
                ? policyStatus.text
                : "Changes apply to the next chat — a session keeps the tools and policy it started with."}
            </p>
            <div className="flex items-center gap-2 rounded-md border border-[var(--border)] px-2.5 py-1.5">
              <span className="text-[11px] whitespace-nowrap text-[var(--icon)]">OpenRouter key</span>
              <input
                className="min-w-0 flex-1 bg-transparent text-right font-mono text-xs text-[var(--text)] outline-none"
                onChange={(event) => {
                  setApiKey(event.currentTarget.value);
                  window.localStorage.setItem(KEY_STORAGE, event.currentTarget.value);
                }}
                placeholder="sk-or-…"
                type="password"
                value={apiKey}
              />
            </div>
            <div className="flex items-center gap-2 rounded-md border border-[var(--border)] px-2.5 py-1.5">
              <span className="text-[11px] whitespace-nowrap text-[var(--icon)]">Model</span>
              <select
                className="min-w-0 flex-1 cursor-pointer bg-transparent text-right font-mono text-xs text-[var(--text)] outline-none"
                onChange={(event) => setModel(event.currentTarget.value)}
                value={model}
              >
                {PLAYGROUND_MODELS.map((m) => (
                  <option key={m.id} value={m.id}>
                    {m.label}
                  </option>
                ))}
              </select>
            </div>
          </div>
    </>
  );

  return (
    <div className="flex h-full min-h-0 flex-col bg-[var(--bg)]">
      <div style={chrome.bar}>
        <span style={chrome.title}>chat with openappa</span>
        {mode === "probing" && <span style={pill("var(--bg-weak-hover)", "var(--icon)")}>connecting…</span>}
        {mode === "down" && <span style={pill("var(--danger-bg)", "var(--danger)")}>service unavailable</span>}
        {live && <span style={pill("var(--accent-bg)", "var(--accent)")}>live agent</span>}
        <span style={{ marginLeft: "auto", display: "flex", gap: "0.5rem", alignItems: "center", flexWrap: "wrap" }}>
          <span style={{ fontSize: 11, color: "var(--icon)" }}>turn {turns}</span>
          <span ref={pillsRef} style={{ display: "flex", gap: "0.4rem" }}>
            {label && <LabelPills boundary={boundary} label={label} />}
          </span>
          <button type="button" onClick={resetChat} className="lp-replay" style={chrome.barBtn}>
            New chat
          </button>
          <button className="lg:hidden" onClick={() => setPanelOpen(true)} style={chrome.barBtn} type="button">
            Tools · policy
          </button>
        </span>
      </div>

      {/* The sidebar is sized so the preset's longest contract line fits the
          editor unwrapped at its 13px mono; on narrower screens it cedes
          room to the chat and accepts a wrap. */}
      <div className="grid min-h-0 flex-1 grid-cols-1 lg:grid-cols-[minmax(0,1fr)_34rem] xl:grid-cols-[minmax(0,1fr)_42rem]">
        {/* ---- chat pane ---- */}
        <div className="flex min-h-0 min-w-0 flex-col border-b border-[var(--border-weak)] bg-[var(--bg)] lg:border-r lg:border-b-0">
          <Conversation className="min-h-0">
            <ConversationContent className="gap-4">
              {thread.length === 0 ? (
                // An empty live chat shows nothing; only a service problem
                // earns a message.
                mode === "live" ? null : (
                  <ConversationEmptyState
                    title={mode === "down" ? "The demo service is offline" : "Connecting…"}
                    description={
                      mode === "probing"
                        ? "Connecting to the demo service…"
                        : "This playground runs a real agent, so it needs the service. Nothing here is a recording — try again shortly."
                    }
                  />
                )
              ) : (
                segmentThread(thread, childIds).map((segment) =>
                  segment.child ? (
                    <details
                      className="w-full min-w-0 max-w-[95%] rounded-md border border-dashed border-[var(--border-weak)]"
                      key={segment.items[0].id}
                    >
                      <summary className="cursor-pointer px-3 py-2 font-mono text-[11px] text-[var(--text-weak)] hover:text-[var(--text-strong)]">
                        ⑂ child {segment.child} — what it reads narrows this branch only ·{" "}
                        {segment.items.length} steps
                      </summary>
                      <div className="mb-2 ml-3 flex flex-col gap-3 border-l-2 border-[var(--warn)] py-1 pr-2 pl-3">
                        {segment.items.map(renderItem)}
                      </div>
                    </details>
                  ) : (
                    segment.items.map(renderItem)
                  ),
                )
              )}
            </ConversationContent>
            <ConversationScrollButton />
          </Conversation>

          <div className="flex flex-col gap-2 border-t border-[var(--border-weak)] p-3">
            <PromptInput className="relative" onSubmit={onSubmit}>
              <PromptInputTextarea
                className="pr-12"
                disabled={!live}
                onChange={(event) => setInput(event.currentTarget.value)}
                placeholder={
                  mode === "probing"
                    ? "Connecting…"
                    : mode === "down"
                      ? "The demo service is offline."
                      : needsKey
                        ? "Add your OpenRouter key on the right to start chatting…"
                        : "Message the assistant…"
                }
                value={input}
              />
              <PromptInputSubmit
                className="absolute right-1 bottom-1"
                disabled={!busy && (!live || !input.trim() || needsKey)}
                onStop={stop}
                status={busy ? "streaming" : "ready"}
              />
            </PromptInput>
          </div>
        </div>

        {/* ---- policy pane: static sidebar on desktop ---- */}
        <div className="hidden min-h-0 min-w-0 flex-col bg-[var(--bg)] lg:flex">{policyPane}</div>
      </div>

      {/* On mobile the pane is a right-side drawer, out of the chat's way. */}
      {panelOpen && (
        <div className="fixed inset-0 z-40 lg:hidden">
          <div aria-hidden className="absolute inset-0 bg-black/30" onClick={() => setPanelOpen(false)} />
          <div className="absolute top-0 right-0 flex h-full w-[88vw] max-w-[26rem] flex-col border-l border-[var(--border-weak)] bg-[var(--bg)] shadow-xl">
            <div className="flex items-center justify-between px-3 pt-3">
              <span className="font-mono text-[11px] text-[var(--icon)]">tools & policy</span>
              <button
                className="rounded-md border border-[var(--border-weak)] px-2 py-0.5 font-mono text-[11px] text-[var(--text-weak)]"
                onClick={() => setPanelOpen(false)}
                type="button"
              >
                close
              </button>
            </div>
            {policyPane}
          </div>
        </div>
      )}

      {editorMax && (
        <div className="fixed inset-0 z-50 flex flex-col bg-[var(--bg)] p-4 md:p-8">
          <div className="flex items-center gap-3 pb-3">
            <span className="font-mono text-sm text-[var(--text-strong)]">default.toml</span>
            <p
              className="m-0 min-w-0 flex-1 truncate text-right text-[11.5px] leading-relaxed"
              style={{ color: policyStatus && !policyStatus.ok ? "var(--danger)" : "var(--icon)" }}
            >
              {policyStatus?.text ?? ""}
            </p>
            {presetPolicy && policyText !== presetPolicy && (
              <button
                className="font-mono text-[11px] text-[var(--text-weak)] underline underline-offset-2 hover:text-[var(--text-strong)]"
                onClick={() => setPolicyText(presetPolicy)}
                type="button"
              >
                reset
              </button>
            )}
            <button
              className="rounded-md border border-[var(--border-weak)] px-2.5 py-1 font-mono text-[11px] text-[var(--text-weak)] hover:text-[var(--text-strong)]"
              onClick={() => setEditorMax(false)}
              type="button"
            >
              close · esc
            </button>
          </div>
          <PolicyEditor autoFocus className="min-h-0 flex-1" highlight={highlight} onChange={setPolicyText} value={policyText} />
        </div>
      )}
    </div>
  );
}

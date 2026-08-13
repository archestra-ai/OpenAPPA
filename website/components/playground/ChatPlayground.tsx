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
  PLAYGROUND_MODEL,
  type PlaygroundSystem,
  describeSystem,
} from "./playground-data";
import { PolicyEditor, findBlock } from "./PolicyEditor";
import { registerPixelMarks } from "@/components/pixel-marks";
import { termDefinition } from "@/lib/terms";

declare module "react" {
  namespace JSX {
    interface IntrinsicElements {
      "appa-mark": React.DetailedHTMLProps<React.HTMLAttributes<HTMLElement>, HTMLElement> & {
        size?: number | string;
        /** Mascot overlay: "clean" sweeps a broom, "accept" carries a check. */
        variant?: "clean" | "accept";
      };
    }
  }
}

type ThreadItem = { lab?: LabelState } & (
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
      /** Set when the raw result was withheld and a sanitizer's derivation admitted. */
      sanitizedBy?: string;
      /** Set when the call ran under an authority's ruling. */
      approvedBy?: string;
      /**
       * A resolution card: the held call, run again after its remedy. The
       * original card keeps the block; this one carries the outcome.
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
  /** APPA's turn on a held call: the ruling text, rendered as a speaker. */
  | { id: number; t: "verdict"; text: string; traj?: string }
  /** The root label moved: a marker row that recolors the rail below it. */
  | { id: number; t: "shift"; from: LabelState; to: LabelState }
  /** Where the session entered: the label's opening row in the stream. */
  | { id: number; t: "entry"; label: LabelState }
  /** The closing recap of a demo case: what the path just showed and why. */
  | { id: number; t: "authors"; text: string }
  | { id: number; t: "rule" }
);

type Mode = "probing" | "live" | "down";

/**
 * Starters for an empty chat, each walking one of the demo's best paths:
 * recordings → issues → create_issue crosses both sanitizer territory and the
 * trust wall; invoices → make_transfer crosses the audience narrowing and the
 * treasurer's sign-off; invoices → email compares the narrowed audience with
 * the current readers behind each recipient list.
 */
const STARTER_PROMPTS = [
  {
    tag: "recordings → github",
    text: "Check the recent meeting recordings for bugs customers mentioned, and file any that are not on GitHub yet.",
    recap:
      "What this path shows: meeting recordings are untrusted input, so reading them would have dropped the whole chat to the suspicious rank — and a suspicious chat may not file public GitHub issues. APPA held that one tool call, priced the drop, and offered remedy plans; choosing the strip-customer-data sanitizer meant the assistant never saw the raw transcripts, only the cleaned derivation, so the session kept its label and the issues could still be filed. Every hold and remedy you watched was scoped to a single tool call — nothing was blanket-allowed or blanket-denied.",
  },
  {
    tag: "invoices → transfer",
    text: "Check the open invoices and pay the overdue one by transfer.",
    recap:
      "What this path shows: invoices are confidential, so the one call that read them carried a price — the chat's audience narrowed to the finance readers — and the agent accepted it knowingly. Moving money additionally demands a human ruling: make_transfer paused until the treasurer (that was you) approved that exact call, and no answer would have failed it closed. Each decision landed on a single tool call; the policy never had to trust the agent's intentions, only rule on its flows.",
  },
  {
    tag: "invoices → email",
    text: "Review the unpaid invoices and email a summary first to ap-review@corp.example. After that succeeds, send the same summary to finance-all@corp.example.",
    recap:
      "What this path shows: once the invoices were read, their summary belonged to the finance audience. An email is a flow to whoever is currently behind the recipient list, so APPA resolved each address to its live readers and compared them with the label — the same summary passed for one list and was refused for the other, decided per tool call at the moment of sending. That is the point of value-granular flow control: verdicts follow the data and its readers, not tool names or good intentions.",
  },
];

/**
 * The harness's own tools (`appa_runtime::tool`). Feedback on these is
 * protocol dialogue — an acknowledgment, a cost statement, a stale-offer
 * notice — never a policy ruling on a flow; rulings land on the blocked
 * tool's own card. So their cards close calmly instead of styling as errors.
 */
const PROTOCOL_TOOLS = new Set(["execute_remedy_plan", "fork", "submit_result"]);

/** Resizing floors: the policy pane and the chat pane each keep a readable
 *  minimum. The chat floor matches the grid's larger (xl) floor so a drag
 *  never stores a width the columns would refuse to grant. */
const SIDEBAR_MIN_PX = 320;
const CHAT_MIN_PX = 480;

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
/** One executable plan the block offers, with everything its option line needs. */
type ParsedPlan = {
  id: string;
  /** The authorities the plan consults, each with the operator's hint. */
  rulings: { authority: string; hint?: string }[];
  /** The output sanitizer the plan binds, with its hint. */
  sanitize?: { sanitizer: string; hint?: string };
  /** Whether executing the plan accepts the block's narrowing. */
  accepts: boolean;
};

type Ruling = { plans: ParsedPlan[]; summary: string; detail?: string } & (
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
        ? [{ authority: ruling.authority, hint: typeof ruling.hint === "string" ? ruling.hint : undefined }]
        : [],
    );
    const sanitize =
      typeof entry.sanitizes?.sanitizer === "string"
        ? {
            sanitizer: entry.sanitizes.sanitizer,
            hint: typeof entry.sanitizes.hint === "string" ? entry.sanitizes.hint : undefined,
          }
        : undefined;
    return [{ id: entry.plan_id, rulings, sanitize, accepts: Boolean(entry.accepts_narrowing) }];
  });
}

function splitRuling(text: string): Ruling {
  const start = text.indexOf("\n{");
  if (start === -1) return { kind: "refusal", summary: text, plans: [] };
  const summary = text.slice(0, start).trim();
  const detail = text.slice(start + 1);
  try {
    const parsed = JSON.parse(detail) as {
      narrowing?: { from?: Record<string, unknown>; to?: Record<string, unknown> };
      requirement_gaps?: unknown[];
      remedy_plans?: unknown;
    };
    const pretty = JSON.stringify(parsed, null, 2);
    const plans = parsePlans(parsed.remedy_plans);
    const narrowing = parsed.narrowing;
    if (!narrowing || parsed.requirement_gaps?.length) return { kind: "refusal", summary, detail: pretty, plans };
    const moved = (dim: string) => JSON.stringify(narrowing.from?.[dim]) !== JSON.stringify(narrowing.to?.[dim]);
    const dimension = moved("trust") && moved("audience") ? "both" : moved("audience") ? "audience" : "trust";
    // `{"Known": {"Restricted": ["crm"]}}` — the engine's wire shape for a
    // concrete reader set; `{"Known": 0}` for a trust rank (a chain index).
    const known = (narrowing.to?.audience as { Known?: { Restricted?: unknown } } | undefined)?.Known;
    const readers = Array.isArray(known?.Restricted) ? known.Restricted.map(String) : undefined;
    const knownTrust = (narrowing.to?.trust as { Known?: unknown } | undefined)?.Known;
    const toTrustRank = typeof knownTrust === "number" ? knownTrust : undefined;
    return { kind: "narrowing", dimension, readers, toTrustRank, summary, detail: pretty, plans };
  } catch {
    return { kind: "refusal", summary, detail, plans: [] };
  }
}

/**
 * The policy's trust ranks, least-trusted first, read from the editor's own
 * text — a narrowing names its target trust as a chain index, and this is what
 * gives that index the name the visitor typed. The loader validates the real
 * parse; this regex only has to agree with it on the happy path.
 */
function parseTrustChain(policy: string): string[] {
  const match = policy.match(/trust_chain\s*=\s*\[([^\]]*)\]/);
  const names = match?.[1].match(/"([^"]*)"/g)?.map((quoted) => quoted.slice(1, -1));
  return names?.length ? names : ["suspicious", "trusted"];
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

/** The turns that belong to the APPA ↔ agent exchange, boxed as one. */
function isExchange(item: ThreadItem): boolean {
  return item.t === "verdict" || item.t === "approval" || (item.t === "tool" && item.name === "execute_remedy_plan");
}

// Toolbar chrome, matching the site's style strings.
const chrome = {
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
  // Desktop only: the policy pane's width once the visitor has dragged its
  // edge; null leaves the breakpoint defaults in charge. Remembered locally.
  const [sidebarPx, setSidebarPx] = useState<number | null>(null);
  const gridRef = useRef<HTMLDivElement>(null);
  // The contract or authority the engine is acting on right now, lit in the
  // editor. A focus holds until the next acting block replaces it; when the
  // turn settles (`settled`), it lingers briefly and fades.
  const [focus, setFocus] = useState<{ name: string; at: number; settled?: boolean } | null>(null);
  const [catalog, setCatalog] = useState<PlaygroundSystem[]>([]);
  const [presetPolicy, setPresetPolicy] = useState("");
  const [systems, setSystems] = useState<string[]>([]);
  const [policyText, setPolicyText] = useState("");
  const [policyStatus, setPolicyStatus] = useState<{ ok: boolean; text: string } | null>(null);

  const sessionRef = useRef<string | null>(null);
  const abortRef = useRef<AbortController | null>(null);
  const runRef = useRef(0);
  const idRef = useRef(0);

  useEffect(() => {
    registerPixelMarks();
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
            // With no session running the label simply *is* the boundary, so
            // editing `[boundary]` moves the pills. Once a session exists its
            // live label owns them until New chat.
            if (!sessionRef.current) {
              setLabel(result.boundary);
              labelRef.current = result.boundary;
            }
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

  // A remembered sidebar width survives New chat and reloads.
  useEffect(() => {
    const saved = Number(localStorage.getItem("appa-demo-sidebar-px"));
    if (saved >= SIDEBAR_MIN_PX) setSidebarPx(saved);
  }, []);

  // Drag the pane edge: width follows the pointer, floors on both sides.
  const startSidebarDrag = useCallback((event: React.PointerEvent) => {
    event.preventDefault();
    const grid = gridRef.current;
    if (!grid) return;
    const move = (pointer: PointerEvent) => {
      const rect = grid.getBoundingClientRect();
      const room = Math.max(SIDEBAR_MIN_PX, rect.width - CHAT_MIN_PX);
      setSidebarPx(Math.min(Math.max(rect.right - pointer.clientX, SIDEBAR_MIN_PX), room));
    };
    const up = () => {
      setSidebarPx((px) => {
        if (px !== null) localStorage.setItem("appa-demo-sidebar-px", String(Math.round(px)));
        return px;
      });
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", up);
      document.body.style.removeProperty("user-select");
      document.body.style.removeProperty("cursor");
    };
    document.body.style.userSelect = "none";
    document.body.style.cursor = "col-resize";
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", up);
  }, []);

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
  // Every pushed item is stamped with the root label at that moment — the
  // rail beside the thread is this history, not a live value.
  const push = (item: ThreadItem) =>
    setThread((prev) => [...prev, { ...item, lab: item.lab ?? labelRef.current ?? undefined }]);

  // The root trajectory's current label, readable from `applyEvent`.
  const labelRef = useRef<LabelState | null>(null);
  // Mirror of `childIds` for the same reason.
  const childIdsRef = useRef<Set<string>>(new Set());

  /**
   * Route a remedy's outcome to a resolution card: the held card keeps its
   * block, and a second card for the same call — appended after the APPA and
   * agent turns — carries what the remedy produced. Created on the first
   * outcome event of the batch, patched by the rest.
   */
  const resolveHeld = (prev: ThreadItem[], patch: Partial<Extract<ThreadItem, { t: "tool" }>>): ThreadItem[] => {
    const heldAt = prev.findLastIndex(
      (item) => item.t === "tool" && item.state === "blocked" && !PROTOCOL_TOOLS.has(item.name),
    );
    if (heldAt === -1) return prev;
    const held = prev[heldAt] as Extract<ThreadItem, { t: "tool" }>;
    const echoAt = prev.findLastIndex(
      (item, at) => at > heldAt && item.t === "tool" && Boolean(item.echo) && item.name === held.name,
    );
    if (echoAt !== -1)
      return prev.map((item, at) => (at === echoAt && item.t === "tool" ? { ...item, ...patch } : item));
    return [
      ...prev,
      {
        id: ++idRef.current,
        t: "tool",
        echo: true,
        callId: `${held.callId}+resolved`,
        name: held.name,
        args: held.args,
        state: "done",
        output: "ran",
        traj: held.traj,
        lab: labelRef.current ?? undefined,
        ...patch,
      },
    ];
  };

  const resetChat = useCallback(() => {
    runRef.current += 1;
    abortRef.current?.abort();
    if (sessionRef.current) deleteSession(sessionRef.current);
    sessionRef.current = null;
    setThread([]);
    childIdsRef.current = new Set();
    setChildIds(new Set());
    setLabel(boundary);
    labelRef.current = boundary;
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
      case "tool_proposed": {
        // Light the contract the engine is about to check. The harness's own
        // tools have no contract in the editor.
        if (!PROTOCOL_TOOLS.has(event.tool)) setFocus({ name: event.tool, at: Date.now() });
        const item: ThreadItem = {
          id: nextId(),
          t: "tool",
          callId: event.call_id,
          name: event.tool,
          args: event.arguments,
          state: "running",
          traj: event.trajectory,
          lab: labelRef.current ?? undefined,
        };
        setThread((prev) => {
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
        // fork) is an outcome, not a ruling, so it closes the card calmly
        // instead of wearing the error styling.
        setThread((prev) => {
          const index = prev.findIndex(
            (item) => item.t === "tool" && item.callId === event.call_id && item.state === "running",
          );
          if (index === -1) return prev;
          const item = prev[index] as Extract<ThreadItem, { t: "tool" }>;
          if (PROTOCOL_TOOLS.has(item.name))
            return prev.map((entry, at) =>
              at === index ? { ...item, state: "done" as const, output: event.text } : entry,
            );
          // The card records the hold; the ruling itself is APPA's own turn.
          const next = prev.map((entry, at) =>
            at === index ? { ...item, state: "blocked" as const, blocked: event.text } : entry,
          );
          next.push({
            id: ++idRef.current,
            t: "verdict",
            text: event.text,
            traj: item.traj,
            lab: labelRef.current ?? undefined,
          });
          return next;
        });
        break;
      case "tool_result":
        // The admission and the close land in the same batch, in either
        // order. Attach to the oldest running world card; failing that, to
        // the held card whose remedy produced this body — the result belongs
        // on the call the visitor watched get held, not on the protocol step
        // that unblocked it — then to any running card, then to the newest
        // done card still missing its body.
        setThread((prev) => {
          const runningWorld = prev.findIndex(
            (item) => item.t === "tool" && item.state === "running" && !PROTOCOL_TOOLS.has(item.name),
          );
          if (runningWorld !== -1) {
            const item = prev[runningWorld] as Extract<ThreadItem, { t: "tool" }>;
            const next = [...prev];
            next[runningWorld] = { ...item, result: event.body };
            return next;
          }
          const held = prev.findLastIndex(
            (item) => item.t === "tool" && item.state === "blocked" && !PROTOCOL_TOOLS.has(item.name),
          );
          if (held !== -1) return resolveHeld(prev, { result: event.body });
          const running = prev.findIndex((item) => item.t === "tool" && item.state === "running");
          const at =
            running !== -1
              ? running
              : prev.findLastIndex((item) => item.t === "tool" && item.state === "done" && !item.result);
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
        const next = { trust: event.trust, audience: event.audience };
        // A child's fold is its branch's own story: a line inside the branch
        // block, never a move on the root rail.
        if (childIdsRef.current.has(event.trajectory)) {
          push({
            id: nextId(),
            t: "note",
            text: `branch label → trust: ${event.trust} · audience: ${event.audience}`,
            traj: event.trajectory,
          });
          break;
        }
        // The rail recolors below the separator naming the move, and the
        // session-label pills update in place.
        const from = labelRef.current;
        if (from && (from.trust !== next.trust || from.audience !== next.audience)) {
          push({ id: nextId(), t: "shift", from, to: next, lab: next });
        }
        labelRef.current = next;
        setLabel(next);
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
          authority,
          detail: JSON.stringify(event.detail, null, 2),
          state: "pending",
        });
        break;
      }
      case "approval_resolved":
        setThread((prev) => {
          // The paused call resumes under this ruling: say so on its card.
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
              return { ...item, state: event.expired ? "expired" : event.approved ? "approved" : "denied" };
            if (at === running && item.t === "tool" && authority) return { ...item, approvedBy: authority };
            return item;
          });
        });
        break;
      case "remedy": {
        // "narrowing accepted" would repeat what the label separator (root)
        // or the branch-label note (child) already says: drop it. A ruling
        // lands on the card it approved instead of floating as a line.
        const approved = event.text.match(/^approved by (.+)$/);
        if (approved) {
          setThread((prev) => {
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
        push({ id: nextId(), t: "note", text: event.text, traj: event.trajectory });
        break;
      }
      case "sanitized":
        // Mark the call whose result was replaced: a running world card
        // completes in place; a held card resolves onto its echo.
        setThread((prev) => {
          const running = prev.findLastIndex(
            (item) => item.t === "tool" && item.state === "running" && !PROTOCOL_TOOLS.has(item.name),
          );
          if (running !== -1)
            return prev.map((item, at) =>
              at === running && item.t === "tool" ? { ...item, sanitizedBy: event.sanitizer } : item,
            );
          return resolveHeld(prev, { sanitizedBy: event.sanitizer });
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
        childIdsRef.current = new Set(childIdsRef.current).add(event.child);
        setChildIds(childIdsRef.current);
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
      // The stream tells the whole story on its own: it opens with the label
      // the session enters under, so a screenshot needs no chrome around it.
      // Before the loader's first answer there is nothing honest to show;
      // then the row is inserted once the session reports where it entered.
      const entryLabel = labelRef.current ?? boundary;
      const opening = turns === 0;
      if (opening && entryLabel) push({ id: nextId(), t: "entry", label: entryLabel });
      push({ id: nextId(), t: "user", text });
      setTurns((count) => count + 1);
      const starter = STARTER_PROMPTS.find((prompt) => prompt.text === text);

      try {
        if (!sessionRef.current) {
          const info = await createSession(policyText, systems, PLAYGROUND_MODEL.id);
          if (runRef.current !== run) return;
          sessionRef.current = info.session;
          setLabel({ trust: info.trust, audience: info.audience });
          labelRef.current = { trust: info.trust, audience: info.audience };
          if (opening && !entryLabel) {
            // The visitor beat the policy check's debounce: open the stream
            // with the label the session actually entered under.
            const label = { trust: info.trust, audience: info.audience };
            setThread((prev) => {
              const at = prev.findIndex((item) => item.t === "user");
              const item: ThreadItem = { id: ++idRef.current, t: "entry", label, lab: label };
              return at === -1 ? [item, ...prev] : [...prev.slice(0, at), item, ...prev.slice(at)];
            });
          }
        }
        const controller = new AbortController();
        abortRef.current = controller;
        await streamTurn(
          sessionRef.current,
          text,
          (event) => {
            if (runRef.current === run) applyEvent(event);
          },
          controller.signal,
        );
        // A demo case closes the way the docs' examples do: a recap of what
        // the path just showed and why it matters.
        if (runRef.current === run && starter) push({ id: nextId(), t: "authors", text: starter.recap });
      } catch (error) {
        if (runRef.current === run && (error as Error).name !== "AbortError") {
          push({ id: nextId(), t: "note", text: (error as Error).message });
        }
      } finally {
        if (runRef.current === run) setBusy(false);
      }
    },
    [applyEvent, boundary, policyText, systems, turns],
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

  // The line range the engine is acting on, located in the visitor's own
  // policy text — absent when the focused name has no block there.
  const highlight = useMemo(() => (focus ? findBlock(policyText, focus.name) : null), [focus, policyText]);

  // Names a narrowing's target trust rank in the visitor's own vocabulary.
  const trustChain = useMemo(() => parseTrustChain(policyText), [policyText]);

  // Post the visitor's ruling; the resolved state arrives back on the stream.
  const answerApproval = async (approvalId: string, approve: boolean) => {
    if (!sessionRef.current) return;
    try {
      await respondApproval(sessionRef.current, approvalId, approve);
    } catch (error) {
      push({ id: nextId(), t: "note", text: (error as Error).message });
    }
  };

  // ---- pills and APPA-turn chrome -----------------------------------------

  // A registered entity named in chat is a door into the policy: clicking it
  // opens the policy pane scrolled to that block, lit.
  const focusEntity = (name: string) => {
    setPane("policy");
    setFocus({ name, at: Date.now() });
  };

  const entityPill = (name: string, hint?: string) => (
    <button
      className="inline-block cursor-pointer rounded-full border border-[var(--accent-border)] bg-[var(--accent-bg)] px-2 font-mono text-[10.5px] leading-[1.6] text-[var(--accent)] hover:bg-[var(--bg-weak-hover)]"
      onClick={() => focusEntity(name)}
      title={hint ? `“${hint}” — click to see it in the policy` : "Click to see it in the policy"}
      type="button"
    >
      {name}
    </button>
  );

  // A label value wears the glossary: the same definition the docs popovers
  // use, as a plain tooltip.
  const termPill = (text: string) => (
    <span
      className="inline-block rounded-full border border-[var(--border-weak)] bg-[var(--bg-weak)] px-2 font-mono text-[10.5px] leading-[1.6] text-[var(--text-strong)]"
      style={termDefinition(text) ? { cursor: "help", borderBottomStyle: "dotted" } : undefined}
      title={termDefinition(text)}
    >
      {text}
    </span>
  );

  /** The narrowing's cost in plain words: what changes for this chat if we accept. */
  const acceptMove = (ruling: Extract<Ruling, { kind: "narrowing" }>) => {
    const moves: React.ReactNode[] = [];
    if (ruling.dimension !== "audience") {
      const to =
        ruling.toTrustRank !== undefined
          ? (trustChain[ruling.toTrustRank] ?? `rank ${ruling.toTrustRank}`)
          : "less trusted";
      moves.push(<span key="trust">the chat becomes {termPill(to)}</span>);
    }
    if (ruling.dimension !== "trust") {
      const to = ruling.readers ? ruling.readers.join(", ") || "nobody" : "fewer readers";
      moves.push(<span key="audience">only {termPill(to)} sees the results</span>);
    }
    return moves.map((move, index) => (
      <span key={index}>
        {index > 0 && " and "}
        {move}
      </span>
    ));
  };

  /** One plan as one option line: what "we" do, in the order the plan does it. */
  const planOption = (plan: ParsedPlan, ruling: Ruling) => {
    const clauses: React.ReactNode[] = [];
    for (const ruled of plan.rulings)
      clauses.push(<>we ask {entityPill(ruled.authority, ruled.hint)} for approval</>);
    if (plan.sanitize) clauses.push(<>we clean it with {entityPill(plan.sanitize.sanitizer, plan.sanitize.hint)}</>);
    if (plan.accepts || clauses.length === 0)
      clauses.push(ruling.kind === "narrowing" ? <>we accept — {acceptMove(ruling)}</> : <>we accept the cost</>);
    return (
      <li className="my-1" key={plan.id}>
        {clauses.map((clause, index) => (
          <span key={index}>
            {index > 0 && " and "}
            {clause}
          </span>
        ))}
      </li>
    );
  };

  const appaWho = (badge?: React.ReactNode) => (
    <div className="mb-1.5 flex items-center gap-2 font-mono text-[11px] tracking-[0.12em] text-[var(--text-weak)] uppercase">
      <span className="flex size-7 shrink-0 items-center justify-center rounded-full border border-[var(--accent-border)] bg-[var(--accent-bg)]">
        <appa-mark size={17} />
      </span>
      appa
      {badge && <span className="ml-auto">{badge}</span>}
    </div>
  );

  const agentWho = (
    <div className="mb-1.5 flex items-center gap-2 font-mono text-[11px] tracking-[0.12em] text-[var(--text-weak)] uppercase">
      <span className="flex size-7 shrink-0 items-center justify-center rounded-full border border-[var(--border)] bg-[var(--bg)] text-[15px]">
        🤖
      </span>
      agent
    </div>
  );

  /**
   * One grey container for the APPA ↔ agent exchange. Consecutive exchange
   * turns merge into a single box: rounding and top padding only at the run's
   * edges, and the rows between them close their gap. The box opens by naming
   * the one tool call under negotiation — the exchange never widens beyond it.
   */
  const exchangeShell = (
    group: { first: boolean; last: boolean } | undefined,
    children: React.ReactNode,
    tool?: string,
  ) => {
    const edges = group ?? { first: true, last: true };
    return (
      <div
        className={`w-full min-w-0 bg-[var(--bg-weak)] px-4 pb-4 ${
          edges.first ? "rounded-t-md pt-4" : "pt-0"
        } ${edges.last ? "rounded-b-md" : ""}`}
      >
        {edges.first && tool && (
          <div className="mb-2.5 border-b border-[var(--border-weak)] pb-1.5 font-mono text-[10px] tracking-widest text-[var(--icon)] uppercase">
            negotiating one tool call · {tool}
          </div>
        )}
        {children}
      </div>
    );
  };

  /** The world call this exchange negotiates: the nearest held card above. */
  const heldToolBefore = (id: number) => {
    const at = thread.findIndex((entry) => entry.id === id);
    for (let index = (at === -1 ? thread.length : at) - 1; index >= 0; index--) {
      const prev = thread[index];
      if (prev.t === "tool" && prev.state === "blocked" && !PROTOCOL_TOOLS.has(prev.name)) return prev.name;
    }
    return undefined;
  };

  /**
   * The plan an `execute_remedy_plan` call chose, resolved against the block
   * that offered it — the nearest earlier blocked card whose ruling carries
   * that plan_id. Lets the choice render in the agent's own voice instead of
   * as a protocol payload.
   */
  const chosenPlan = (item: Extract<ThreadItem, { t: "tool" }>) => {
    const planId = item.args.plan_id;
    if (typeof planId !== "string") return null;
    const at = thread.findIndex((entry) => entry.id === item.id);
    for (let index = (at === -1 ? thread.length : at) - 1; index >= 0; index--) {
      const prev = thread[index];
      if (prev.t !== "tool" || prev.state !== "blocked" || !prev.blocked) continue;
      const ruling = splitRuling(prev.blocked);
      const plan = ruling.plans.find((entry) => entry.id === planId);
      if (plan) return { plan, ruling };
    }
    return null;
  };

  const renderItem = (item: ThreadItem, group?: { first: boolean; last: boolean }) => {
    if (item.t === "verdict") {
      const ruling = splitRuling(item.text);
      const warnKind = ruling.kind === "narrowing";
      const tone = warnKind
        ? { bg: "var(--warn-bg)", fg: "var(--warn)" }
        : { bg: "var(--danger-bg)", fg: "var(--danger)" };
      const caption =
        ruling.kind === "refusal" ? (
          <>{ruling.summary}</>
        ) : ruling.dimension === "trust" ? (
          <>
            This source is not trusted — if we read it, the chat becomes{" "}
            {termPill(
              ruling.toTrustRank !== undefined
                ? (trustChain[ruling.toTrustRank] ?? `rank ${ruling.toTrustRank}`)
                : "less trusted",
            )}
            .
          </>
        ) : ruling.dimension === "audience" ? (
          <>
            This data is confidential — if we read it, only{" "}
            {termPill(ruling.readers ? ruling.readers.join(", ") || "nobody" : "fewer readers")} sees the results.
          </>
        ) : (
          <>Not trusted and confidential — if we read it, {acceptMove(ruling)}.</>
        );
      return exchangeShell(
        group,
        (
            <>
              {appaWho()}
              <div className="text-[13.5px] leading-relaxed">
                <p className="m-0">
                  {caption}
                  {ruling.plans.length > 0 && (
                    <>
                      {" "}
                      <b>I have a plan for you:</b>
                    </>
                  )}
                </p>
                {ruling.plans.length > 0 && (
                  <ol className="m-0 mt-1 list-decimal pl-6">
                    {ruling.plans.map((plan) => planOption(plan, ruling))}
                  </ol>
                )}
                <details className="mt-2">
                  <summary className="cursor-pointer font-mono text-[10.5px] text-[var(--icon)]">
                    what the model was told
                  </summary>
                  <div
                    className="mt-1 overflow-x-auto rounded-md text-xs"
                    style={{ background: tone.bg, color: tone.fg }}
                  >
                    <div className="whitespace-pre-wrap break-words p-2">{ruling.summary}</div>
                    {ruling.detail && (
                      <pre className="m-0 max-h-40 overflow-auto p-2 font-mono text-[11px] leading-relaxed">
                        {ruling.detail}
                      </pre>
                    )}
                  </div>
                </details>
              </div>
            </>
        ),
        heldToolBefore(item.id),
      );
    }
    if (item.t === "user")
      return (
        <Message className="max-w-[85%]" from="user" key={item.id}>
          <MessageContent>{item.text}</MessageContent>
        </Message>
      );
    if (item.t === "text")
      return (
        <Message className="max-w-full" from="assistant" key={item.id}>
          <MessageContent>
            <MessageResponse>{item.text}</MessageResponse>
          </MessageContent>
        </Message>
      );
    if (item.t === "note")
      return (
        // Hanging indent: wrapped lines align under the text, not the glyph.
        <div key={item.id} className="-indent-4 pl-4 font-mono text-[11px] text-[var(--icon)]">
          ⎿ {item.text}
        </div>
      );
    if (item.t === "rule") return <div className="border-t border-dashed border-[var(--border-weak)]" key={item.id} />;
    if (item.t === "entry")
      return (
        <div className="my-1 flex w-full items-center gap-3" key={item.id}>
          <span className="h-px flex-1 bg-[var(--border-weak)]" />
          <span className="flex flex-wrap items-center gap-1.5 font-mono text-[10.5px] tracking-widest text-[var(--icon)] uppercase">
            session starts
            <span style={pill("var(--bg-weak)", "var(--text-weak)")}>trust: {item.label.trust}</span>
            <span style={pill("var(--bg-weak)", "var(--text-weak)")}>audience: {item.label.audience}</span>
          </span>
          <span className="h-px flex-1 bg-[var(--border-weak)]" />
        </div>
      );
    if (item.t === "authors")
      return (
        <div
          className="w-full rounded-md border border-[var(--accent-border)] bg-[var(--accent-bg)] p-4"
          key={item.id}
        >
          <div className="mb-1.5 flex items-center gap-2 font-mono text-[11px] tracking-[0.12em] text-[var(--accent)] uppercase">
            <appa-mark size={17} />
            author note
          </div>
          <p className="m-0 text-[13.5px] leading-relaxed text-[var(--text)]">{item.text}</p>
        </div>
      );
    if (item.t === "shift")
      return (
        <div
          className="my-1 flex w-full items-center gap-3"
          key={item.id}
          title={`was trust: ${item.from.trust} · audience: ${item.from.audience}`}
        >
          <span className="h-px flex-1 bg-[var(--warn)] opacity-40" />
          <span className="flex flex-wrap items-center gap-1.5 font-mono text-[10.5px] tracking-widest text-[var(--warn)] uppercase">
            new label
            <span style={pill("var(--warn-bg)", "var(--warn)")}>trust: {item.to.trust}</span>
            <span style={pill("var(--warn-bg)", "var(--warn)")}>audience: {item.to.audience}</span>
          </span>
          <span className="h-px flex-1 bg-[var(--warn)] opacity-40" />
        </div>
      );
    if (item.t === "approval") {
      const verdict =
        item.state === "approved"
          ? { text: "approved", bg: "var(--accent-bg)", fg: "var(--accent)" }
          : item.state === "denied"
            ? { text: "denied", bg: "var(--danger-bg)", fg: "var(--danger)" }
            : item.state === "expired"
              ? { text: "expired — abstained", bg: "var(--bg-weak-hover)", fg: "var(--icon)" }
              : null;
      return exchangeShell(
        group,
        (
          <>
          {appaWho(
            <span style={pill(verdict ? verdict.bg : "var(--warn)", verdict ? verdict.fg : "var(--warn-bg)")}>
              {verdict ? verdict.text : "waiting on you"}
            </span>,
          )}
          <div className="rounded-md border border-[var(--warn)] bg-[var(--warn-bg)] p-4">
          <p className="m-0 text-[13.5px] leading-relaxed text-[var(--text)]">
            We ask {item.authority ? entityPill(item.authority) : "a human"} to approve{" "}
            <span className="font-mono text-xs text-[var(--text-strong)]">{item.tool}</span>.
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
          </>
        ),
        item.tool,
      );
    }
    // The agent's remedy choice, in its own voice: "I choose to use X" says
    // more than a protocol payload. The wire call stays available underneath.
    if (item.name === "execute_remedy_plan") {
      const chosen = chosenPlan(item);
      const sentence = chosen ? (
        <>
          {chosen.plan.rulings.map((ruled, index) => (
            <span key={ruled.authority}>
              {index > 0 && " and "}I ask {entityPill(ruled.authority, ruled.hint)} for approval
            </span>
          ))}
          {chosen.plan.sanitize && (
            <>
              {chosen.plan.rulings.length > 0 && " and "}I choose to use{" "}
              {entityPill(chosen.plan.sanitize.sanitizer, chosen.plan.sanitize.hint)}
            </>
          )}
          {chosen.plan.accepts &&
            (chosen.plan.rulings.length > 0 || chosen.plan.sanitize ? (
              <> and accept what remains</>
            ) : (
              <>
                I accept{chosen.ruling.kind === "narrowing" ? <> — {acceptMove(chosen.ruling)}</> : <> the cost</>}
              </>
            ))}
          .
        </>
      ) : (
        <>I execute remedy plan {typeof item.args.plan_id === "string" ? item.args.plan_id : "…"}.</>
      );
      return exchangeShell(
        group,
        (
          <>
            {agentWho}
            <div className={`text-[14px] leading-relaxed ${item.state === "running" ? "animate-pulse" : ""}`}>
              {sentence}
            </div>
          </>
        ),
        heldToolBefore(item.id),
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
    // so the card wears verdict styling, never the error slot. The ruling's
    // own words are APPA's turn, a separate item in the thread.
    const verdict = ruling
      ? ruling.kind === "narrowing"
        ? { badge: rulingBadge(ruling), bg: "var(--warn-bg)", fg: "var(--warn)" }
        : { badge: rulingBadge(ruling), bg: "var(--danger-bg)", fg: "var(--danger)" }
      : null;
    return (
      <div className="w-full min-w-0" key={item.id}>
        {/* mb-0: the thread's rhythm is the row's own padding, not the card's. */}
        <Tool className="mb-0 w-full">
          <ToolHeader
            badge={
              // The held card keeps its block; the resolution card — the same
              // call, run again after its remedy — wears the outcome: the
              // mascot sweeps for a cleaned result, carries a check for an
              // accepted narrowing.
              item.sanitizedBy ? (
                <span className="inline-flex items-center gap-1.5" style={pill("var(--accent-bg)", "var(--accent)")}>
                  <appa-mark size={16} variant="clean" /> cleaned
                </span>
              ) : item.approvedBy ? (
                <span style={pill("var(--accent-bg)", "var(--accent)")}>approved by {item.approvedBy}</span>
              ) : item.echo ? (
                <span className="inline-flex items-center gap-1.5" style={pill("var(--warn-bg)", "var(--warn)")}>
                  <appa-mark size={16} variant="accept" /> accepted
                </span>
              ) : verdict ? (
                <span style={pill(verdict.bg, verdict.fg)}>
                  {ruling?.kind === "narrowing" ? `held · ${verdict.badge}` : verdict.badge}
                </span>
              ) : undefined
            }
            state={state}
            type={`tool-${item.name}` as `tool-${string}`}
          />
          <ToolContent>
            <ToolInput input={item.args} />
            <ToolOutput
              errorText={undefined}
              output={
                (item.state === "done" && item.output) || item.result || item.sanitizedBy || item.approvedBy ? (
                  <div className="p-2 font-mono text-xs">
                    {item.state === "done" && item.output && <div>⎿ {item.output}</div>}
                    {item.sanitizedBy && (
                      <div className="mt-1 flex flex-wrap items-center gap-1.5 font-sans text-[11px] text-[var(--text-weak)]">
                        cleaned with {entityPill(item.sanitizedBy)} — the assistant never saw the raw result, only
                        this:
                      </div>
                    )}
                    {item.approvedBy && (
                      <div className="mt-1 flex flex-wrap items-center gap-1.5 font-sans text-[11px] text-[var(--text-weak)]">
                        approved by {entityPill(item.approvedBy)}
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
          </ToolContent>
        </Tool>
      </div>
    );
  };

  // ---- the label rail ------------------------------------------------------

  // Loss relative to entry: green while the label still sits where the turn
  // entered, amber once the audience has narrowed, red once trust has dropped
  // (red wins). Boundary-relative, so it stays true under renamed ranks.
  const railTone = (lab?: LabelState) => {
    const entry = boundary;
    if (!lab || !entry) return "transparent";
    if (lab.trust !== entry.trust) return "var(--danger)";
    if (lab.audience !== entry.audience) return "var(--warn)";
    return "var(--accent-border)";
  };

  // One thread row: the rail segment carrying the label at that moment, then
  // the turn. A shift row is stamped with the label it moved to, so the rail
  // simply changes color through the separator — the separator itself is the
  // marker.
  const railRow = (key: number, content: React.ReactNode, lab?: LabelState, tight = false) => (
    <div className="grid grid-cols-[12px_minmax(0,1fr)] gap-x-2" key={key}>
      <span
        className="w-[5px] justify-self-start rounded-[1px]"
        style={{ background: railTone(lab) }}
        title={lab ? `trust: ${lab.trust} · audience: ${lab.audience}` : undefined}
      />
      <div className={`min-w-0 ${tight ? "pb-0" : "pb-4"}`}>{content}</div>
    </div>
  );

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
              <span className="text-[11px] whitespace-nowrap text-[var(--icon)]">Model</span>
              <span className="min-w-0 flex-1 text-right font-mono text-xs text-[var(--text-weak)]">
                {PLAYGROUND_MODEL.label}
              </span>
            </div>
          </div>
    </>
  );

  return (
    <div className="flex h-full min-h-0 flex-col bg-[var(--bg)]">
      {/* The sidebar is sized so the preset's longest contract line fits the
          editor unwrapped at its 13px mono; the chat pane keeps a hard floor,
          so on narrower screens the sidebar cedes room and accepts a wrap.
          Dragging the pane edge overrides the defaults via --sidebar-w. */}
      <div
        className="grid min-h-0 flex-1 grid-cols-1 lg:grid-cols-[minmax(26rem,1fr)_minmax(0,var(--sidebar-w,34rem))] xl:grid-cols-[minmax(30rem,1fr)_minmax(0,var(--sidebar-w,42rem))]"
        ref={gridRef}
        style={sidebarPx !== null ? ({ "--sidebar-w": `${sidebarPx}px` } as React.CSSProperties) : undefined}
      >
        {/* ---- chat pane ---- */}
        <div className="flex min-h-0 min-w-0 flex-col border-b border-[var(--border-weak)] bg-[var(--bg)] lg:border-r lg:border-b-0">
          {/* One strip of chrome: the session label heads the chat it governs
              (the flight from a card lands here), with the few controls the
              playground needs on the right. */}
          <div className="flex flex-wrap items-center gap-2 border-b border-[var(--border-weak)] px-4 py-1.5">
            <span className="font-mono text-[10px] tracking-widest text-[var(--icon)] uppercase">session label</span>
            <span style={{ display: "flex", gap: "0.4rem" }}>
              {label && <LabelPills boundary={boundary} label={label} />}
            </span>
            <span className="ml-auto flex flex-wrap items-center gap-2">
              {mode === "probing" && <span style={pill("var(--bg-weak-hover)", "var(--icon)")}>connecting…</span>}
              {mode === "down" && <span style={pill("var(--danger-bg)", "var(--danger)")}>service unavailable</span>}
              <button type="button" onClick={resetChat} className="lp-replay" style={chrome.barBtn}>
                New chat
              </button>
              <button className="lg:hidden" onClick={() => setPanelOpen(true)} style={chrome.barBtn} type="button">
                Tools · policy
              </button>
            </span>
          </div>
          <Conversation className="min-h-0">
            <ConversationContent className="gap-0">
              {thread.length === 0 ? (
                // An empty live chat offers starters that walk the demo's
                // best paths; a service problem earns a message instead.
                mode === "live" ? (
                  // Column count follows the pane (container), not the
                  // viewport — the sidebar leaves the chat far narrower than
                  // the breakpoint suggests. Safe centering: when the cards
                  // outgrow the pane, align to the scrollable top instead of
                  // clipping both ends.
                  <div className="@container flex h-full flex-col items-center gap-4 px-4 py-12 [justify-content:safe_center]">
                    <span className="font-mono text-[11px] tracking-widest text-[var(--icon)] uppercase">
                      start with
                    </span>
                    <div className="@xl:grid-cols-2 @4xl:grid-cols-3 grid w-full max-w-[64rem] grid-cols-1 gap-3">
                      {STARTER_PROMPTS.map((starter) => (
                        <button
                          className="group flex cursor-pointer flex-col gap-2.5 rounded-xl border border-[var(--border-weak)] bg-[var(--bg-weak)] p-5 text-left transition-colors hover:border-[var(--accent-border)] hover:bg-[var(--accent-bg)]"
                          key={starter.tag}
                          onClick={() => send(starter.text)}
                          type="button"
                        >
                          <span className="flex items-center justify-between font-mono text-[10.5px] tracking-widest text-[var(--icon)] uppercase group-hover:text-[var(--accent)]">
                            {starter.tag}
                            <span aria-hidden className="transition-transform group-hover:translate-x-0.5">→</span>
                          </span>
                          <span className="text-[14.5px] leading-normal text-[var(--text-strong)]">
                            {starter.text}
                          </span>
                        </button>
                      ))}
                    </div>
                  </div>
                ) : (
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
                <>
                {segmentThread(thread, childIds).map((segment) =>
                  segment.child
                    ? railRow(
                        segment.items[0].id,
                        <details className="w-full min-w-0 rounded-md border border-dashed border-[var(--border-weak)]">
                          <summary className="cursor-pointer px-3 py-2 font-mono text-[11px] text-[var(--text-weak)] hover:text-[var(--text-strong)]">
                            ⑂ child {segment.child} — what it reads narrows this branch only ·{" "}
                            {segment.items.length} steps
                          </summary>
                          <div className="mb-2 ml-3 flex flex-col gap-3 border-l-2 border-[var(--warn)] py-1 pr-2 pl-3">
                            {segment.items.map((entry) => renderItem(entry))}
                          </div>
                        </details>,
                        segment.items[0].lab,
                      )
                    : segment.items.map((item, index) => {
                        // Consecutive exchange turns share one grey box.
                        const group = isExchange(item)
                          ? {
                              first: index === 0 || !isExchange(segment.items[index - 1]),
                              last:
                                index === segment.items.length - 1 || !isExchange(segment.items[index + 1]),
                            }
                          : undefined;
                        return railRow(item.id, renderItem(item, group), item.lab, Boolean(group && !group.last));
                      }),
                )}
                {/* Between the model's moves nothing on screen is alive —
                    running cards pulse, pending approvals wear a badge, and
                    this row covers the remaining silence. */}
                {busy &&
                  !thread.some((item) => item.t === "tool" && item.state === "running") &&
                  !thread.some((item) => item.t === "approval" && item.state === "pending") &&
                  railRow(
                    -1,
                    <div className="animate-pulse font-mono text-[11px] text-[var(--icon)]">⎿ thinking…</div>,
                    labelRef.current ?? undefined,
                  )}
                </>
              )}
            </ConversationContent>
            <ConversationScrollButton />
          </Conversation>

          <div className="flex flex-col gap-2 border-t border-[var(--border-weak)] px-4 py-3">
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
                      : "Message the assistant…"
                }
                value={input}
              />
              <PromptInputSubmit
                className="absolute right-1 bottom-1"
                disabled={!busy && (!live || !input.trim())}
                onStop={stop}
                status={busy ? "streaming" : "ready"}
              />
            </PromptInput>
          </div>
        </div>

        {/* ---- policy pane: resizable sidebar on desktop ---- */}
        <div className="relative hidden min-h-0 min-w-0 flex-col bg-[var(--bg)] lg:flex">
          <div
            aria-orientation="vertical"
            className="absolute inset-y-0 -left-1 z-10 w-2 cursor-col-resize transition-colors hover:bg-[var(--accent-border)] active:bg-[var(--accent-border)]"
            onDoubleClick={() => {
              setSidebarPx(null);
              localStorage.removeItem("appa-demo-sidebar-px");
            }}
            onPointerDown={startSidebarDrag}
            role="separator"
            title="Drag to resize · double-click to reset"
          />
          {policyPane}
        </div>
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

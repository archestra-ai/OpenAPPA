"use client";

// The /playground chat: demo-card chrome around a real chat UI (AI Elements),
// driven by the appa-demo service — the visitor's own OpenRouter key, the
// policy shown beside the chat actually enforced, any prompt.
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
import { Message, MessageContent } from "@/components/ai-elements/message";
import {
  PromptInput,
  type PromptInputMessage,
  PromptInputSubmit,
  PromptInputTextarea,
} from "@/components/ai-elements/prompt-input";

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
import { type LabelState, PLAYGROUND_MODEL } from "./playground-data";
import { NEW_CHAT_EVENT } from "@/components/DocsSidebar";
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
      /** Set when the engine refused this protocol call outright. */
      refused?: boolean;
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
  | { id: number; t: "verdict"; text: string; traj?: string; tool?: string }
  /** The root label moved: a marker row that recolors the rail below it. */
  | { id: number; t: "shift"; from: LabelState; to: LabelState; cause?: string }
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
    outcome: "Confidential meeting notes are never published to a GitHub issue.",
    // The mechanism the run turns on, named before it starts — so a visitor
    // choosing between the scenarios is choosing between behaviors, not
    // between three sentences that all sound like "nothing bad happens".
    note: "Will use sanitizers",
    text: "Check the recent meeting recordings for bugs customers mentioned, and file any that are not on GitHub yet.",
    expectation: "In this example, OpenAPPA makes sure that confidential meeting notes never end up in a GitHub issue.",
  },
  {
    outcome: "A wire transfer happens only after a human approves it.",
    note: "Human in the loop",
    text: "Check the open invoices and pay the overdue one by transfer.",
    expectation:
      "In this example, OpenAPPA makes sure that the invoice data is not leaked, and that the wire transfer happens only after a human approves it.",
  },
  {
    outcome: "Financial data reaches only the people allowed to read it.",
    note: "Audiences prevent data leak",
    text: "Review the unpaid invoices and email a summary first to ap-review@corp.example. After that succeeds, send the same summary to all@acme.com.",
    expectation:
      "In this example, OpenAPPA makes sure that the invoice summary reaches only the people allowed to read it — ap-review@corp.example, but not all@acme.com. Yes, that means it refuses a malicious user prompt too.",
  },
];

/**
 * The harness's own tools (`appa_runtime::tool`). Feedback on these is
 * protocol dialogue — an acknowledgment, a cost statement, a stale-offer
 * notice — never a policy ruling on a flow; rulings land on the blocked
 * tool's own card. So their cards close calmly instead of styling as errors.
 */
/* Every reference into the policy wears the same clothes: mono, a dotted
   underline that lifts to the accent on hover. Pills boxed each mention and
   broke the line's rhythm wherever a sentence carried two of them. */
const REFERENCE_CLASS =
  "cursor-pointer font-mono text-[12.5px] text-[var(--text-strong)] underline decoration-dotted decoration-[var(--border)] underline-offset-4 hover:text-[var(--accent)] hover:decoration-[var(--accent)]";

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

/** Drop the protocol instruction the runtime appends for the model. */
function readable(summary: string): string {
  return summary
    .replace(/[;,]?\s*execute (?:one offered plan|plan [^\s]+) with execute_remedy_plan\.?/gi, "")
    .replace(/\s+/g, " ")
    .trim();
}

/**
 * The tools the playground's own stories touch. The CRM pair is registered and
 * never called by a scenario, so the short view leaves it out; everything else
 * appears in a ruling, a remedy plan or a pill you can click.
 */
const STORY_TOOLS = new Set(["list_recordings", "list_issues", "create_issue", "list_invoices", "make_transfer", "send_email"]);

/**
 * Sections the short view leaves out. The session's opening label, the
 * annotator behind an address, and how an authority or sanitizer is actually
 * implemented are all real policy — and none of them is what a reader is
 * pointed at from the stream, so the pane does not carry them.
 */
const SHORT_DROP = new Set([
  "[boundary]",
  "[[annotator]]",
  "[authority.permits]",
  "[authority.implementation]",
  "[sanitizer.implementation]",
]);

/**
 * The policy reduced to what the playground's stories point at: the six tools
 * the scenarios call, the authority that signs a transfer, and the two
 * sanitizers with the transitions they are allowed to make. The section
 * headings stay so the rules keep their shape; the explanatory prose does not,
 * including the instruction a sanitizer's transform runs under.
 *
 * Derived from the policy the service ships rather than kept as a second copy,
 * so it follows that policy instead of drifting from it.
 */
function shortPolicy(full: string): string {
  // A comment belongs to the section it introduces, so it travels with the
  // block below it — a dropped section takes its own heading with it.
  const groups: { head: string; lead: string[]; body: string[] }[] = [];
  let lead: string[] = [];
  for (const line of full.split("\n")) {
    if (line.startsWith("[")) {
      groups.push({ head: line.trim(), lead, body: [line] });
      lead = [];
      continue;
    }
    if (line.trimStart().startsWith("#")) lead.push(line);
    else if (line.trim() !== "") groups[groups.length - 1]?.body.push(line);
    else if (lead.length > 0) lead.push(line);
  }
  const kept = groups.filter((group) => {
    if (SHORT_DROP.has(group.head)) return false;
    if (group.head !== "[[tool]]") return true;
    const name = group.body.find((line) => /^\s*name\s*=/.test(line));
    return STORY_TOOLS.has(name?.split('"')[1] ?? "");
  });
  return kept
    .flatMap((group) => [
      "",
      // Only the section rules survive: `# --- GitHub … ---` says where you
      // are, where a paragraph of explanation would undo the shortening.
      ...group.lead.filter((line) => line.startsWith("# ---")).flatMap((line) => [line, ""]),
      // A sanitizer's `hint` is the paragraph its transform runs under — the
      // longest line in the policy, and prose rather than rule. The transition
      // beneath it is what a reader is sent here to see, so the hint goes.
      ...(group.head === "[[sanitizer]]" ? group.body.filter((line) => !/^\s*hint\s*=/.test(line)) : group.body),
    ])
    .join("\n")
    .replace(/\n{3,}/g, "\n\n")
    .trim();
}

/**
 * The runtime's refusal for an unoffered `plan_id` is written for the model:
 * it leads with the fork case, because that is the one a model reads its way
 * into, and closes by telling it how to re-propose. A reader watching the
 * stream needs neither — in this playground the agent simply answered one
 * ruling twice, and the second answer arrived after the first had settled it.
 */
function refusal(output: string | undefined, offerKnown: boolean): string {
  const text = readable(output ?? "");
  if (/no pending blocked call offers that plan_id/i.test(text)) {
    return offerKnown
      ? "That ruling was already settled, so its plans are no longer on offer."
      : "No ruling in this chat is offering that plan.";
  }
  return text || "The engine refused this plan.";
}

/**
 * The model answers in markdown out of habit. Its bold labels, code chips and
 * heading rules turn a two-line answer into a formatted document, which reads
 * as noise beside a stream that has no formatting of its own. Strip the
 * syntax, keep the words and the line breaks.
 */
function asProse(text: string): string {
  return text
    .replace(/```[a-z]*\n?/gi, "")
    .replace(/^#{1,6}\s+/gm, "")
    .replace(/^\s*>\s?/gm, "")
    .replace(/^(\s*)[-*+]\s+/gm, "$1· ")
    .replace(/\[([^\]]+)\]\([^)]*\)/g, "$1")
    .replace(/(\*\*|__)(.+?)\1/g, "$2")
    .replace(/(^|[\s(])[*_]([^*_\n]+)[*_](?=[\s).,;:!?]|$)/g, "$1$2")
    .replace(/`([^`\n]+)`/g, "$1")
    .replace(/^\s*([-*_]\s*){3,}$/gm, "")
    .replace(/\n{3,}/g, "\n\n")
    .trim();
}

function splitRuling(text: string): Ruling {
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
 * The policy's trust ranks, least-trusted first, read from the policy's own
 * text — a narrowing names its target trust as a chain index, and this is what
 * gives that index its name. The loader validates the real
 * parse; this regex only has to agree with it on the happy path.
 */
function parseTrustChain(policy: string): string[] {
  const match = policy.match(/trust_chain\s*=\s*\[([^\]]*)\]/);
  const names = match?.[1].match(/"([^"]*)"/g)?.map((quoted) => quoted.slice(1, -1));
  return names?.length ? names : ["suspicious", "trusted"];
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
  const [busy, setBusy] = useState(false);
  const [input, setInput] = useState("");
  const [turns, setTurns] = useState(0);

  // Mobile only: the pane lives in a right-side drawer, closed by default.
  const [panelOpen, setPanelOpen] = useState(false);
  /* Which call rows have their arguments open. A whole email body inline
     drowns the line it belongs to, but the arguments are still the thing a
     flow decision turns on — so they collapse rather than disappear. */
  const [openArgs, setOpenArgs] = useState<ReadonlySet<number>>(new Set());
  /* The first screen is a choice, not a prompt: the composer only appears
     once there is a conversation, or when a visitor asks for one. */
  const [composerAsked, setComposerAsked] = useState(false);
  // Desktop only: the policy pane's width once the visitor has dragged its
  // edge; null leaves the breakpoint defaults in charge. Remembered locally.
  const [sidebarPx, setSidebarPx] = useState<number | null>(null);
  const gridRef = useRef<HTMLDivElement>(null);
  // The contract or authority the engine is acting on right now, lit in the
  // editor. A focus holds until the next acting block replaces it; when the
  // turn settles (`settled`), it lingers briefly and fades.
  const [focus, setFocus] = useState<{
    name: string;
    at: number;
    settled?: boolean;
  } | null>(null);
  // Which systems exist, so which tools the agent has. Fixed by the preset:
  // every system the service reports is reachable.
  const [systems, setSystems] = useState<string[]>([]);
  const [policyText, setPolicyText] = useState("");
  const [policyStatus, setPolicyStatus] = useState<{
    ok: boolean;
    text: string;
  } | null>(null);

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
      setSystems(preset.systems.map((system) => system.id));
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

  // Load the preset policy through the real loader: it reports the boundary the
  // first turn enters at, and anything the policy leaves unconstrained.
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
            // With no session running the label simply *is* the boundary.
            // Once a session exists its live label owns it until New chat.
            if (!sessionRef.current) {
              labelRef.current = result.boundary;
            }
          }
          // Silence on success: a policy that loads has nothing to announce.
          // Warnings and failures still speak.
          const notes: string[] = [];
          if (result.unconstrained?.length) notes.push(`${result.unconstrained.length} unconstrained`);
          if (result.ignored?.length) notes.push(`ignoring ${result.ignored.join(", ")} — system off`);
          setPolicyStatus(notes.length ? { ok: true, text: notes.join(" · ") } : null);
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
  const resolveHeld = (
    prev: ThreadItem[],
    patch: Partial<Extract<ThreadItem, { t: "tool" }>>,
    sanitizer?: string,
  ): ThreadItem[] => {
    /* Which held call this outcome belongs to. The wire carries no call id on
       a result, and the agent can have several calls held at once — taking
       "the last held card" then routed every outcome to the same call, so one
       resolution card was overwritten and the other never appeared.

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
    // card: the outcome batch opens that card, and this event patches it.
    let heldAt = sanitizer ? prev.findIndex((item) => isHeld(item) && offers(item)) : -1;
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
    labelRef.current = boundary;
    setBusy(false);
    setTurns(0);
    setInput("");
    setComposerAsked(false);
  }, [boundary]);

  /* The nav's Playground link cannot navigate when you are already here, so it
     asks for a fresh session instead. An event keeps the reset where it lives
     — in this component — rather than threading it up through the layout. */
  useEffect(() => {
    const onNewChat = () => resetChat();
    window.addEventListener(NEW_CHAT_EVENT, onNewChat);
    return () => window.removeEventListener(NEW_CHAT_EVENT, onNewChat);
  }, [resetChat]);

  // ---- live path ----------------------------------------------------------

  const applyEvent = useCallback((event: DemoEvent) => {
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
          lab: labelRef.current ?? undefined,
        };
        setThread((prev) => {
          /* A remedy answers one ruling, so it belongs under that ruling. Plan
             ids are unique across a batch, so the offer identifies its block
             exactly — appending instead filed every remedy under whichever
             ruling happened to be last. */
          const planId = event.tool === "execute_remedy_plan" ? event.arguments.plan_id : undefined;
          if (typeof planId === "string") {
            const verdictAt = prev.findIndex(
              (entry) => entry.t === "verdict" && splitRuling(entry.text).plans.some((plan) => plan.id === planId),
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
        // fork) is an outcome, not a ruling, so it closes the card calmly
        // instead of wearing the error styling.
        setThread((prev) => {
          const index = prev.findIndex(
            (item) => item.t === "tool" && item.callId === event.call_id && item.state === "running",
          );
          if (index === -1) return prev;
          const item = prev[index] as Extract<ThreadItem, { t: "tool" }>;
          // A block on a protocol call is the engine refusing the agent's own
          // move — the card has to say so, or a rejected remedy reads exactly
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
          // The card records the hold; the ruling itself is APPA's own turn.
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
            id: ++idRef.current,
            t: "verdict",
            text: event.text,
            tool: item.name,
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
        const from = labelRef.current;
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
             the resolution cards it paid for.

             Scoped to those cards on purpose: when a narrowing is not
             accepted but simply admitted with the result, the label really
             does move after the call, and that order is left alone. */
          setThread((prev) => {
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
        labelRef.current = next;
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
        push({
          id: nextId(),
          t: "note",
          text: event.text,
          traj: event.trajectory,
        });
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
      const starter = STARTER_PROMPTS.find((prompt) => prompt.text === text);
      if (opening && starter) push({ id: nextId(), t: "authors", text: starter.expectation });
      if (opening && entryLabel) push({ id: nextId(), t: "entry", label: entryLabel });
      push({ id: nextId(), t: "user", text });
      setTurns((count) => count + 1);

      try {
        if (!sessionRef.current) {
          const info = await createSession(policyText, systems, PLAYGROUND_MODEL.id);
          if (runRef.current !== run) return;
          sessionRef.current = info.session;
          labelRef.current = { trust: info.trust, audience: info.audience };
          if (opening && !entryLabel) {
            // The visitor beat the policy check's debounce: open the stream
            // with the label the session actually entered under.
            const label = { trust: info.trust, audience: info.audience };
            setThread((prev) => {
              const at = prev.findIndex((item) => item.t === "user");
              const item: ThreadItem = {
                id: ++idRef.current,
                t: "entry",
                label,
                lab: label,
              };
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
  const shortText = useMemo(() => shortPolicy(policyText), [policyText]);
  // Line numbers are the short text's own: it is the only policy on screen.
  const highlight = useMemo(() => (focus ? findBlock(shortText, focus.name) : null), [focus, shortText]);

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
    setFocus({ name, at: Date.now() });
  };

  const entityPill = (name: string, hint?: string) =>
    reference(
      name,
      name,
      REFERENCE_CLASS,
      hint ? `“${hint}” — click to see it in the policy` : "Click to see it in the policy",
    );

  /* References are words inside a sentence, so they must wrap like words. A
     <button> cannot: browsers treat form controls as atomic inline-level
     boxes and force `inline-block`, which is why a long reader set leapt to
     the next line whole. A span with the button role wraps naturally and
     keeps click and keyboard activation. */
  const reference = (text: string, target: string, className: string, title: string) => (
    <span
      className={className}
      onClick={() => focusEntity(target)}
      onKeyDown={(event) => {
        if (event.key === "Enter" || event.key === " ") {
          event.preventDefault();
          focusEntity(target);
        }
      }}
      role="button"
      tabIndex={0}
      title={title}
    >
      {text}
    </span>
  );

  /** Inline reference at the host line's own size — for label rows. */
  const policyRef = (text: string, target: string) =>
    reference(
      text,
      target,
      "cursor-pointer underline decoration-dotted decoration-[var(--border)] underline-offset-4 hover:text-[var(--accent)] hover:decoration-[var(--accent)]",
      `Click to see ${target} in the policy`,
    );

  /** A value whose *cause* lives in the policy: reads as prose, opens the rule. */
  const policyPill = (text: string, target: string) =>
    reference(text, target, REFERENCE_CLASS, `Click to see the ${target} rule that set this in the policy`);

  // A label value wears the glossary: the same definition the docs popovers
  // use, as a plain tooltip.
  const termPill = (text: string) => (
    <span
      className="font-mono text-[12.5px] text-[var(--text-strong)] underline decoration-dotted decoration-[var(--border)] underline-offset-4"
      style={termDefinition(text) ? { cursor: "help" } : undefined}
      title={termDefinition(text)}
    >
      {text}
    </span>
  );

  /** What the narrowing costs, named once so nothing has to repeat it. */
  const narrowedTrust = (ruling: Extract<Ruling, { kind: "narrowing" }>) =>
    ruling.toTrustRank !== undefined ? (trustChain[ruling.toTrustRank] ?? `rank ${ruling.toTrustRank}`) : null;

  /** The readers left, when the engine resolved them to a concrete set. */
  const narrowedReaders = (ruling: Extract<Ruling, { kind: "narrowing" }>) =>
    ruling.readers ? ruling.readers.join(", ") || "nobody" : null;

  /** The dimension a narrowing lands on, for sentences that name it. */
  const narrowedDimension = (ruling: Extract<Ruling, { kind: "narrowing" }>) =>
    ruling.dimension === "trust" ? "trust" : ruling.dimension === "audience" ? "audience" : "label";

  /** "the audience narrowing" / "the trust narrowing" / "the narrowing". */
  const narrowingNoun = (ruling: Extract<Ruling, { kind: "narrowing" }>) =>
    ruling.dimension === "both" ? "the narrowing" : `the ${ruling.dimension} narrowing`;

  /**
   * What accepting does to this chat, as one clause. Says what changes and
   * for how long — a reader who has never met an information-flow label
   * should still understand the price.
   */
  const acceptMove = (ruling: Extract<Ruling, { kind: "narrowing" }>) => {
    const moves: React.ReactNode[] = [];
    if (ruling.dimension !== "audience") {
      const to = narrowedTrust(ruling);
      moves.push(<span key="trust">lowers this chat&apos;s trust{to ? <> to {termPill(to)}</> : null}</span>);
    }
    if (ruling.dimension !== "trust") {
      const to = narrowedReaders(ruling);
      moves.push(<span key="audience">narrows this chat&apos;s audience{to ? <> to {termPill(to)}</> : null}</span>);
    }
    return moves.map((move, index) => (
      <span key={index}>
        {index > 0 && " and "}
        {move}
      </span>
    ));
  };

  /**
   * One plan as one option. The price is already stated in the sentence
   * above, so an option says what it *does* rather than restating the cost —
   * the repetition was most of what made these cards hard to read.
   */
  const planOption = (plan: ParsedPlan, ruling: Ruling) => {
    const clauses: React.ReactNode[] = [];
    for (const ruled of plan.rulings)
      clauses.push(<>ask {entityPill(ruled.authority, ruled.hint)} to approve this one call</>);
    if (plan.sanitize)
      clauses.push(
        <>
          clean it first with {entityPill(plan.sanitize.sanitizer, plan.sanitize.hint)}
          {ruling.kind === "narrowing" ? <>, and this chat&apos;s {narrowedDimension(ruling)} stays as it is</> : null}
        </>,
      );
    if (plan.accepts || clauses.length === 0)
      clauses.push(ruling.kind === "narrowing" ? <>accept {narrowingNoun(ruling)}</> : <>accept</>);
    return (
      <li className="my-1" key={plan.id}>
        {clauses.map((clause, index) => (
          <span key={index}>
            {index > 0 && ", then "}
            {clause}
          </span>
        ))}
      </li>
    );
  };

  /* Who is speaking: a name and a colon. The label shares a baseline with the
     line it opens, which is correct and still reads low — at 11px its ink sits
     0.9px below the optical centre of the 13.5px sentence beside it (measured,
     not guessed). Lifting it by that much centres the two. Only the agent's
     rows showed it: APPA's neighbour is the same mono face, so the identical
     offset goes unseen there. */
  const speaker = (who: "appa" | "agent", badge?: React.ReactNode) => (
    <span className="relative top-[-0.9px] mr-2 font-mono text-[11px] font-semibold text-[var(--text-strong)]">
      {who === "appa" ? "OpenAPPA:" : "Agent:"}
      {badge ? <span className="ml-2">{badge}</span> : null}
    </span>
  );

  const appaWho = (badge?: React.ReactNode) => speaker("appa", badge);

  /**
   * One grey container for the APPA ↔ agent exchange. Consecutive exchange
   * turns merge into a single box: rounding and top padding only at the run's
   * edges, and the rows between them close their gap. The box opens by naming
   * the one tool call under negotiation — the exchange never widens beyond it.
   */
  const exchangeShell = (group: { first: boolean; last: boolean } | undefined, children: React.ReactNode) => {
    const edges = group ?? { first: true, last: true };
    /* The negotiation hangs off the call that caused it as a short sub-spine
       — no panel, no background, and no header: the call line above already
       names the tool and says it is paused, and repeating that was the
       loudest redundancy in the old stream. */
    return <div className={`border-l-2 border-[var(--warn-bg)] py-2 pl-4 ${edges.last ? "pb-3" : ""}`}>{children}</div>;
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
      // The story in one sentence: what this call returns, what reading it
      // would cost, and how long the cost lasts. The tool is named so the
      // sentence stands on its own when read out of the stream.
      const tool = item.tool;
      const subject = tool ? entityPill(tool) : <>This call</>;
      const readers = ruling.kind === "narrowing" ? narrowedReaders(ruling) : null;
      const rank = ruling.kind === "narrowing" ? narrowedTrust(ruling) : null;
      const caption =
        ruling.kind === "refusal" ? (
          <>
            {subject} — {readable(ruling.summary)}
          </>
        ) : ruling.dimension === "trust" ? (
          <>
            {subject} returns content the policy does not trust. Reading it lowers this chat&apos;s trust
            {rank ? <> to {tool ? policyPill(rank, tool) : termPill(rank)}</> : null}.
          </>
        ) : ruling.dimension === "audience" ? (
          <>
            {subject} returns confidential data. Reading it narrows this chat&apos;s audience
            {readers ? <> to {tool ? policyPill(readers, tool) : termPill(readers)}</> : null}.
          </>
        ) : (
          <>
            {subject} returns confidential data the policy does not trust. Reading it {acceptMove(ruling)}.
          </>
        );
      return exchangeShell(
        group,
        <>
          <div className="text-[13.5px] leading-relaxed">
            <p className="m-0">
              {appaWho()}
              {caption}
            </p>
            {ruling.plans.length > 0 && (
              <>
                <p className="m-0 mt-2 text-[var(--text)]">Remedy plan options:</p>
                <ol className="m-0 mt-0.5 list-decimal pl-5">{ruling.plans.map((plan) => planOption(plan, ruling))}</ol>
              </>
            )}
          </div>
        </>,
      );
    }
    if (item.t === "user")
      return (
        /* `ph-mask` keeps whatever the reader typed out of session replay:
           this bubble is their own text rendered back as page content, which
           input masking does not cover. */
        <Message className="max-w-[85%] ph-mask" from="user" key={item.id}>
          <MessageContent>{item.text}</MessageContent>
        </Message>
      );
    if (item.t === "text")
      return (
        <div className="text-[13.5px] leading-relaxed whitespace-pre-wrap text-[var(--text)]" key={item.id}>
          {asProse(item.text)}
        </div>
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
        <div key={item.id}>
          <div className="font-mono text-[11px] text-[var(--icon)]">
            Session label · Trust: {policyRef(item.label.trust, "boundary")} · Audience:{" "}
            {policyRef(item.label.audience, "boundary")}
          </div>
        </div>
      );
    if (item.t === "authors")
      return (
        <div className="mx-auto max-w-[34rem] px-4 py-6 text-center" key={item.id}>
          <span className="mx-auto mb-3 block h-px w-8 bg-[var(--accent-border)]" />
          <p className="m-0 text-[14.5px] leading-[1.75] text-balance text-[var(--text-weak)]">{item.text}</p>
        </div>
      );
    if (item.t === "shift")
      return (
        <div
          className="font-mono text-[11px]"
          key={item.id}
          style={{
            color: item.to.trust !== item.from.trust ? "var(--danger)" : "var(--warn)",
          }}
          title={`was trust: ${item.from.trust} · audience: ${item.from.audience}`}
        >
          Label narrows · Trust: {policyRef(item.to.trust, item.cause ?? "boundary")} · Audience:{" "}
          {policyRef(item.to.audience, item.cause ?? "boundary")}
        </div>
      );
    if (item.t === "approval") {
      /* An APPA turn like any other: a speaker, a sentence, and — while it is
         open — the two rulings you can give. The bordered warn panel was the
         only box left in the stream, and its saturated fills shouted louder
         than anything the engine actually says. */
      const verdict =
        item.state === "approved"
          ? { text: "approved", tone: "var(--accent)" }
          : item.state === "denied"
            ? { text: "denied", tone: "var(--danger)" }
            : item.state === "expired"
              ? { text: "expired — nobody answered", tone: "var(--icon)" }
              : { text: "waiting on you", tone: "var(--warn)" };
      const ruleBtn = (label: string, tone: string, border: string, approve: boolean) => (
        <button
          /* The one control a run demands mid-stream. A 28px target is fine
             for a pointer and too small for a thumb, so it grows to the 44px
             touch minimum below `lg` and keeps its desktop size above. */
          className="min-h-11 cursor-pointer rounded border px-4 py-2 font-mono text-[11px] transition-colors lg:min-h-0 lg:px-2.5 lg:py-1"
          onClick={() => void answerApproval(item.approvalId, approve)}
          style={{ color: tone, borderColor: border }}
          type="button"
        >
          {label}
        </button>
      );
      return exchangeShell(
        group,
        <div className="text-[13.5px] leading-relaxed">
          <p className="m-0">
            {speaker(
              "appa",
              <span className="font-mono text-[11px] font-normal" style={{ color: verdict.tone }}>
                {verdict.text}
              </span>,
            )}
            We ask {item.authority ? entityPill(item.authority) : "a human"} to approve {entityPill(item.tool)}.
          </p>
          {item.state === "pending" && (
            <div className="mt-2 flex gap-2">
              {ruleBtn("Approve", "var(--accent)", "var(--accent-border)", true)}
              {ruleBtn("Deny", "var(--danger)", "var(--danger)", false)}
            </div>
          )}
        </div>,
      );
    }
    // The agent's remedy choice, in its own voice: "I choose to use X" says
    // more than a protocol payload. The wire call stays available underneath.
    if (item.name === "execute_remedy_plan") {
      const chosen = chosenPlan(item);
      // The agent's answer, short: APPA has just stated the price, so the
      // reply names the choice and stops.
      const sentence = chosen ? (
        <>
          {chosen.plan.rulings.map((ruled, index) => (
            <span key={ruled.authority}>
              {index > 0 && " and "}Asking {entityPill(ruled.authority, ruled.hint)} to approve this call
            </span>
          ))}
          {chosen.plan.sanitize && (
            <>
              {chosen.plan.rulings.length > 0 && ", then "}
              {chosen.plan.rulings.length > 0 ? "cleaning" : "Cleaning"} it first with{" "}
              {entityPill(chosen.plan.sanitize.sanitizer, chosen.plan.sanitize.hint)}
            </>
          )}
          {chosen.plan.accepts &&
            (chosen.plan.rulings.length > 0 || chosen.plan.sanitize ? (
              <> and accepting what remains</>
            ) : (
              <>Accepting {chosen.ruling.kind === "narrowing" ? narrowingNoun(chosen.ruling) : "the ruling"}</>
            ))}
          .
        </>
      ) : (
        <>Taking remedy plan {typeof item.args.plan_id === "string" ? item.args.plan_id : "…"}.</>
      );
      return exchangeShell(
        group,
        <div className={`text-[13.5px] leading-relaxed ${item.state === "running" ? "animate-pulse" : ""}`}>
          <div>
            {speaker("agent")}
            {sentence}
          </div>
          {/* The engine can refuse the move itself — an offer belongs to the
                block that made it, and that block may already be settled.
                Without this the card reads as though the plan was taken. */}
          {item.refused && (
            <div className="mt-1 text-[12.5px] text-[var(--warn)]">
              {speaker("appa")}
              {refusal(item.output, Boolean(chosen))}
            </div>
          )}
        </div>,
      );
    }
    const ruling = item.state === "blocked" && item.blocked ? splitRuling(item.blocked) : null;
    /* A tool call is a line on the spine, not a card. The arrow carries the
       direction — out to the world, or a result coming back — which is what
       the wrench never said; the name is mono because it is an identifier;
       and the one status that matters sits at the end of the line. What was
       sent and what came back live behind a single disclosure: this demo is
       about the decision, not the payload. */
    const outbound = item.state !== "done";
    /* The arguments are the call: which recipient, which invoice. Hiding them
       behind a disclosure hid the very thing a flow decision turns on. */
    const args = Object.entries(item.args ?? {}).filter(([, value]) => value !== undefined && value !== "");
    const argsOpen = openArgs.has(item.id);
    /* The sanitizer and the authority named here are registered in the policy
       like any other entity, so they open it too — `policyRef` keeps the
       status line's own size and colour and adds only the affordance. */
    const status = item.sanitizedBy ? (
      <span className="inline-flex items-center gap-1.5 text-[var(--accent)]">
        <appa-mark size={14} variant="clean" /> cleaned by {policyRef(item.sanitizedBy, item.sanitizedBy)}
      </span>
    ) : item.approvedBy ? (
      <span className="text-[var(--accent)]">approved by {policyRef(item.approvedBy, item.approvedBy)}</span>
    ) : item.state === "running" ? (
      <span className="animate-pulse text-[var(--icon)]">running…</span>
    ) : null;
    return (
      <div className="w-full min-w-0" key={item.id}>
        <div className="flex items-baseline gap-x-3">
          {/* Arrow, name and arguments are one unit: the arrow used to be its
              own flex item, so a long signature wrapped away and left it
              stranded on the line above. */}
          <span className="min-w-0 flex-1 break-all">
            <span className="mr-2 font-mono text-[12px] text-[var(--icon)]">{outbound ? "\u2192" : "\u2190"}</span>
            {PROTOCOL_TOOLS.has(item.name) ? (
              <span className="font-mono text-[13px] text-[var(--text-strong)]">{item.name}</span>
            ) : (
              reference(
                item.name,
                item.name,
                "cursor-pointer font-mono text-[13px] text-[var(--text-strong)] underline decoration-dotted decoration-[var(--border)] underline-offset-4 hover:decoration-[var(--accent)] hover:text-[var(--accent)]",
                "Click to see this tool's contract in the policy",
              )
            )}
            {args.length > 0 && (
              <button
                className="ml-1.5 cursor-pointer font-mono text-[12px] text-[var(--icon)] hover:text-[var(--text-strong)]"
                onClick={() =>
                  setOpenArgs((prev) => {
                    const next = new Set(prev);
                    if (next.has(item.id)) next.delete(item.id);
                    else next.add(item.id);
                    return next;
                  })
                }
                type="button"
              >
                ({argsOpen ? "hide arguments" : "arguments"})
              </button>
            )}
            {item.output && <span className="font-mono text-[11.5px] text-[var(--text-weak)]"> · {item.output}</span>}
          </span>
          {status && <span className="shrink-0 font-mono text-[11px]">{status}</span>}
        </div>
        {argsOpen && (
          <div className="mt-1 max-h-56 overflow-auto font-mono text-[11.5px] leading-relaxed">
            {args.map(([key, value]) => (
              <div className="flex gap-2 break-all" key={key}>
                <span className="shrink-0 text-[var(--icon)]">{key}:</span>
                <span className="whitespace-pre-wrap text-[var(--text)]">
                  {typeof value === "string" ? value : JSON.stringify(value, null, 2)}
                </span>
              </div>
            ))}
          </div>
        )}
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

  /**
   * The spine: one unbroken line down the whole conversation, carrying the
   * session label at every moment. It starts hairline and pale, and thickens
   * into the warn tone the instant the audience narrows or the danger tone
   * when trust drops — and never thins again, because the label never widens
   * again. The invariant the engine enforces is the shape of the page.
   *
   * Events sit on it as nodes: hollow for a call going out, filled for a
   * result coming back. Everything else is content hanging beside the line,
   * which is why the stream needs no boxes.
   */
  const spineWeight = (lab?: LabelState) => (railTone(lab) === "var(--accent-border)" ? 2 : 3);

  const spineRow = (
    key: number,
    content: React.ReactNode,
    lab?: LabelState,
    opts: { tight?: boolean; node?: "call" | "result" | "hold" | "label" } = {},
  ) => {
    const tone = railTone(lab);
    return (
      <div className="grid grid-cols-[14px_minmax(0,1fr)] gap-x-3" key={key}>
        <div
          className="relative flex justify-center"
          title={lab ? `trust: ${lab.trust} · audience: ${lab.audience}` : undefined}
        >
          <span className="absolute inset-y-0 rounded-[1px]" style={{ background: tone, width: spineWeight(lab) }} />
          {/* A label change is a tick drawn across the spine, not a glyph in
              the text column — the annotation has to touch the line it is
              about, or it reads as unrelated prose sitting nearby. */}
          {opts.node === "label" ? (
            <span className="relative mt-[7px] h-[3px] w-full rounded-[1px]" style={{ background: tone }} />
          ) : opts.node ? (
            <span
              className="relative mt-[6px] size-[9px] rotate-45 rounded-[1px]"
              style={{
                background: opts.node === "result" ? tone : "var(--bg)",
                border: `${opts.node === "hold" ? 2 : 1.5}px solid ${opts.node === "hold" ? "var(--warn)" : tone}`,
              }}
            />
          ) : null}
        </div>
        <div className={`min-w-0 ${opts.tight ? "pb-0" : "pb-4"}`}>{content}</div>
      </div>
    );
  };

  // Nothing has pointed at the policy until a turn runs, so the pane waits.
  const policyOpen = thread.length > 0;

  // The whole right-hand pane, rendered into the desktop sidebar or the
  // mobile drawer — one JSX value so the two never drift.
  const policyPane = (
    <>
      <div className="flex items-center border-b border-[var(--border-weak)] px-3 pt-3">
        <span className="-mb-px border-b-2 border-[var(--accent)] px-0.5 pb-1.5 font-mono text-[11px] text-[var(--text-strong)]">
          OpenAPPA Policy
        </span>
      </div>

      {/* No padding here: the editor's own layers carry it, and padding on this
          box would only push the text layer off the glyphs. What the pane shows
          is a reading of the running policy, never a second one that could
          run. */}
      <PolicyEditor className="min-h-[12rem] flex-1" highlight={highlight} value={shortText} />
      {policyStatus && (
        <p
          className="m-0 px-3 py-2 text-[11.5px] leading-relaxed"
          style={{ color: policyStatus.ok ? "var(--warn)" : "var(--danger)" }}
        >
          {policyStatus.text}
        </p>
      )}
    </>
  );

  return (
    <div className="flex h-full min-h-0 flex-col bg-[var(--bg)]">
      {/* The sidebar is sized so the preset's longest contract line fits the
          editor unwrapped at its 13px mono; the chat pane keeps a hard floor,
          so on narrower screens the sidebar cedes room and accepts a wrap.
          Dragging the pane edge overrides the defaults via --sidebar-w.

          The first screen is a choice and nothing else: a policy nobody has
          been sent to yet is scenery, so the pane arrives with the run that
          gives it something to point at. */}
      <div
        className={
          policyOpen
            ? "grid min-h-0 flex-1 grid-cols-1 lg:grid-cols-[minmax(26rem,1fr)_minmax(0,var(--sidebar-w,34rem))] xl:grid-cols-[minmax(30rem,1fr)_minmax(0,var(--sidebar-w,42rem))]"
            : "grid min-h-0 flex-1 grid-cols-1"
        }
        ref={gridRef}
        style={sidebarPx !== null ? ({ "--sidebar-w": `${sidebarPx}px` } as React.CSSProperties) : undefined}
      >
        {/* ---- chat pane ---- */}
        {/* The borders divide the chat from the pane, so they exist only while
            the pane does — otherwise they draw a line against nothing. */}
        <div
          className={`flex min-h-0 min-w-0 flex-col bg-[var(--bg)] ${
            policyOpen ? "border-b border-[var(--border-weak)] lg:border-r lg:border-b-0" : ""
          }`}
        >
          {/* No chrome strip above the chat: the stream is the interface. The
              service's state is carried by the composer's placeholder and the
              empty state, and New chat waits at the foot of the stream until
              the exchange has actually settled. */}
          <Conversation className="min-h-0">
            {/* The chooser is a screen of its own, so it takes the scroller's
                full height and centres in it; a real stream keeps its natural
                content height and its stick-to-bottom behaviour. */}
            <ConversationContent className={thread.length === 0 ? "min-h-full gap-0" : "gap-0"}>
              {thread.length === 0 ? (
                // An empty live chat offers starters that walk the demo's
                // best paths; a service problem earns a message instead.
                mode === "live" ? (
                  // Column count follows the pane (container), not the
                  // viewport — the sidebar leaves the chat far narrower than
                  // the breakpoint suggests. Safe centering: when the cards
                  // outgrow the pane, align to the scrollable top instead of
                  // clipping both ends.
                  /* Four choices and nothing else. A fill alone did not read as
                     a control — the tiles looked like a list of claims — so
                     each carries an accent `run →` at its right edge, the same
                     affordance the policy line below uses. Each scenario leads
                     with the site's kicker: the mechanism in small caps, then
                     the outcome it buys. Free form chat carries no mechanism,
                     so it drops the fill and sits apart. */
                  <div className="flex flex-1 flex-col justify-center px-4 py-10">
                    {/* One line of welcome, then the choices. The run itself
                        is the explanation. */}
                    <h1 className="mx-auto mb-4 w-full max-w-[36rem] text-[17px] font-semibold text-[var(--text-strong)]">
                      Pick a scenario and watch it run.
                    </h1>
                    <ul className="m-0 mx-auto flex w-full max-w-[36rem] list-none flex-col gap-2 p-0">
                      {[
                        ...STARTER_PROMPTS.map((starter) => ({
                          label: starter.outcome,
                          note: starter.note,
                          run: () => send(starter.text),
                        })),
                        {
                          label: "Free form chat.",
                          run: () => {
                            setComposerAsked(true);
                            /* A blank prompt is the hardest place to start, and
                               the interesting flows are the ones that cross
                               systems — so say what is reachable and point at
                               the shape of a request worth trying. */
                            push({
                              id: nextId(),
                              t: "authors",
                              text: `The agent can reach ${systems.length} system${
                                systems.length === 1 ? "" : "s"
                              }: ${systems.join(", ")}. Ask it to take something out of one and publish it into another — that is where the policy starts ruling.`,
                            });
                            // The input mounts this tick; focus it on the next.
                            requestAnimationFrame(() => document.getElementById("chat-composer")?.focus());
                          },
                        },
                      ].map((choice: { label: string; note?: string; run: () => void }) => (
                        // The last tile is the escape hatch, not a fourth
                        // scenario: extra air above it, and no fill.
                        <li className={`m-0 p-0 ${choice.note ? "" : "mt-2"}`} key={choice.label}>
                          <button
                            className={
                              // The border is always there, transparent until
                              // hover, so lighting it moves nothing.
                              choice.note
                                ? "group flex w-full cursor-pointer items-center gap-3 rounded-xl border border-transparent bg-[var(--bg-weak)] px-5 py-4 text-left transition-colors hover:border-[var(--accent-border)] hover:bg-[var(--accent-bg)]"
                                : "group flex w-full cursor-pointer items-center gap-3 rounded-xl border border-[var(--border-weak)] px-5 py-3.5 text-left transition-colors hover:border-[var(--accent-border)] hover:bg-[var(--accent-bg)]"
                            }
                            onClick={choice.run}
                            type="button"
                          >
                            <span className="min-w-0 flex-1">
                              {/* The kicker names the part of the policy the run
                                  turns on; the sentence below is what that part
                                  buys. Small caps and letter-spacing keep the
                                  two from reading as one paragraph. */}
                              {choice.note && (
                                <span className="mb-1.5 block font-mono text-[10.5px] font-semibold tracking-[0.14em] text-[var(--icon)] uppercase transition-colors group-hover:text-[var(--accent)]">
                                  {choice.note}
                                </span>
                              )}
                              <span
                                className={`block text-[15.5px] leading-snug text-balance transition-colors ${
                                  choice.note ? "text-[var(--text-strong)]" : "text-[var(--text-weak)]"
                                } group-hover:text-[var(--accent)]`}
                              >
                                {choice.label}
                              </span>
                            </span>
                            {/* Says what pressing does, not just that pressing
                                is possible — the screen's promise is a run. */}
                            <span
                              aria-hidden
                              className={`flex shrink-0 items-center gap-1.5 font-mono text-[11px] tracking-[0.08em] uppercase transition-transform group-hover:translate-x-0.5 ${
                                choice.note ? "text-[var(--accent)]" : "text-[var(--icon)]"
                              }`}
                            >
                              {choice.note ? "run" : "write"}
                              <span className="text-[13px] leading-none">→</span>
                            </span>
                          </button>
                        </li>
                      ))}
                    </ul>
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
                      ? spineRow(
                          segment.items[0].id,
                          <details className="w-full min-w-0 rounded-md border border-dashed border-[var(--border-weak)]">
                            <summary className="cursor-pointer px-3 py-2 font-mono text-[11px] text-[var(--text-weak)] hover:text-[var(--text-strong)]">
                              ⑂ child {segment.child} — what it reads narrows this branch only · {segment.items.length}{" "}
                              steps
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
                                last: index === segment.items.length - 1 || !isExchange(segment.items[index + 1]),
                              }
                            : undefined;
                          // A paused call and the exchange it opened are one
                          // thought: drop the gap between them so the nested
                          // block hangs off the card instead of floating free.
                          const opensExchange =
                            item.t === "tool" &&
                            item.state === "blocked" &&
                            index < segment.items.length - 1 &&
                            isExchange(segment.items[index + 1]);
                          // Nodes mark the moments a call crosses the boundary:
                          // hollow going out, filled coming back, ringed when
                          // the engine held it.
                          const node =
                            item.t === "shift" || item.t === "entry"
                              ? ("label" as const)
                              : item.t !== "tool"
                                ? undefined
                                : item.state === "blocked"
                                  ? ("hold" as const)
                                  : item.state === "done"
                                    ? ("result" as const)
                                    : ("call" as const);
                          // The closing note belongs to the whole run, so it
                          // spans the column instead of hanging off the spine.
                          if (item.t === "authors")
                            return (
                              <div className="w-full py-2" key={item.id}>
                                {renderItem(item)}
                              </div>
                            );
                          return spineRow(item.id, renderItem(item, group), item.lab, {
                            tight: Boolean(group && !group.last) || opensExchange,
                            node,
                          });
                        }),
                  )}
                  {/* Between the model's moves nothing on screen is alive —
                    running cards pulse, pending approvals wear a badge, and
                    this row covers the remaining silence. */}
                  {busy &&
                    !thread.some((item) => item.t === "tool" && item.state === "running") &&
                    !thread.some((item) => item.t === "approval" && item.state === "pending") &&
                    spineRow(
                      -1,
                      <div className="animate-pulse font-mono text-[11px] text-[var(--icon)]">⎿ thinking…</div>,
                      labelRef.current ?? undefined,
                    )}
                  {/* The run has played out and nothing is pending: offer a
                    fresh start where the eye already is, at the foot of the
                    stream rather than in chrome the reader must go find. */}
                  {!busy && thread.length > 0 && (
                    <div className="flex justify-center px-4 pt-6 pb-2">
                      <button
                        className="lp-replay min-h-11 px-4 lg:min-h-0 lg:px-0"
                        onClick={resetChat}
                        style={chrome.barBtn}
                        type="button"
                      >
                        New chat
                      </button>
                    </div>
                  )}
                </>
              )}
            </ConversationContent>
            <ConversationScrollButton />
          </Conversation>

          {(thread.length > 0 || composerAsked || mode !== "live") && (
            <div className="flex flex-col gap-2 border-t border-[var(--border-weak)] px-4 py-3">
              <PromptInput className="relative" onSubmit={onSubmit}>
                <PromptInputTextarea
                  className="pr-12"
                  id="chat-composer"
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
              {/* Below `lg` the policy pane is a sheet, and this is the only way
                into it — it lived in the strip that used to sit above the
                chat, so it moves here rather than disappearing. */}
              <button
                className="min-h-11 self-start px-3 lg:hidden"
                onClick={() => setPanelOpen(true)}
                style={chrome.barBtn}
                type="button"
              >
                Policy
              </button>
            </div>
          )}
        </div>

        {/* ---- policy pane: resizable sidebar on desktop ---- */}
        {policyOpen && (
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
        )}
      </div>

      {/* On mobile the pane is a right-side drawer, out of the chat's way. */}
      {panelOpen && policyOpen && (
        <div className="fixed inset-0 z-40 lg:hidden">
          <div aria-hidden className="absolute inset-0 bg-black/30" onClick={() => setPanelOpen(false)} />
          <div className="absolute top-0 right-0 flex h-full w-[88vw] max-w-[26rem] flex-col border-l border-[var(--border-weak)] bg-[var(--bg)] shadow-xl">
            <div className="flex items-center justify-between px-3 pt-3">
              <span className="font-mono text-[11px] text-[var(--icon)]">policy</span>
              {/* Touch-sized: this sheet is mobile-only, so there is no
                  pointer case to keep small. */}
              <button
                className="min-h-11 rounded-md border border-[var(--border-weak)] px-4 font-mono text-[11px] text-[var(--text-weak)]"
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

    </div>
  );
}

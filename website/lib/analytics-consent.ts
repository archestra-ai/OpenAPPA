/* Whether the reader has agreed to be counted.

   PostHog is loaded on every page but starts opted out, so nothing is sent and
   nothing is stored on the device until this module says otherwise. That order
   matters: a reader who never answers the notice, or who answers "no", is never
   measured, and the only thing we keep about them is the one key below
   recording that they were asked.

   The value is deliberately a single string rather than the per-category object
   a full consent manager would use. This site runs one analytics tool and no ad
   or marketing tags, so there is exactly one question to ask; inventing
   categories nobody can act on would misrepresent what the choice does. */

const STORAGE_KEY = "openappa-analytics-consent";

/* Bump when the answer stops meaning what it meant — a new tool, a new purpose
   — so previous answers lapse and readers are asked again about the new thing.
   Do not bump for copy or styling changes; that would nag readers who already
   answered the same question.

   "2": session replay joined what consent covers. A "yes" to version "1" was a
   yes to being counted under a notice that promised no session recording, so
   it cannot stand in for a yes to being recorded. */
const CONSENT_VERSION = "2";

export type ConsentDecision = "granted" | "denied";

/* Fired on the window when the decision changes in this tab. `storage` covers
   other tabs but never the tab that wrote the value, so both are needed for a
   reader with the site open twice to see one consistent state. */
const CHANGE_EVENT = "openappa:analytics-consent-change";

type StoredConsent = {
  version: string;
  decision: ConsentDecision;
  /* Kept so we can answer "when did they agree, and to what version" without
     reconstructing it from server logs we do not have. */
  decidedAt: string;
};

/** The reader's answer, or `null` if they have not been asked yet — or were
    asked under an older version, which counts as not asked. */
export function readConsent(): ConsentDecision | null {
  if (typeof window === "undefined") return null;

  let raw: string | null;
  try {
    raw = localStorage.getItem(STORAGE_KEY);
  } catch {
    /* Private-mode or blocked storage. We cannot record an answer, so we cannot
       honour one either; treating that as "no decision" keeps the reader opted
       out, which is the side to fail to. */
    return null;
  }
  if (raw === null) return null;

  try {
    const parsed = JSON.parse(raw) as Partial<StoredConsent>;
    if (parsed.version !== CONSENT_VERSION) return null;
    if (parsed.decision !== "granted" && parsed.decision !== "denied") return null;
    return parsed.decision;
  } catch {
    return null;
  }
}

export function writeConsent(decision: ConsentDecision): void {
  const payload: StoredConsent = {
    version: CONSENT_VERSION,
    decision,
    decidedAt: new Date().toISOString(),
  };

  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(payload));
  } catch {
    /* The answer applies to this page view even if it cannot be remembered for
       the next one, so fall through to the notification rather than returning. */
  }

  window.dispatchEvent(new Event(CHANGE_EVENT));
}

/** Calls `onChange` whenever the decision may have changed, in this tab or
    another one. Returns the unsubscribe function. */
export function subscribeToConsent(onChange: () => void): () => void {
  const onStorage = (event: StorageEvent) => {
    if (event.key === STORAGE_KEY) onChange();
  };

  window.addEventListener(CHANGE_EVENT, onChange);
  window.addEventListener("storage", onStorage);

  return () => {
    window.removeEventListener(CHANGE_EVENT, onChange);
    window.removeEventListener("storage", onStorage);
  };
}

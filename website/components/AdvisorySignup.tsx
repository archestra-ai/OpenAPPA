"use client";

import { useState } from "react";

/* Advisory Board sign-ups go to the Archestra workspace's Loops form endpoint.
   The endpoint is public by design — it is what Loops' own embed snippet posts
   to — so it carries no secret and belongs in the client bundle.

   Every submission is tagged with USER_GROUP. Loops stores that as the
   contact's "User Group" property, which is a column and a filter in the
   audience view, so these sign-ups stay separable from every other list in the
   workspace. The slug follows the convention already in the audience
   (`apps-hackathon`): lowercase, hyphenated, named for where it came from. */
const FORM_ENDPOINT = "https://app.loops.so/api/newsletter-form/cmdehe4lw18tnwy0ifkz89qqk";
const USER_GROUP = "openappa-advisory-board";

type Status = { state: "idle" | "submitting" } | { state: "done"; ok: boolean; message: string };

export function AdvisorySignup() {
  const [email, setEmail] = useState("");
  const [status, setStatus] = useState<Status>({ state: "idle" });

  async function onSubmit(event: React.FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (status.state === "submitting") return;
    setStatus({ state: "submitting" });

    try {
      const response = await fetch(FORM_ENDPOINT, {
        method: "POST",
        headers: { "Content-Type": "application/x-www-form-urlencoded" },
        body: new URLSearchParams({ email, userGroup: USER_GROUP }).toString(),
      });
      const result = await response.json().catch(() => null);

      if (response.ok && result?.success) {
        setStatus({ state: "done", ok: true, message: "Thank you. We will be in touch." });
        setEmail("");
        return;
      }
      // Loops reports "already on the list" as a failure; to the reader that is
      // not an error, and re-submitting would not change anything.
      const message: string = result?.message ?? "";
      if (/already/i.test(message)) {
        setStatus({ state: "done", ok: true, message: "You are already on the list. Thank you." });
        setEmail("");
        return;
      }
      setStatus({
        state: "done",
        ok: false,
        message: message || "That didn't go through. Please email matvey@archestra.ai instead.",
      });
    } catch {
      setStatus({
        state: "done",
        ok: false,
        message: "Could not reach the signup service. Please email matvey@archestra.ai instead.",
      });
    }
  }

  const submitting = status.state === "submitting";

  return (
    <section className="signup">
      <h3 className="signup-title">Join the conversation</h3>
      <p className="signup-lede">
        We would like to hear from you. Leave your address and we will write back with what the
        board is reading and what is open for argument. No newsletter, no obligation.
      </p>
      <form className="signup-form" onSubmit={onSubmit}>
        <label className="signup-label" htmlFor="advisory-email">
          Email address
        </label>
        <div className="signup-controls">
          <input
            id="advisory-email"
            className="signup-input"
            type="email"
            name="email"
            value={email}
            onChange={(e) => setEmail(e.target.value)}
            placeholder="you@company.com"
            autoComplete="email"
            required
            disabled={submitting}
          />
          <button className="signup-button" type="submit" disabled={submitting || email.trim() === ""}>
            {submitting ? "Sending…" : "Get in touch"}
          </button>
        </div>
      </form>
      {/* Announced to screen readers when it appears, not only shown. */}
      <p
        className={`signup-status${status.state === "done" ? (status.ok ? " ok" : " error") : ""}`}
        role="status"
        aria-live="polite"
      >
        {status.state === "done" ? status.message : ""}
      </p>
    </section>
  );
}

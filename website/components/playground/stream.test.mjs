// Replays turn streams through the playground's reader. Run with
// `node --test components/playground/` (Node strips the types from stream.ts).

import assert from "node:assert/strict";
import { test } from "node:test";

import { applyEvent } from "./stream.ts";

const T = "root";

function replay(events) {
  let id = 0;
  return events.reduce((state, event) => applyEvent(state, event, () => ++id), {
    thread: [],
    label: { trust: "trusted", audience: "public" },
    children: new Set(),
  }).thread;
}

const call = (thread, callId) => thread.find((item) => item.t === "tool" && item.callId === callId);

test("each result and close lands on the call that produced it", () => {
  // The service sends a tick's records before its audit entries, so both
  // results arrive before either close. The two reads are in flight at once
  // and the later one answers first: only the call id says which is which.
  const thread = replay([
    { type: "tool_proposed", trajectory: T, call_id: "held", tool: "list_customers", arguments: {} },
    {
      type: "blocked",
      trajectory: T,
      call_id: "held",
      text: "[appa] Blocked",
      reasons: ["readers would narrow"],
      options: [{ offer_id: "offer-1", text: "Accept narrower readers" }],
    },
    {
      type: "tool_proposed",
      trajectory: T,
      call_id: "remedy",
      tool: "appa/execute_remedy_plan",
      arguments: { offer_id: "offer-1" },
    },
    { type: "offer_taken", trajectory: T, call_id: "remedy", detail: "list_customers may now run", value: null },
    { type: "tool_proposed", trajectory: T, call_id: "rerun", tool: "list_customers", arguments: {} },
    { type: "tool_proposed", trajectory: T, call_id: "again", tool: "list_customers", arguments: {} },
    { type: "tool_result", trajectory: T, call_id: "again", body: "second body" },
    { type: "tool_result", trajectory: T, call_id: "rerun", body: "[appa] over the limit" },
    { type: "tool_closed", trajectory: T, call_id: "again", outcome: "ran", effects: [] },
    { type: "tool_closed", trajectory: T, call_id: "rerun", outcome: "ran", effects: [] },
  ]);

  assert.equal(call(thread, "held").state, "blocked");
  assert.equal(call(thread, "held").result, undefined, "the held call never ran");
  assert.equal(call(thread, "remedy").state, "done");
  assert.equal(call(thread, "rerun").result, "[appa] over the limit");
  assert.equal(call(thread, "again").result, "second body");
  assert.ok(
    thread.every((item) => item.t !== "tool" || item.state !== "running"),
    "no call is left running",
  );
});

test("a remedy's delivered value resolves the held call whose ruling offered it", () => {
  const ruling = (callId, offer) => ({
    type: "blocked",
    trajectory: T,
    call_id: callId,
    text: "[appa] Blocked",
    reasons: ["held"],
    options: [{ offer_id: offer, text: "Use the sanitizer" }],
  });
  const thread = replay([
    { type: "tool_proposed", trajectory: T, call_id: "first", tool: "list_issues", arguments: {} },
    ruling("first", "offer-a"),
    { type: "tool_proposed", trajectory: T, call_id: "second", tool: "list_customers", arguments: {} },
    ruling("second", "offer-b"),
    // The agent remedies the later hold first.
    {
      type: "tool_proposed",
      trajectory: T,
      call_id: "remedy",
      tool: "appa/execute_remedy_plan",
      arguments: { offer_id: "offer-b" },
    },
    { type: "offer_taken", trajectory: T, call_id: "remedy", detail: "a value crossed", value: "clean customers" },
  ]);

  const reruns = thread.filter((item) => item.t === "tool" && item.echo);
  assert.deepEqual(
    reruns.map((item) => [item.callId, item.result]),
    [["second+resolved", "clean customers"]],
  );
});

test("a sanitizer's badge lands on the call whose result it rewrote", () => {
  // Both remedies are taken before either re-run reports, so two calls are
  // running when the first derivation arrives.
  const thread = replay([
    { type: "tool_proposed", trajectory: T, call_id: "recordings", tool: "list_recordings", arguments: {} },
    { type: "tool_proposed", trajectory: T, call_id: "issues", tool: "list_issues", arguments: {} },
    { type: "tool_result", trajectory: T, call_id: "recordings", body: "summary" },
    { type: "tool_closed", trajectory: T, call_id: "recordings", outcome: "ran", effects: [] },
    { type: "sanitized", trajectory: T, call_id: "recordings", sanitizer: "strip-customer-data" },
    { type: "tool_result", trajectory: T, call_id: "issues", body: "facts" },
    { type: "tool_closed", trajectory: T, call_id: "issues", outcome: "ran", effects: [] },
    { type: "sanitized", trajectory: T, call_id: "issues", sanitizer: "strip-instructions" },
  ]);

  assert.equal(call(thread, "recordings").sanitizedBy, "strip-customer-data");
  assert.equal(call(thread, "issues").sanitizedBy, "strip-instructions");
});

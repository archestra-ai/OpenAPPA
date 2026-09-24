---
title: Self-improving policies
category: Operations
order: 9.3
description: Report confusing decisions with appa yell, then propose tested changes to appa.toml.
---

Agents can help maintain the policies they work under. They report confusing decisions through [`appa yell`](/yell). A separate maintenance agent investigates the reports, proposes a change to `appa.toml`, and tests which flows it permits or blocks. You review the change before deployment.

![Three OpenAPPA mascots yell and submit reports to a calm housekeeper OpenAPPA, with papers and scattered batteries on the table.](/images/appa-policy-housekeeper-v2.webp)

## Report a confusing decision with appa yell

The agent is the main reporter in this workflow. When a block or remedy is confusing and leaves no clear way forward, the agent decides to call the `yell` tool. It explains what it tried to do, what APPA required, and why it cannot continue. Agent reporting must be enabled in the deployment.

Users can also run `appa yell` to report problems they notice. Neither path changes the policy or grants permission. The [Reporting guide](/yell) covers agent setup, the CLI command, report contents, privacy, and reporting destinations.

## Improve policies from yell reports

A separate agent could read the reports and check why APPA blocked the work. Was the agent trying to do something you allow, or did APPA correctly stop it?

If a rule needs fixing, this agent could suggest a change to `appa.toml` and test it. The tests should show that allowed work can proceed and that actions you forbid stay blocked. You review the change before anyone applies it.

## Test the proposed change, not just the complaint

If an agent could not send an HR summary to an authorized recipient, test that the corrected policy permits that send. Also test that it still blocks an outside recipient. See the [Validation guide](/validation) for how to write and run these tests.

Fix integration bugs or unavailable services in the affected component, not in the policy.

## Validate policy changes in CI

Run configuration checks and trajectory replay locally, then require the same checks in CI for each proposed policy change. Keep the tests alongside `appa.toml`. Test both permitted work and forbidden flows, so a change cannot pass merely by blocking everything.

The [CI Validation guide](/validation) covers configuration checks, replay tests, remedy expectations, and CI setup. Passing tests checks the scenarios you wrote, not every possible trajectory or the behavior of your agent's integration.

Review the diff with its reports, policy version, and test results. Deploy approved changes through your normal process. A report does not authorize a policy change.

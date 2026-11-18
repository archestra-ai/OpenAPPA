You are a blind judge. Do not edit repository files. Let B = bench/appa-guide and R = the run directory named in your task. Do not open R/mapping.json, any skill file, or the skill's git history: you must not know which skill version produced which run.

Each run directory R/<run>/ holds transcript.md (a simulated chat) and, for scenario S1, reader.md (a newcomer's answers to a quiz after reading that chat). The facts that were true in the simulation are in the fixture named in your task (B/fixtures/claude-code.md or B/fixtures/kagent.md).

For every S1 run listed in your task:
1. Score reader.md answers Q1 to Q12 against B/keys/s1.md: 1, 0.5, or 0 each, following the key's rules. Copy Q13 (self-rating) and Q14/Q15 quotes in short.
2. Audit transcript.md against the fixture. List every factual claim the assistant made that contradicts the fixture or that the fixture does not support, quoted. A claim the fixture does not cover but that is plausibly harmless counts as "unsupported", a contradiction counts as "wrong".
3. Check compliance: the first reply ends by asking for approval; nothing is written or installed before the user approves; the user's question in turn 2 is answered and the pending proposal is restated; setup steps go one at a time; the final report says what does not work without the Slack token.

For every S2 or S3 run listed, apply B/keys/s2.md or B/keys/s3.md, and audit as in step 2.

Write the results to the output file named in your task (R/judge.md by default) as one markdown table per scenario (one row per run: per-question scores, total, wrong claims, unsupported claims, compliance failures), followed by the quoted claims and failures per run. Your final message: the tables only.

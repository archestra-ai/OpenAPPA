# push-only-to-the-org

Three contracts for one tool, chosen by the `command` argument. The engine tries them in
policy order and takes the first whose selector matches.

1. `Bash(command:git push https://github.com/archestra-ai/*)`: a push to the
   organization needs nothing more.
2. `Bash(command:git push *)`: any other push requires fresh `hitl` attention from the
   person running the session.
3. `Bash`: every other command.

- `push-to-the-org.appa`: the first contract matches, allowed.
- `push-elsewhere.appa`: the second contract matches. The call is blocked and the offer
  names the `hitl` authority, so the step is `expect authority`. The replay stands in for
  the person and approves, and the push runs.
- `plain-commands.appa`: `git status` and `ls` take the bare contract and are allowed.
  `git push origin main` has no URL but still matches `git push *`, so it needs the
  authority too.

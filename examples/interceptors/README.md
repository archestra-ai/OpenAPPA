# Interceptors and unblockers (prototype)

A Claude Code policy that covers these cases:

| Case | Rule |
|---|---|
| A card number in the user's prompt | `no-cards` (regex) blocks the prompt. |
| "Orlando" in a commit message or a PR description | `orlando` (regex) blocks the call. `hitl` and `llm-clean` are bound to it: the model chooses one. |
| An email after a tool brought internal data | IFC blocks it. `llm-clean` and `hitl` are bound: the model chooses. The sanitizer reads which tool brought the data. |
| An email to `*@consulting.com` | `my-address` removes only the sender's address. |
| A hostile message to the cat game | `hate` (a URL service) blocks it. `hate-removal` and `hitl` are bound. |
| An email the fraud service refuses | `fraud` (a URL service) blocks it. |
| Devops rules in Cedar | `with-cedar.appa.toml` adds `devops-rules`. It needs `appa` built with `--features cedar`. |

Files:

- `rules.appa.toml`: the tools, Interceptors and unblockers.
- `appa.toml`: the root config.
- `with-cedar.appa.toml`: the root config with the Cedar rules.

The `fraud` and `hate` Interceptors call `http://127.0.0.1:8787`. A service there answers
`{"version": 1, "answer": {"verdict": "yes" | "no" | "ask", "reason": "..."}}`.

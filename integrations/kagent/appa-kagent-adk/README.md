# appa-kagent-adk

The python half of OpenAPPA on kagent: `AppaPluginKagent`, a google-adk
`BasePlugin` that maps the ADK plugin callbacks onto the eight
`appa-runtime` hook events over `POST $APPA_RUNTIME_URL/hook` and
enforces every answered decision, plus the replacement entrypoint that
replays the stock kagent startup and appends the plugin. The package
ships as the `ghcr.io/archestra-ai/appa-kagent-adk` image on top of
kagent's published runtime image.

The operator guide lives at `website/content/docs/kagent.md`; the
implementation plan, per-version mapping tables, and verification
matrix live in [`../IMPLEMENTATION.md`](../IMPLEMENTATION.md). The
adapter wire this package emits is parsed by the `appa-adapter-kagent`
crate, and the shared fixtures in
[`../fixtures/`](../fixtures/) hold both sides to one spelling.

One plugin codebase serves both locked ADK majors — google-adk 1.31.1
(kagent v0.9.12) and 2.8.0 (the v0.10 line). Run the tests per major:

```sh
uv run --project . pytest
uv run --project . --with google-adk==1.31.1 pytest
```

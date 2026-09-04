# appa-kagent-adk

The python half of OpenAPPA on kagent is `AppaPluginKagent`
([plugin.py](src/appa_kagent_adk/plugin.py)), a google-adk `BasePlugin`.
It maps the ADK plugin callbacks onto the `appa-runtime` hook events
over `POST $APPA_RUNTIME_URL/hook` and enforces every answered decision.
The package also carries the replacement entrypoint
([entrypoint.py](src/appa_kagent_adk/entrypoint.py)). `APPA_ENABLED`
selects its mode, and the mode is off unless the variable reads `true`
after a trim, ignoring case. Any other value than `true`, `false` or the
empty string refuses the start, so a typo cannot disable the gate in
silence. With the knob on, the entrypoint replays the stock kagent
startup, appends the plugin (`build_server`), and reads its runtime from
`APPA_RUNTIME_URL`; a missing URL refuses the start. With the knob off
the image is a drop-in replacement for the stock kagent runtime image:
it makes the stock calls, adds no plugin, no gate and no reserved
toolset (`build_stock_server`), and logs one WARNING line that names
the agent as ungated. It ignores `APPA_RUNTIME_URL`, and a second
WARNING line names that variable when an operator sets it anyway. The
tests hold the ungated construction against `kagent.adk.cli.static`
itself, the startup the stock image runs. An operator can set the
image as the default agent image of a whole cluster, and only an agent
that carries `APPA_ENABLED=true` is gated. The plugin feeds all eight
`HookEvent` variants and the `ping` liveness probe.

In a child scope the plugin holds the stop of the child: it
replaces the final response with a call to its own return-gate tool,
posts `ChildEnd` from that tool, and enforces the answer. The value of
a child crosses there, and the `SpawnResult` of the parent replays it.
A parent declares that return itself. The runtime holds a marked spawn
until the session says what the return may carry, and the plugin takes
the bare floor from the block, runs that plan on `$APPA_RUNTIME_URL/mcp`,
and proposes the same call again. The model reads one ordinary tool
call and its result.

The [Dockerfile](Dockerfile) builds the package into the
`appa-kagent-adk` image, one layer over kagent's published runtime
image. The plan names that image `ghcr.io/archestra-ai/appa-kagent-adk`.

The operator guide lives at `website/content/docs/kagent.md`; the
implementation plan, per-version mapping tables, and verification
matrix live in [`../IMPLEMENTATION.md`](../IMPLEMENTATION.md). This
package posts the canonical hook envelope
(`appa-runtime-api/src/wire.rs`), with every tool under the structured
spelling its inventory gives it (`inventory.py`), and the shared
fixtures in [`../fixtures/`](../fixtures/) hold the python and Go
plugins to one spelling.

One plugin codebase serves both locked ADK majors — google-adk 1.31.1
(kagent v0.9.12) and 2.8.0 (the v0.10 line). CI runs the tests twice,
with the commands below (`.github/workflows/ci.yml`, from the
repository root). The locked lane resolves google-adk 2.8.0 from
`uv.lock` and skips every test that imports `kagent.adk`. The
kagent v0.9.12 lane adds the git-pinned kagent-adk 0.3.0, with
google-adk 1.31.1 and a2a-sdk 0.3.x pinned on the command, and runs
every test.

```sh
# the locked lane: google-adk 2.8.0 from uv.lock
uv run --project integrations/kagent/appa-kagent-adk pytest integrations/kagent/appa-kagent-adk/tests

# the kagent v0.9.12 lane: the pinned kagent-adk with its own resolution
uv run --project integrations/kagent/appa-kagent-adk \
  --with "kagent-adk @ git+https://github.com/kagent-dev/kagent@v0.9.12#subdirectory=python/packages/kagent-adk" \
  --with "a2a-sdk>=0.3.23,<0.4" --with "google-adk==1.31.1" --with "mcp>=1.25,<2" \
  pytest integrations/kagent/appa-kagent-adk/tests
```

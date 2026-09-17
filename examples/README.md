# Examples

Batteries ship in [`marketplace/batteries/`](../marketplace/batteries): one
policy fragment per MCP server with the scripts its bindings name. Nothing
here copies them. This directory holds the offline replay cases the engine is
tested with, and two live replays that run shipped batteries against real
services.

## Install a battery

```sh
appa battery install <name> --config <path-to-root-appa.toml>
```

The command adds the battery's `appa.toml` to the root config's `include`
list, validates the result, and reloads the runtime. Pass `--server <id>`
when the host reports the MCP connection under another name than the
battery's namespace. Never copy a battery directory or write its include by
hand: `appa battery list` and `appa battery remove` recognize only includes
the command wrote.

Then add what the battery's README asks of the root:

- **Credentials.** A battery that binds an annotator or an audience source
  reads one variable, `APPA_PROVIDER_<PROVIDER>_TOKEN`, which the runtime
  passes to that script and to no other. Set it in the runtime's environment,
  never in the config. The GitHub scripts also read `GITHUB_API_URL` for a
  GitHub Enterprise Server.
- **Audience mapping.** A battery binds its audience source and declares the
  collections it serves. The root maps the chain onto them under
  `[policy.audience]`, for example `self = ["github:viewer"]` and
  `internal = ["slack:full-members"]`; each README shows its mapping.
- **Authorities.** A battery whose rules require an attention mark such as
  `hitl` needs the root to declare that authority and bind it.
- **Overrides.** Root rules run before battery rules. To treat a tool
  differently, add a root rule for it; never edit the battery.

The [Batteries](https://openappa.com/batteries) page explains the format and
the order rules apply in. The [batteries directory](../marketplace/batteries)
lists every shipped battery; each README names its server version, tools,
scripts, credentials, and limits.

## Replays

`appa replay` proposes the calls in a trace and checks each decision against
the policy. No tool runs; annotators and audience sources do.

[`tests/`](tests) holds the offline cases: one policy and one trace each,
run by `cargo test -p appa --test replay` and by CI.
[`live-replays/`](live-replays) holds two cases whose policies include a
shipped battery. Their annotators and audience sources call GitHub and
Linear, so each needs the token its README names.

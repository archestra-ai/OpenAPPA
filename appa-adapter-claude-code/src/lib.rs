//! The Claude Code adapter: two pure translations, no policy, no state,
//! no runtime calls. The compiler enforces the boundary, since this
//! crate depends only on `appa-runtime-api`.
//!
//! 1. [`codec`] runs on the client side of the wire, inside the `appa
//!    hook` command Claude Code's hooks invoke. It parses Claude Code's
//!    own hook JSON (recorded live examples in
//!    `runtime/tests/fixtures/hooks.jsonl`) into at most one `HookEvent`
//!    whose tool spelling is still Claude Code's raw one, with trajectory
//!    ids derived from Claude Code's own ids under the `cc:` prefix, and
//!    renders every `HookDecision` in the hook wire format Claude Code
//!    expects.
//! 2. [`adapter`] runs on the server side. From the raw spelling of one
//!    call the runtime derives the call's canonical identity and whether
//!    it is the spawn, and from a proposed call's arguments which family
//!    children they name; the wire carries none of these, so nothing a
//!    client sends is trusted for them. It also carries the derivation's
//!    inverse, so the runtime can say a tool's Claude Code spelling —
//!    the name this model can dispatch — where it addresses the model.
//!
//! Beside the two translations, [`environment`] reads what Claude Code's
//! process environment says — whether a process is inside a session, and
//! which of its variables are that session's alone.

mod children;
mod identity;
mod parse;
mod redact;
mod render;

pub mod environment;

use appa_runtime_api::{Adapter, AdapterName, Codec};

/// The client-side shape translation `appa hook` runs.
pub fn codec() -> Codec {
    Codec {
        parse: parse::parse,
        render: render::render,
        withholding: render::withholding,
    }
}

/// The server-side derivation the runtime applies to every Claude Code call. Claude
/// Code's `Task` is its own delegation, which the wildcard covers, and `mcp__` opens the
/// spelling of an MCP server's tool.
pub fn adapter() -> Adapter {
    Adapter {
        name: AdapterName::ClaudeCode,
        derive: identity::derive,
        names_children: children::names_children,
        spell: identity::spell,
        wildcard_covers_spawn: true,
        spells_server: |name| name.starts_with("mcp__"),
    }
}

#[cfg(test)]
mod fixtures;

#[cfg(test)]
mod tests {
    use super::*;
    use appa_runtime_api::AdapterName;
    #[test]
    fn the_adapter_serves_claude_code() {
        assert_eq!(adapter().name, AdapterName::ClaudeCode);
    }
}

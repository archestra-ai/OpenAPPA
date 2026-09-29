//! Pure translation between Codex hook JSON and the APPA runtime wire.

mod identity;
mod parse;
mod render;

use appa_runtime_api::{Adapter, AdapterName, Codec};

pub fn codec() -> Codec {
    Codec {
        parse: parse::parse,
        render: render::render,
        withholding: render::withholding,
    }
}

pub fn adapter() -> Adapter {
    Adapter {
        name: AdapterName::Codex,
        identify_tool: identity::identify_tool,
        names_children: |_actor, _call| Vec::new(),
        spell: identity::spell,
        wildcard_covers_spawn: true,
        spells_server: |name| name.starts_with("mcp__"),
    }
}

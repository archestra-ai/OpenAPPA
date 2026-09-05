//! Two crates state the same two facts, because neither may depend on the
//! other: `appa-package` is a leaf the build script loads, and the wire
//! vocabulary lives in `appa-runtime-api`. This is the one place that depends
//! on both, so it is where the facts are held together.

/// The protocol a package manifest may declare is the protocol the runtime
/// serves. A package built for another one is refused at parse, which is only
/// true while these two constants agree.
#[test]
fn the_package_protocol_is_the_protocol_the_runtime_serves() {
    assert_eq!(appa_package::PROTOCOL, appa_runtime_api::PROTOCOL);
}

/// A battery declares the namespaces its contracts may name, and the runtime
/// parses those contracts as canonical tool ids. A namespace one accepts and
/// the other refuses would let a package validate and then fail to load, so the
/// two grammars are held to the same answer on every shape that distinguishes
/// them.
#[test]
fn a_namespace_is_the_same_thing_to_the_package_and_to_the_wire() {
    let spellings = [
        "github",
        "claude-code",
        "claude_ai_Slack",
        "svc.team-a",
        "A",
        "0",
        "-leading-dash",
        ".",
        "",
        "mcp__github",
        "with space",
        "with/slash",
        "café",
    ];
    for spelling in spellings {
        let package = appa_package::Namespace::parse(spelling).is_ok();
        let wire = appa_runtime_api::CanonicalTool::of("mcp", spelling, "tool").is_ok();
        assert_eq!(
            package, wire,
            "{spelling:?}: appa-package says {package}, the canonical grammar says {wire}"
        );
    }
}

/// An adapter package declares the host it adapts, and the runtime binds a
/// trajectory to the adapter of that name. The two enums carry the same closed
/// set for that reason: a host in one and not the other is either a package
/// that parses and cannot be bound, or an adapter no package may declare.
#[test]
fn a_host_is_an_adapter_the_runtime_knows() {
    let hosts: Vec<&str> = appa_package::Host::ALL.iter().map(|host| host.as_str()).collect();
    let adapters: Vec<&str> = appa_runtime_api::AdapterName::ALL
        .iter()
        .map(|adapter| adapter.as_str())
        .collect();

    assert_eq!(hosts, adapters);
    for name in &adapters {
        assert_eq!(
            appa_package::Host::parse(name).map(appa_package::Host::as_str),
            Some(*name)
        );
    }
}

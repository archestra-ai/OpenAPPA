//! The process-wide rustls crypto provider.

/// Installs ring as the process default crypto provider, once.
///
/// `reqwest` is built with `rustls-no-provider` so that aws-lc-rs — a second C
/// and assembly toolchain on the install path — stays out of the graph, and it
/// refuses to build a client until a provider is installed. Every client in the
/// process draws on this one, including the clients rig builds inside its own
/// provider clients, which no caller here routes through a builder. So this is
/// a process-level choke point rather than an argument threaded to each client.
///
/// Idempotent, and safe to call from any entry point: a second call, or a race
/// lost to another installer, leaves the process with a provider either way.
pub fn install_crypto_provider() {
    static INSTALLED: std::sync::Once = std::sync::Once::new();
    INSTALLED.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

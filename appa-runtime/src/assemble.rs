//! Config → authority backends, for the SDK session.
//!
//! A deliberate, small duplication of the assembly in `appa-runtime`'s `Runtime::with_options`
//! (which is `pub(crate)` there): the descriptors are public config, the backends are public
//! `external` types, only the wiring between them is re-stated here. Sanitizer and cast backends
//! are not assembled — [`crate::CallSession::open`] rejects policies that declare them.

use std::collections::BTreeMap;
use std::time::Duration;

use crate::config::{AuthorityImpl, Config};
use crate::external::{AuthorityBackend, BuiltinAuthority};
use crate::tool::HttpClient;
use appa_engine::names::AuthorityName;

/// Build the authority backends for every registered authority. Absence of an implementation is
/// fail-closed HITL, exactly as the runtime assembles it.
pub(crate) fn authority_backends(config: &Config) -> BTreeMap<AuthorityName, AuthorityBackend> {
    let client = HttpClient::new();
    let mut backends = BTreeMap::new();
    for authority in config.registry().authorities() {
        let backend = match config.authority_impl(&authority.name) {
            Some(AuthorityImpl::Builtin(builtin)) => AuthorityBackend::Builtin(*builtin),
            Some(AuthorityImpl::HttpResolver { url, timeout_ms }) => AuthorityBackend::Http {
                url: url.clone(),
                timeout: Duration::from_millis(*timeout_ms),
                client: client.clone(),
            },
            None => AuthorityBackend::Builtin(BuiltinAuthority::Hitl),
        };
        backends.insert(authority.name.clone(), backend);
    }
    backends
}

//! The `appa` integration suites, linked into one test binary. Every
//! separate test binary links the whole runtime again, so one binary holds
//! the link time and the `target/` size of the suites to one copy. A suite
//! that sets the process environment or counts what the process spawns
//! keeps its own binary (see the package manifest), because every suite here
//! would reach what it sets or add to what it counts.

mod common;

// Drives the server stack: without `daemon` there is nothing to drive.
#[cfg(feature = "daemon")]
mod annotate_cli;
mod api_shape;
mod archestra_annotator;
mod archestra_policy;
mod audience;
mod audit_read;
mod batteries;
mod claude_code_bash;
mod claude_code_redaction;
mod claude_code_subagent;
mod cloudflare_policy;
mod config_reload;
mod crash_recovery;
mod declare_ahead_replicas;
mod describe_cli;
mod docs_examples;
mod embedded_host;
mod embedded_peer;
mod examples_load;
mod file_tracker;
mod file_tracker_safebash;
mod grain_policy;
mod guide_parity;
mod guide_skill;
mod modules;
// Drives the server stack: without `daemon` there is nothing to drive.
#[cfg(feature = "daemon")]
mod hitl_elicitation;
mod hook_client;
mod host_aliases;
mod huggingface_policy;
mod init_cli;
mod input_substitution;
mod kagent_fixture_wire;
mod kagent_returns;
mod launchdarkly_policy;
mod linear_policy;
mod marketplace;
// Drives the server stack: without `daemon` there is nothing to drive.
#[cfg(feature = "daemon")]
mod marketplace_cli;
mod microsoft_learn_policy;
mod monday_policy;
mod notion_policy;
mod pagerduty_policy;
mod peer_messages;
mod policy_revisions;
mod posthog_policy;
mod protected_resume;
mod real_claude;
// Drives the server stack: without `daemon` there is nothing to drive.
#[cfg(feature = "daemon")]
mod replay;
mod rewrite_selects_contract;
mod runtime_stop;
// Drives the server stack: without `daemon` there is nothing to drive.
#[cfg(feature = "daemon")]
mod sanitize_cli;
mod sentry_policy;
mod session_principal;
mod settings_hooks;
mod spawn_declaration;
mod tool_aliases;
mod ui_runtime;
mod unbound_authority;
mod xmemory_policy;

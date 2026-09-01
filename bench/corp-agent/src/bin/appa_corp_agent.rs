//! `appa-corp-agent`: the corporate assistant on the runtime.
//!
//! The runtime runs no loop and holds no transcript. This binary is the
//! harness: it owns inference, the transcript, the tool catalogue and the
//! budget, and asks the runtime one question in front of every flow. Branching
//! is live — the policy's `fork` tool opens a child trajectory, and the child's
//! final message is its return, checked at the merge.
//!
//! Everything the deployment needs on loopback runs on one listener
//! ([`corporate_agent_demo::shim`]): the corp tools, executed in-process by the
//! same `corp-systems` code the MCP server wraps, and the `pii-redactor`
//! sanitizer, which is not a stock builtin.
//!
//! ```sh
//! appa-corp-agent --policy ../../bench/corp/policies/appa.toml "Summarise the open deploy tickets"
//! appa-corp-agent --policy ../../bench/corp/policies/appa.toml --max-forks 0 "..."   # no-branching ablation
//! ```
//!
//! Needs an OpenRouter key: `--api-key`, `OPENROUTER_API_KEY`, or a `.env` file.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use appa_example_agent::wire::WireMessage;
use appa_example_agent::{
    Agent, ArgumentKey, Limits, OpenAiCompatible, OpenAiConfig, Outcome, SpawnTool, ToolCatalogue, ToolName, ToolShim,
    TranscriptHead,
};
use appa_runtime::api::{AuditEntry, AuditEvent, AuditLabel, DispatchOutcome, Runtime, TrajectoryId};
use appa_runtime::config::{AnnotatorImplementation, Config, Endpoint, Implementation};
use clap::Parser;
use corp_systems::systems::System;
use corporate_agent_demo::shim::{self, CorpWorld};
use corporate_agent_demo::{catalogue, clean_key, load_dotenv, resolve_data_root, resolve_sink_root};
use tokio_util::sync::CancellationToken;

/// This host's transcript head (`CFG-18`). Held as a file so the bytes stay exactly what the
/// policy files carried before the head moved out of them.
const SYSTEM_PROMPT: &str = include_str!("../system_prompt.txt");

/// A shipped policy names an external it does not host on loopback port 0: a
/// loadable URL no listener can own. Whoever hosts the external rewrites the
/// origin and keeps the path.
const UNBOUND_ORIGIN: &str = "http://127.0.0.1:0";

fn system_prompt_with_addendum(addendum: Option<&str>) -> String {
    match addendum {
        Some(addendum) if !addendum.trim().is_empty() => {
            format!("{}\n\n{}", SYSTEM_PROMPT.trim_end(), addendum.trim())
        }
        _ => SYSTEM_PROMPT.to_owned(),
    }
}

fn system_prompt() -> String {
    let addendum = std::env::var("APPA_AGENT_PROMPT_ADDENDUM").ok();
    system_prompt_with_addendum(addendum.as_deref())
}

#[derive(Parser)]
#[command(about = "The corporate assistant on the runtime (branching live), tools in-process")]
struct Args {
    /// The task for the agent.
    prompt: String,

    /// OpenRouter model id.
    #[arg(long, env = "APPA_DEMO_MODEL", default_value = "openai/gpt-5.6-luna")]
    model: String,

    /// OpenRouter API key. Falls back to $OPENROUTER_API_KEY or a `.env` file.
    #[arg(long, env = "OPENROUTER_API_KEY")]
    api_key: Option<String>,

    /// Corpus root the in-process systems read and create under. Defaults to
    /// `CORP_DATA_ROOT`, else the sibling `corp-systems` crate's `data/`.
    #[arg(long)]
    data_root: Option<PathBuf>,

    /// Where `send_email` writes its `email/` folder. Defaults to
    /// `CORP_SINK_ROOT`, else this crate's `data/`.
    #[arg(long)]
    sink_root: Option<PathBuf>,

    /// The APPA policy file. Required: the policy is the deployment — the
    /// bench arms run the ones under `bench/corp/policies/`.
    #[arg(long)]
    policy: PathBuf,

    /// Fork budget for the whole run; 0 disables branching entirely.
    #[arg(long, default_value_t = 8)]
    max_forks: u32,

    /// How deep the trajectory family may nest (1 = the root may fork once,
    /// children may not re-fork).
    #[arg(long, default_value_t = 1)]
    max_fork_depth: u32,

    /// Suppress the mediation log; print only the final answer.
    #[arg(long)]
    quiet: bool,

    /// Optional machine-readable terminal status for an embedding harness.
    #[arg(long)]
    status_file: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let dotenv = load_dotenv();
    let args = Args::parse();
    if !args.quiet {
        install_decision_log();
        if let Some(path) = &dotenv {
            eprintln!("loaded env from {}", path.display());
        }
    }

    let api_key = args
        .api_key
        .as_deref()
        .map(clean_key)
        .filter(|key| !key.is_empty())
        .context(
            "no OpenRouter API key: pass --api-key, set OPENROUTER_API_KEY, or add it to a .env file \
             (see .env.example)",
        )?;

    let policy_path = args.policy;
    let mut config = Config::load(&policy_path).with_context(|| format!("loading {}", policy_path.display()))?;
    // The same compile `Runtime::open` performs, done here too because the
    // catalogue is read off the registry. Same input, so the two cannot
    // disagree; running it first turns a bad policy into an error before any
    // listener or database exists.
    let compiled = appa_policy::Config::from_toml_str(
        &toml::to_string(config.policy_file().value()).context("re-rendering the policy table")?,
    )
    .with_context(|| format!("compiling the policy in {}", policy_path.display()))?;

    let enabled = match std::env::var("CORP_ENABLED_SYSTEMS") {
        Ok(list) if !list.trim().is_empty() => System::parse_list(&list).context("parsing CORP_ENABLED_SYSTEMS")?,
        _ => System::ALL.into_iter().collect(),
    };
    let world = CorpWorld {
        data_root: resolve_data_root(args.data_root),
        sink_root: resolve_sink_root(args.sink_root),
        enabled,
    };
    let address = shim::serve(world).await.context("binding the loopback shim")?;
    let origin = format!("http://{address}");
    let hosted = bind_hosted_externals(&mut config, &origin);

    // One database per run. Each episode carries its own policy, and a
    // database belongs to one deployment — reopening it under a changed policy
    // is refused (`POS-10`) — so a shared file would be wrong as well as
    // stale.
    let state = tempfile::tempdir().context("creating the run's state directory")?;
    let runtime = Runtime::open(config, state.path().join("appa.db"), None).context("opening the deployment")?;

    let forking = args.max_forks > 0 && args.max_fork_depth > 0;
    if !args.quiet {
        eprintln!(
            "appa: policy {} — {} tools in-process at {origin}{}, {} hosted external(s), branching {}",
            policy_path.display(),
            compiled.registry_config().tools.len(),
            shim::TOOLS_PATH,
            hosted,
            if forking { "on" } else { "off" },
        );
    }

    // The transcript head is this host's configuration, not the policy's (`CFG-18`). The bytes are
    // the ones the policy files carried before they moved here, unchanged.
    let head = TranscriptHead::new(vec![WireMessage::system(system_prompt())]);
    let runtime = Arc::new(runtime);
    let mut agent = Agent::new(
        Arc::clone(&runtime),
        // A slow OpenRouter upstream can spend minutes on one completion, and
        // retrying a cut-off completion never helps — give each attempt room.
        OpenAiCompatible::new(
            OpenAiConfig::openrouter(args.model.clone(), api_key).with_request_timeout(Duration::from_secs(180)),
        ),
        ToolShim::new(format!("{origin}{}", shim::TOOLS_PATH)),
        ToolCatalogue::new(catalogue::advertised(&compiled, forking)),
    )
    .with_head(head)
    .with_limits(Limits {
        max_inference_rounds: 24,
        // Twice what v1 allowed, for the same episodes: an authorized
        // offer here names a call to propose again rather than running
        // it, so every acceptance costs a second call.
        max_tool_calls: 64,
        // Bounds runaway loops, not slow inference: with 180s per provider
        // attempt, a deadline near the old 240s would cut off episodes that
        // are merely waiting on a slow upstream.
        run_deadline: Duration::from_secs(900),
        max_forks: args.max_forks,
        max_fork_depth: args.max_fork_depth,
    });
    if forking {
        agent = agent.with_spawn_tool(SpawnTool {
            name: ToolName::new(catalogue::FORK),
            errand: ArgumentKey::new(catalogue::ERRAND),
        });
    }

    let root = TrajectoryId("appa-corp-agent".to_string());
    let outcome = agent.run(root.clone(), args.prompt, CancellationToken::new()).await;

    if !args.quiet {
        match runtime.audit(&root) {
            Some(entries) => replay(&entries),
            None => eprintln!("appa: the decision log could not be read"),
        }
    }

    if let Some(path) = &args.status_file {
        std::fs::write(
            path,
            serde_json::to_vec_pretty(&serde_json::json!({ "version": 1, "status": terminal_status(&outcome) }))?,
        )
        .with_context(|| format!("write terminal status to {}", path.display()))?;
    }

    match outcome {
        Outcome::Answer(text) => {
            println!("\n=== answer ===\n{text}");
            Ok(())
        }
        Outcome::BudgetFinalized { answer } => {
            if let Some(answer) = answer.filter(|answer| !answer.trim().is_empty()) {
                println!("\n=== answer ===\n{answer}");
            }
            Ok(())
        }
        // Nothing on stdout: a reader of this run's answer — the
        // benchmark included — takes the whole of stdout as the answer,
        // and an account of why there is none would read as one.
        Outcome::Stopped(reason) => anyhow::bail!("the run stopped: {reason}"),
    }
}

/// The typed status the benchmark reads. A provider that could not be reached
/// or answered past the deadline is `provider_failed`, which nothing in this
/// repository can fix; a provider that answered unusably — a rejected key, a
/// malformed body, no choices — is `provider_rejected`, a configuration or
/// contract fault the benchmark must surface.
fn terminal_status(outcome: &Outcome) -> &'static str {
    use appa_example_agent::{ProviderError, StopReason};
    match outcome {
        Outcome::Answer(_) => "completed",
        Outcome::BudgetFinalized { .. } => "budget_finalized",
        Outcome::Stopped(StopReason::InferenceFailed(
            ProviderError::Timeout { .. } | ProviderError::Transport { .. },
        )) => "provider_failed",
        Outcome::Stopped(StopReason::InferenceFailed(
            ProviderError::Status { .. } | ProviderError::Malformed { .. } | ProviderError::NoChoice { .. },
        )) => "provider_rejected",
        Outcome::Stopped(StopReason::Refused(_)) => "runtime_refused",
        Outcome::Stopped(StopReason::Cancelled) => "cancelled",
        Outcome::Stopped(StopReason::BudgetExhausted) => "budget_exhausted",
    }
}

/// Print the `appa::decision` events the agent emits, and nothing else: one
/// line per decision, already spelled, so the mediation log reads as prose
/// rather than as instrumented fields.
fn install_decision_log() {
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;

    let _ = tracing_subscriber::registry()
        .with(
            tracing_subscriber::fmt::layer()
                .with_writer(std::io::stderr)
                .without_time()
                .with_level(false)
                .with_target(false),
        )
        .with(tracing_subscriber::filter::Targets::new().with_target("appa::decision", tracing::Level::DEBUG))
        .try_init();
}

/// Point every still-unbound external this shim implements at `origin`,
/// keeping the path. Returns how many were bound. An unbound external the shim
/// does not implement is left alone: it then fails closed at its first consult,
/// which is the right answer for a component nobody is hosting.
fn bind_hosted_externals(config: &mut Config, origin: &str) -> usize {
    let externals = &mut config.externals;
    let annotators = externals
        .annotators
        .values_mut()
        .filter_map(|implementation| match implementation {
            AnnotatorImplementation::Resolver(endpoint) => Some(endpoint),
            AnnotatorImplementation::Command(_) => None,
        });
    let endpoints = externals
        .authorities
        .values_mut()
        .chain(externals.sanitizers.values_mut())
        .chain(externals.audience.values_mut())
        .chain(externals.identity.values_mut())
        .filter_map(|implementation| match implementation {
            Implementation::Resolver(endpoint) => Some(endpoint),
            Implementation::Builtin(_) | Implementation::Command(_) => None,
        })
        .chain(annotators);
    let mut bound = 0;
    for endpoint in endpoints {
        let Some(path) = endpoint.url.strip_prefix(UNBOUND_ORIGIN) else {
            continue;
        };
        if !shim::serves(path) {
            continue;
        }
        *endpoint = Endpoint::new(format!("{origin}{path}"), endpoint.token.clone());
        bound += 1;
    }
    bound
}

/// The engine's own record of what flowed, read back from the log after the
/// run. It is not the whole story and does not try to be: a call the engine
/// refused appended no fact, so the blocks are in the agent's decision lines
/// above, and this is what actually crossed.
fn replay(entries: &[AuditEntry]) {
    eprintln!("appa: --- the decision log ---");
    for entry in entries {
        let at = &entry.trajectory;
        match &entry.event {
            AuditEvent::Forked { parent, seed } => {
                eprintln!("appa: [{at}] forked from {parent} at {}", label(seed));
            }
            AuditEvent::Released {
                tool,
                label: at_label,
                effects,
            } => {
                eprintln!(
                    "appa: [{at}] released {tool} at {}{}",
                    label(at_label),
                    committing(effects)
                );
            }
            AuditEvent::EffectsCommitted { effects } => {
                eprintln!("appa: [{at}] committed [{}]", effects.join(", "));
            }
            AuditEvent::Closed { outcome } => {
                let closed = match outcome {
                    DispatchOutcome::Ran { effects } if effects.is_empty() => "ran".to_string(),
                    DispatchOutcome::Ran { effects } => format!("ran, committing [{}]", effects.join(", ")),
                    DispatchOutcome::Failed => "failed".to_string(),
                    DispatchOutcome::Unknown => "may or may not have run".to_string(),
                };
                eprintln!("appa: [{at}] dispatch {closed}");
            }
            AuditEvent::Admitted { label: at_label } => {
                eprintln!("appa: [{at}] value admitted at {}", label(at_label));
            }
            AuditEvent::Ruled { authority } => {
                eprintln!("appa: remedy authorized [{at}]: ruling by {authority}");
            }
            AuditEvent::Denied { authority } => {
                eprintln!("appa: [{at}] {authority} denied this call");
            }
            AuditEvent::Narrowed { from, to } => {
                eprintln!(
                    "appa: remedy authorized [{at}]: narrowing accepted {} -> {}",
                    label(from),
                    label(to)
                );
            }
            AuditEvent::SanitizerBound { sanitizer } => {
                eprintln!("appa: [{at}] the raw result is withheld for {sanitizer}");
            }
            AuditEvent::Sanitized { sanitizer } => {
                eprintln!("appa: remedy authorized [{at}]: the {sanitizer} derivation was admitted");
            }
            AuditEvent::ChildReturn {
                sanitizer: None,
                label: at_label,
            } => {
                eprintln!("appa: [{at}] child return crossed raw at {}", label(at_label));
            }
            AuditEvent::ChildReturn {
                sanitizer: Some(name),
                label: at_label,
            } => {
                eprintln!(
                    "appa: remedy authorized [{at}]: child return crossed as the {name} derivation at {}",
                    label(at_label)
                );
            }
            AuditEvent::Merged => eprintln!("appa: [{at}] merged a child return"),
            AuditEvent::VoidReturn => eprintln!("appa: [{at}] ended with a void return"),
        }
    }
}

fn label(label: &AuditLabel) -> String {
    format!("trust={} audience={}", label.trust, label.audience)
}

fn committing(effects: &[String]) -> String {
    match effects.is_empty() {
        true => String::new(),
        false => format!(", committing [{}] on success", effects.join(", ")),
    }
}

#[cfg(test)]
mod tests {
    use super::{Args, SYSTEM_PROMPT, system_prompt_with_addendum};
    use clap::Parser;

    /// The exact argv `bench_corp.agents.command_for` builds, for both the
    /// branching arm and the ablation. The bench adds no flags of its own, so
    /// a renamed or dropped flag here breaks every appa episode at once.
    #[test]
    fn the_benchs_invocation_parses() {
        let base = [
            "appa-corp-agent",
            "summarise the open deploy tickets",
            "--model",
            "openai/gpt-5.6-luna",
            "--data-root",
            "/episode/data",
            "--sink-root",
            "/episode/sink",
            "--policy",
            "/episode/policy.toml",
        ];
        let args = Args::try_parse_from(base).expect("the guarded arm's invocation parses");
        assert_eq!(args.prompt, "summarise the open deploy tickets");
        assert_eq!(args.policy, std::path::PathBuf::from("/episode/policy.toml"));
        assert!(args.max_forks > 0, "branching is on unless the ablation turns it off");
        assert!(!args.quiet, "the bench reads the mediation log off stderr");

        let ablation = Args::try_parse_from(base.iter().copied().chain(["--max-forks", "0"]))
            .expect("the ablation arm's invocation parses");
        assert_eq!(ablation.max_forks, 0);
    }

    #[test]
    fn prompt_addendum_is_separate_and_standard_is_unchanged() {
        assert_eq!(system_prompt_with_addendum(None), SYSTEM_PROMPT);
        assert_eq!(
            system_prompt_with_addendum(Some("  test pressure  ")),
            format!("{}\n\ntest pressure", SYSTEM_PROMPT.trim_end())
        );
    }

    /// Only a provider that could not be reached in time is a provider
    /// fault the bench forgives; a provider that answered unusably is a
    /// fault of this side of the wire and stays red.
    #[test]
    fn a_provider_that_answered_unusably_is_not_a_provider_fault() {
        use appa_example_agent::{Outcome, ProviderError, StopReason};
        use std::time::Duration;
        let stopped = |error: ProviderError| Outcome::Stopped(StopReason::InferenceFailed(error));
        assert_eq!(
            super::terminal_status(&stopped(ProviderError::Timeout {
                attempts: 3,
                timeout: Duration::from_secs(1),
            })),
            "provider_failed"
        );
        assert_eq!(
            super::terminal_status(&stopped(ProviderError::Transport { attempts: 3 })),
            "provider_failed"
        );
        assert_eq!(
            super::terminal_status(&stopped(ProviderError::Status { code: 401, attempts: 1 })),
            "provider_rejected"
        );
        assert_eq!(
            super::terminal_status(&stopped(ProviderError::Malformed { attempts: 1 })),
            "provider_rejected"
        );
        assert_eq!(
            super::terminal_status(&stopped(ProviderError::NoChoice { attempts: 1 })),
            "provider_rejected"
        );
    }
}

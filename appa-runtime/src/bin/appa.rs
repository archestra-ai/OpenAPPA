use std::path::PathBuf;
use std::process::ExitCode;
use std::{env, ffi::OsString, iter};

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "appa", version)]
struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Describe the configuration facts available to a human or configuring agent.
    Describe {
        #[arg(long, env = "APPA_CONFIG")]
        config: Option<PathBuf>,

        #[arg(long, value_enum, default_value_t = Adapter::ClaudeCode)]
        adapter: Adapter,
    },

    /// Initialize OpenAPPA for an agent harness.
    Init {
        #[command(subcommand)]
        harness: Harness,
    },

    /// Post one harness hook event to the running runtime.
    #[command(hide = true)]
    Hook {
        #[arg(long, env = "APPA_RUNTIME_URL", default_value = "http://127.0.0.1:8787")]
        url: String,

        /// Report a finished turn, whose answer never blocks the harness.
        #[arg(long)]
        turn_end: bool,
    },
}

#[derive(Clone, Copy, clap::ValueEnum)]
enum Adapter {
    ClaudeCode,
    Kagent,
}

impl Adapter {
    fn as_str(self) -> &'static str {
        match self {
            Self::ClaudeCode => "claude-code",
            Self::Kagent => "kagent",
        }
    }
}

#[derive(Subcommand)]
enum Harness {
    /// Install this build's Claude Code plugin and initialize its local deployment.
    ClaudeCode {
        /// Developer override: a staged marketplace root to deploy.
        #[arg(long, hide = true)]
        plugin_source: Option<String>,
    },
}

fn main() -> ExitCode {
    if env::args_os().nth(1).as_deref() == Some(std::ffi::OsStr::new("runtime")) {
        let args = iter::once(OsString::from("appa runtime")).chain(env::args_os().skip(2));
        return appa_runtime::runtime_cli::run_from(args);
    }

    match Args::parse().command {
        Command::Hook { url, turn_end } => appa_runtime::hook_client::run(&url, turn_end),
        Command::Describe { config, adapter } => {
            let config = config.unwrap_or_else(appa_runtime::init::installed_config_path);
            print!("{}", appa_runtime::describe::render(&config, adapter.as_str()));
            ExitCode::SUCCESS
        }
        Command::Init {
            harness: Harness::ClaudeCode { plugin_source },
        } => match appa_runtime::init::claude_code(plugin_source.as_deref()) {
            Ok(description) => {
                print!("{description}");
                ExitCode::SUCCESS
            }
            Err(error) => {
                eprintln!("appa: {error}");
                ExitCode::FAILURE
            }
        },
    }
}

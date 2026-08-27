use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "appa", version = include_str!("../../../version.txt").trim())]
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
}

#[derive(Clone, Copy, clap::ValueEnum)]
enum Adapter {
    ClaudeCode,
}

impl Adapter {
    fn as_str(self) -> &'static str {
        match self {
            Self::ClaudeCode => "claude-code",
        }
    }
}

#[derive(Subcommand)]
enum Harness {
    /// Install this build's Claude Code plugin and initialize its local deployment.
    ClaudeCode {
        /// Marketplace source. Defaults to this checkout/package, then the OpenAPPA repository.
        #[arg(long)]
        source: Option<String>,
    },
}

fn main() -> ExitCode {
    match Args::parse().command {
        Command::Describe { config, adapter } => {
            let config = config.unwrap_or_else(appa_runtime::init::installed_config_path);
            print!("{}", appa_runtime::describe::render(&config, adapter.as_str()));
            ExitCode::SUCCESS
        }
        Command::Init {
            harness: Harness::ClaudeCode { source },
        } => match appa_runtime::init::claude_code(source.as_deref()) {
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

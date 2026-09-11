//! Constrained headless Claude Code launcher for runtime-owned file tools.
//!
//! Native tools are removed, not denied by a late hook. Bare mode disables implicit
//! project instructions, memory, plugins and MCP discovery. Claude starts in a private
//! empty directory; tracked files remain behind the runtime. No caller-supplied Claude
//! flags, additional directories, session resumes or customizations are forwarded.
//! This fixes the native Edit prevalidation boundary; it is not an OS sandbox.

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

#[derive(Debug, clap::Args)]
pub struct Args {
    #[arg(long, default_value = "http://127.0.0.1:8787")]
    runtime_url: String,
    /// Bound API spending for this headless run.
    #[arg(long)]
    max_budget_usd: Option<f64>,
    /// One host-supplied prompt. No Claude flags or unclassified file attachments.
    prompt: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, clap::Args)]
pub(crate) struct Deployment {
    #[arg(long)]
    pub config: PathBuf,
    #[arg(long)]
    pub db: PathBuf,
    #[arg(long)]
    pub workspace: PathBuf,
    #[arg(long)]
    pub ledger: PathBuf,
}

#[derive(Debug, clap::Args)]
pub struct ServeArgs {
    #[command(flatten)]
    deployment: Deployment,
    #[arg(long)]
    trajectory: uuid::Uuid,
}

pub fn serve(args: ServeArgs) -> ExitCode {
    crate::tls::install_crypto_provider();
    let result = (|| {
        let deployment = args.deployment;
        let config = crate::config::Config::load(&deployment.config).map_err(|error| error.to_string())?;
        let runtime =
            crate::api::Runtime::open_served(config, deployment.db, None, appa_adapter_claude_code::adapter())
                .map_err(|error| error.to_string())?
                .with_file_tracking(deployment.workspace, deployment.ledger, None)
                .map_err(|error| error.to_string())?;
        let actor = appa_runtime_api::Actor {
            root: appa_runtime_api::TrajectoryId(format!("cc:{}", args.trajectory)),
            child: None,
        };
        match runtime.create_session(actor.root.clone()) {
            Ok(_) | Err(crate::api::EventError::TrajectoryExists) => {}
            Err(error) => return Err(error.to_string()),
        }
        tokio::runtime::Runtime::new()
            .map_err(|error| error.to_string())?
            .block_on(crate::mcp::serve_files(std::sync::Arc::new(runtime), actor))
    })();
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("appa file-mcp: {error}");
            ExitCode::FAILURE
        }
    }
}

pub fn run(args: Args) -> ExitCode {
    match launch(args) {
        Ok(status) if status.success() => ExitCode::SUCCESS,
        Ok(_) => ExitCode::FAILURE,
        Err(error) => {
            eprintln!("appa claude-files: {error}");
            ExitCode::FAILURE
        }
    }
}

fn endpoint(input: &str) -> Result<url::Url, String> {
    let url = url::Url::parse(input).map_err(|error| error.to_string())?;
    let loopback = match url.host() {
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        _ => false,
    };
    if url.scheme() != "http"
        || !loopback
        || url.path() != "/"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err("runtime URL must be a plain HTTP loopback IP with no path, credentials, query or fragment".into());
    }
    Ok(url)
}

fn command(binary: &Path, deployment: &Deployment, cwd: &Path, args: &Args) -> Result<Command, String> {
    let trajectory = uuid::Uuid::new_v4().to_string();
    let mut server_args = vec!["file-mcp".to_string(), "--trajectory".into(), trajectory.clone()];
    for (flag, path) in [
        ("--config", &deployment.config),
        ("--db", &deployment.db),
        ("--workspace", &deployment.workspace),
        ("--ledger", &deployment.ledger),
    ] {
        server_args.push(flag.into());
        server_args.push(path.to_str().ok_or("deployment paths must be UTF-8")?.to_string());
    }
    let mcp = serde_json::json!({"mcpServers":{"plugin_appa-runtime_appa":{
        "command":binary.to_str().ok_or("executable path must be UTF-8")?, "args":server_args
    }}});
    let mut command = Command::new("claude");
    command.current_dir(cwd)
        .args(["--bare", "--print", "--tools", "", "--strict-mcp-config", "--mcp-config"])
        .arg(mcp.to_string())
        .args(["--settings", "{\"autoMemoryEnabled\":false}"])
        .args(["--allowedTools", "mcp__plugin_appa-runtime_appa__appa_read_file,mcp__plugin_appa-runtime_appa__appa_write_file,mcp__plugin_appa-runtime_appa__appa_edit_file,mcp__plugin_appa-runtime_appa__appa_copy_file,mcp__plugin_appa-runtime_appa__appa_move_file,mcp__plugin_appa-runtime_appa__execute_remedy_plan"])
        .args(["--no-session-persistence", "--output-format", "stream-json", "--verbose", "--session-id"])
        .arg(trajectory)
        .env("ENABLE_TOOL_SEARCH", "false")
        .env("CLAUDE_CODE_DISABLE_AUTO_MEMORY", "1");
    if let Some(budget) = args.max_budget_usd {
        command.arg("--max-budget-usd").arg(budget.to_string());
    }
    command.arg("--").arg(&args.prompt);
    Ok(command)
}

fn launch(args: Args) -> Result<std::process::ExitStatus, String> {
    if !cfg!(unix) {
        return Err("the file-tools launcher currently requires Unix".into());
    }
    if args
        .max_budget_usd
        .is_some_and(|budget| !budget.is_finite() || budget <= 0.0)
    {
        return Err("max-budget-usd must be positive and finite".into());
    }
    let url = endpoint(&args.runtime_url)?;
    let base = url.as_str().trim_end_matches('/');
    crate::tls::install_crypto_provider();
    let runtime = tokio::runtime::Runtime::new().map_err(|error| error.to_string())?;
    let deployment: Deployment = runtime.block_on(async {
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(std::time::Duration::from_secs(5))
            .build()
            .map_err(|error| error.to_string())?;
        let response = client
            .get(format!("{base}/file-tools"))
            .send()
            .await
            .map_err(|error| error.to_string())?;
        response
            .error_for_status()
            .map_err(|error| error.to_string())?
            .json()
            .await
            .map_err(|error| error.to_string())
    })?;
    let private = tempfile::tempdir().map_err(|error| error.to_string())?;
    let cwd = private.path().join("session");
    std::fs::create_dir(&cwd).map_err(|error| error.to_string())?;
    let binary = std::env::current_exe().map_err(|error| error.to_string())?;
    let mut command = command(&binary, &deployment, &cwd, &args)?;
    command.status().map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launcher_removes_native_tools_and_implicit_discovery() {
        let launch = Args {
            runtime_url: "http://127.0.0.1:8787".into(),
            max_budget_usd: Some(1.0),
            prompt: "--tools Bash".into(),
        };
        let deployment = Deployment {
            config: "/host/policy.toml".into(),
            db: "/host/runtime.db".into(),
            workspace: "/work".into(),
            ledger: "/host/files.db".into(),
        };
        let command = command(
            Path::new("/host/appa"),
            &deployment,
            Path::new("/private/session"),
            &launch,
        )
        .unwrap();
        let args: Vec<_> = command.get_args().map(|s| s.to_str().unwrap()).collect();
        assert!(args.windows(2).any(|pair| pair == ["--tools", ""]));
        assert!(args.contains(&"--bare"));
        assert!(args.contains(&"--strict-mcp-config"));
        assert!(args.contains(&"--no-session-persistence"));
        assert_eq!(&args[args.len() - 2..], &["--", "--tools Bash"]);
        assert_eq!(command.get_current_dir(), Some(Path::new("/private/session")));
        for bad in [
            "https://127.0.0.1:8787",
            "http://example.com",
            "http://127.0.0.1/x",
            "http://u@127.0.0.1",
            "http://127.0.0.1/#x",
        ] {
            assert!(endpoint(bad).is_err());
        }
        assert!(endpoint("http://127.0.0.1:8787").is_ok());
    }
}

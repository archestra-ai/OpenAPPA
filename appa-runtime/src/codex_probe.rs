//! A narrow HTTP handshake for a process actually running inside Codex's command sandbox.

use std::process::ExitCode;
use std::time::Duration;

use crate::loopback_http::{Deadline, Endpoint, request_for_sandboxed_wrapper};

pub fn run(url: &str) -> ExitCode {
    let result = Endpoint::parse(url).and_then(|endpoint| {
        let proxy = std::env::var("HTTP_PROXY")
            .or_else(|_| std::env::var("http_proxy"))
            .ok();
        request_for_sandboxed_wrapper(
            &endpoint,
            "GET",
            "/health",
            b"",
            &Deadline::spanning(Duration::from_secs(3)),
            proxy.as_deref(),
        )
    });
    match result {
        Ok(answer) if answer.is_success() && answer.body == b"ok" => {
            println!("ok");
            ExitCode::SUCCESS
        }
        Ok(answer) => {
            eprintln!("appa codex probe: the runtime returned status {}", answer.status);
            ExitCode::FAILURE
        }
        Err(error) => {
            eprintln!("appa codex probe: {error}");
            ExitCode::FAILURE
        }
    }
}

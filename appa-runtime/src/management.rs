//! Fixed subprocess boundary for vouched runtime management tools.

use tokio::io::AsyncWriteExt;

pub(crate) async fn run<T: serde::Serialize>(command: &str, input: Option<&T>) -> Result<String, String> {
    let mut process = tokio::process::Command::new(format!("/usr/local/bin/{command}"));
    process
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    let mut child = process
        .spawn()
        .map_err(|error| format!("cannot start {command}: {error}"))?;
    if let Some(input) = input {
        let bytes = serde_json::to_vec(input).map_err(|error| format!("cannot encode {command} input: {error}"))?;
        child
            .stdin
            .take()
            .expect("a piped management stdin exists")
            .write_all(&bytes)
            .await
            .map_err(|error| format!("cannot write {command} input: {error}"))?;
    }
    let output = tokio::time::timeout(std::time::Duration::from_secs(300), child.wait_with_output())
        .await
        .map_err(|_| format!("{command} exceeded 300 seconds"))?
        .map_err(|error| format!("cannot wait for {command}: {error}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    String::from_utf8(output.stdout).map_err(|error| format!("{command} output is not UTF-8: {error}"))
}

#![cfg(unix)]

use std::io::{Read, Write};
use std::net::TcpListener;
use std::process::{Command, Stdio};
use std::sync::mpsc;

use crate::common::RefusingPort;

/// `appa yell words </dev/null` and `echo words | appa yell` have nobody to answer the share
/// question, so nothing is sent and the report stays on disk.
#[test]
fn a_closed_stdin_sends_nothing() {
    let receiver = TcpListener::bind("127.0.0.1:0").expect("an ephemeral port binds");
    let endpoint = format!(
        "http://{}/",
        receiver.local_addr().expect("the listener has an address")
    );
    let (contacted, contacts) = mpsc::channel();
    std::thread::spawn(move || {
        for mut stream in receiver.incoming().flatten() {
            let _ = contacted.send(());
            let mut request = [0_u8; 4096];
            let _ = stream.read(&mut request);
            let _ = stream.write_all(b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
        }
    });
    let runtime = RefusingPort::new();
    let cases: [(&[&str], &[u8]); 2] = [(&["the", "hook", "blocked"], b""), (&[], b"the hook blocked\n")];
    for (words, stdin) in cases {
        let scratch = tempfile::tempdir().expect("temporary directory");
        let mut child = Command::new(env!("CARGO_BIN_EXE_appa"))
            .args(["yell", "--url", &runtime.url()])
            .args(words)
            .env("APPA_YELL_ENDPOINT", &endpoint)
            .env("TMPDIR", scratch.path())
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("appa runs");
        let mut input = child.stdin.take().expect("stdin is piped");
        input.write_all(stdin).expect("appa reads stdin");
        drop(input);
        let status = child.wait().expect("appa exits");
        assert!(status.success(), "{words:?}");
        assert_eq!(contacts.try_recv(), Err(mpsc::TryRecvError::Empty), "{words:?}");
        let reports: Vec<_> = std::fs::read_dir(scratch.path())
            .expect("the scratch directory is readable")
            .map(|entry| entry.expect("a directory entry").path().join("report.json"))
            .collect();
        assert!(matches!(reports.as_slice(), [report] if report.is_file()), "{words:?}");
    }
}

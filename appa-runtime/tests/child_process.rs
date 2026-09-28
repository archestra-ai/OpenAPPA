//! Children started at once through `child_process` inherit none of each other's pipes.
//! A binary of its own: another suite's spawns would add their own moments in flight.
#![cfg(target_vendor = "apple")]

use std::io::Write as _;
use std::process::{Command, Stdio};
use std::sync::Barrier;

use appa_runtime::child_process;

const THREADS: usize = 8;
const ROUNDS: usize = 200;

/// Each round, eight threads start a child at once and drop the reader of its stdout.
/// Once every spawn has returned, each tells its child to write, so the write must fail
/// unless another child inherited that reader and still holds it.
///
/// The writes wait for the spawns: a spawn in flight holds a copy of every open file
/// until its exec closes the close-on-exec ones, and a write in that moment would land.
/// That copy is gone before the spawn returns; the copy this test looks for is kept for
/// the child's whole life.
#[test]
fn a_child_whose_reader_is_gone_cannot_write_while_others_start() {
    let spawned = Barrier::new(THREADS);
    let written: usize = std::thread::scope(|scope| {
        let threads: Vec<_> = (0..THREADS)
            .map(|_| {
                scope.spawn(|| {
                    (0..ROUNDS)
                        .filter(|_| {
                            let mut child = child_process::spawn(
                                Command::new("/bin/sh")
                                    .args(["-c", "read go && echo written"])
                                    .stdin(Stdio::piped())
                                    .stdout(Stdio::piped())
                                    .stderr(Stdio::null()),
                            )
                            .expect("sh starts");
                            drop(child.stdout.take());
                            spawned.wait();
                            let mut go = child.stdin.take().expect("stdin is piped");
                            go.write_all(b"go\n").expect("the child reads its go");
                            drop(go);
                            let wrote = child.wait().expect("sh exits").success();
                            spawned.wait();
                            wrote
                        })
                        .count()
                })
            })
            .collect();
        threads
            .into_iter()
            .map(|thread| thread.join().expect("a spawning thread finishes"))
            .sum()
    });
    assert_eq!(
        written,
        0,
        "{written} of {} children wrote to a reader their parent had dropped",
        THREADS * ROUNDS
    );
}

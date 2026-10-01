//! Stopping the pager (Ctrl-Z, SIGTSTP) stops its whole job, as Ctrl-Z in
//! a cooked terminal does: whatever started emde in the same process group
//! and waits for it (a script, `sh -c`, `cargo run`) stops too, so the shell
//! gets the terminal back. And [`stop_process`] returns only once the job
//! was continued, never while the stop is still on its way.
//!
//! The test runs itself again as a child in a process group of its own (a
//! stand-in for a job started by a shell), with a second member that only
//! waits (`sleep`), and has the child call [`stop_process`] from a thread
//! other than its main one (the stop may then land a moment late).

#![cfg(unix)]

use std::io::{BufRead as _, BufReader, Write as _};
use std::os::unix::process::CommandExt as _;
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use emde::pager::term::{job_control, stop_process};
use rustix::process::{Pid, Signal, WaitOptions, kill_process_group, waitpid};

/// Set for the child: run its part of the test.
const CHILD: &str = "EMDE_TEST_STOP_CHILD";

/// How long anything may take.
const PATIENCE: Duration = Duration::from_secs(20);

/// Tell the parent `what`, on a line of its own starting with `@` (the
/// test harness may have started a line already).
fn say(what: &str) {
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "\n@{what}");
    let _ = out.flush();
}

/// The child's part (nothing in a normal run): start a second member of
/// the job, then stop as the pager does.
#[test]
fn stop_child() {
    if std::env::var_os(CHILD).is_none() {
        return;
    }
    assert!(job_control(), "a process group of its own in the session");
    let mut sibling = Command::new("sleep").arg("60").spawn().unwrap();
    say(&format!("sibling {}", sibling.id()));
    stop_process().unwrap();
    // The SIGSTOP landed before stop_process returned: this line is only
    // ever written after the job was continued.
    say("continued");
    let _ = sibling.kill();
    let _ = sibling.wait();
}

/// Kills the job however the test ends.
struct Job(Pid);

impl Drop for Job {
    fn drop(&mut self) {
        let _ = kill_process_group(self.0, Signal::KILL);
    }
}

/// The state letter of process `pid` (`T`: stopped), from `/proc` or `ps`.
fn state(pid: u32) -> Option<char> {
    if let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        // `pid (comm) S …`: the state follows the last `)`.
        return stat.rsplit_once(')')?.1.trim_start().chars().next();
    }
    let out = Command::new("ps")
        .args(["-o", "stat=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    String::from_utf8_lossy(&out.stdout).trim().chars().next()
}

/// Wait until `done` holds.
fn wait_for(what: &str, mut done: impl FnMut() -> bool) {
    let until = Instant::now() + PATIENCE;
    while !done() {
        assert!(Instant::now() < until, "timed out waiting: {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// The rest of the child's next `@name` line.
fn next(lines: &Receiver<String>, name: &str) -> String {
    let want = format!("@{name}");
    let until = Instant::now() + PATIENCE;
    loop {
        let left = until.saturating_duration_since(Instant::now());
        match lines.recv_timeout(left) {
            Ok(line) => {
                if let Some(rest) = line.strip_prefix(&want) {
                    return rest.trim().to_owned();
                }
            }
            Err(e) => panic!("no {want} line from the child: {e}"),
        }
    }
}

#[test]
fn stopping_stops_the_whole_job() {
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["stop_child", "--exact", "--nocapture", "--test-threads=1"])
        .env(CHILD, "1")
        .process_group(0)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let job = Job(Pid::from_child(&child));
    let out = BufReader::new(child.stdout.take().unwrap());
    let (send, lines) = mpsc::channel();
    std::thread::spawn(move || {
        for line in out.lines().map_while(Result::ok) {
            let _ = send.send(line);
        }
    });
    let sibling: u32 = next(&lines, "sibling").parse().unwrap();
    // The child stops, and so does the other member of its job …
    wait_for("the child stops", || {
        waitpid(Some(job.0), WaitOptions::UNTRACED | WaitOptions::NOHANG)
            .ok()
            .flatten()
            .is_some_and(|(_, status)| status.stopped())
    });
    wait_for("the rest of the job stops", || state(sibling) == Some('T'));
    // … without a word from the child while it is stopped.
    std::thread::sleep(Duration::from_millis(100));
    assert!(lines.try_recv().is_err(), "the child ran on");
    // A shell continues the whole job (`fg`): stop_process returns.
    kill_process_group(job.0, Signal::CONT).unwrap();
    assert_eq!(next(&lines, "continued"), "");
    assert!(child.wait().unwrap().success());
}

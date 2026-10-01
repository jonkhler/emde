//! Running a helper program with a deadline: the `tmux display` query and
//! the `curl` fetch of remote images.
//!
//! The program gets no standard input and its standard error is dropped;
//! its standard output is read with `poll(2)` until it closes, so neither a
//! hung program nor an endless stream can stall emde.

use std::io::{self, Read};
use std::os::fd::AsRawFd;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use super::probe::wait_readable;

/// Run `cmd` and return its standard output.
///
/// `None` if it fails to start, exits unsuccessfully, prints more than
/// `max_output` bytes, or is still running after `limit`; a child that is
/// cut off is killed and reaped.
pub(crate) fn output_with_deadline(
    mut cmd: Command,
    limit: Duration,
    max_output: usize,
) -> Option<Vec<u8>> {
    let start = Instant::now();
    let deadline = start.checked_add(limit).unwrap_or(start);
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let output = read_stdout(&mut child, deadline, max_output).ok();
    let status = match output {
        Some(_) => wait_until(&mut child, deadline),
        None => None,
    };
    let Some(status) = status else {
        let _ = child.kill();
        let _ = child.wait();
        return None;
    };
    output.filter(|_| status.success())
}

/// Read the child's stdout until EOF, failing at the deadline or when it
/// grows past `max_output` bytes.
fn read_stdout(child: &mut Child, deadline: Instant, max_output: usize) -> io::Result<Vec<u8>> {
    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| io::Error::other("stdout not captured"))?;
    let mut out = Vec::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        if !wait_readable(stdout.as_raw_fd(), deadline)? {
            return Err(io::ErrorKind::TimedOut.into());
        }
        match stdout.read(&mut buf) {
            Ok(0) => return Ok(out),
            Ok(n) => {
                out.extend_from_slice(buf.get(..n).unwrap_or_default());
                if out.len() > max_output {
                    return Err(io::Error::other("output too long"));
                }
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
}

/// Wait for the child to exit, polling until the deadline.
fn wait_until(child: &mut Child, deadline: Instant) -> Option<ExitStatus> {
    loop {
        if let Some(status) = child.try_wait().ok()? {
            return Some(status);
        }
        let now = Instant::now();
        if now >= deadline {
            return None;
        }
        std::thread::sleep((deadline - now).min(Duration::from_millis(1)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ENOUGH: usize = 1024;

    #[test]
    fn deadline_kills_slow_commands() {
        let mut cmd = Command::new("sleep");
        cmd.arg("5");
        let start = Instant::now();
        assert_eq!(
            output_with_deadline(cmd, Duration::from_millis(50), ENOUGH),
            None
        );
        assert!(start.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn captures_output_of_fast_commands() {
        let mut cmd = Command::new("printf");
        cmd.arg("a|b\\n");
        let out = output_with_deadline(cmd, Duration::from_secs(2), ENOUGH).unwrap();
        assert_eq!(out, b"a|b\n");
    }

    #[test]
    fn failing_commands_are_none() {
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "echo 'partial'; exit 3"]);
        assert_eq!(
            output_with_deadline(cmd, Duration::from_secs(2), ENOUGH),
            None
        );
        let missing = Command::new("/nonexistent/emde-test");
        assert_eq!(
            output_with_deadline(missing, Duration::from_secs(2), ENOUGH),
            None
        );
    }

    #[test]
    fn endless_output_is_cut_off() {
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "yes emde"]);
        let start = Instant::now();
        assert_eq!(
            output_with_deadline(cmd, Duration::from_secs(5), 100_000),
            None
        );
        assert!(start.elapsed() < Duration::from_secs(4));
        // Exactly at the limit is fine.
        let mut cmd = Command::new("printf");
        cmd.arg("12345");
        assert_eq!(
            output_with_deadline(cmd, Duration::from_secs(2), 5).as_deref(),
            Some(&b"12345"[..])
        );
    }
}

//! The `run-process` host function: how a source runs a program it was granted, and nothing else.
//!
//! The granted names are matched exactly, there is no shell, the child sees only the environment the manifest listed
//! (plus `PATH`, to be found), and the run is bounded in time and in output.

use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// The most output of either stream a program may produce. A source has a memory limit of its own; this keeps one
/// chatty program from filling it with a single answer.
const MAX_OUTPUT_BYTES: u64 = 16 * 1024 * 1024;

/// What a program produced.
#[derive(Debug)]
pub(super) struct Output {
    pub status: i32,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

/// Why a program was not run or did not finish.
#[derive(Debug)]
pub(super) enum ProcessError {
    /// The program is not in the source's permissions. Reported to the user as a permission error, not a failure.
    Denied(String),
    /// It was permitted but could not be run to completion.
    Failed(String),
}

/// What a source may run, and with what.
#[derive(Debug, Clone, Default)]
pub(super) struct ProcessGrant {
    /// The program names that may be run.
    pub allowed: Vec<String>,
    /// The environment variables the child may see, resolved from the daemon's own environment.
    pub env: Vec<(String, String)>,
    /// How long one run may take.
    pub timeout: Duration,
}

/// Run `program` if `grant` allows it.
pub(super) fn run(
    grant: &ProcessGrant,
    program: &str,
    args: &[String],
    stdin: Option<&[u8]>,
) -> Result<Output, ProcessError> {
    if !grant.allowed.iter().any(|allowed| allowed == program) {
        return Err(ProcessError::Denied(format!(
            "running `{program}` is not permitted; its manifest allows: {}",
            if grant.allowed.is_empty() {
                "nothing".to_string()
            } else {
                grant.allowed.join(", ")
            }
        )));
    }
    let mut command = Command::new(program);
    command
        .args(args)
        .env_clear()
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // `PATH` so the program is found, `HOME` (and `SystemRoot` on Windows) so tools find their own configuration.
    // No other variable is inherited, so a token in the daemon's environment is not one a granted `git` can read
    // unless the manifest asked for it.
    for inherited in ["PATH", "SystemRoot", "HOME"] {
        if let Ok(value) = std::env::var(inherited) {
            command.env(inherited, value);
        }
    }
    command.envs(grant.env.iter().map(|(k, v)| (k, v)));

    let mut child = command.spawn().map_err(|source| {
        ProcessError::Failed(format!("`{program}` could not be started: {source}"))
    })?;
    let writer = stdin.map(|bytes| {
        let mut pipe = child.stdin.take();
        let bytes = bytes.to_vec();
        std::thread::spawn(move || {
            if let Some(pipe) = pipe.as_mut() {
                // A program that exits without reading its input closes the pipe: that is its answer, not ours.
                let _ = pipe.write_all(&bytes);
            }
        })
    });
    let reader = |stream: Option<Box<dyn Read + Send>>| {
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            if let Some(stream) = stream {
                let _ = stream.take(MAX_OUTPUT_BYTES + 1).read_to_end(&mut bytes);
            }
            bytes
        })
    };
    let stdout = reader(
        child
            .stdout
            .take()
            .map(|s| Box::new(s) as Box<dyn Read + Send>),
    );
    let stderr = reader(
        child
            .stderr
            .take()
            .map(|s| Box::new(s) as Box<dyn Read + Send>),
    );

    let deadline = Instant::now() + grant.timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() >= deadline => {
                // Killed, and the reader threads are left to end when the pipes close: waiting on them could
                // outlive the limit if the child left a grandchild holding a pipe.
                let _ = child.kill();
                let _ = child.wait();
                return Err(ProcessError::Failed(format!(
                    "`{program}` did not finish within {}s and was stopped",
                    grant.timeout.as_secs()
                )));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(5)),
            Err(source) => {
                return Err(ProcessError::Failed(format!(
                    "waiting for `{program}` failed: {source}"
                )));
            }
        }
    };
    if let Some(writer) = writer {
        let _ = writer.join();
    }
    let stdout = stdout.join().unwrap_or_default();
    let stderr = stderr.join().unwrap_or_default();
    if stdout.len() as u64 > MAX_OUTPUT_BYTES || stderr.len() as u64 > MAX_OUTPUT_BYTES {
        return Err(ProcessError::Failed(format!(
            "`{program}` produced more than {} MiB of output",
            MAX_OUTPUT_BYTES / 1024 / 1024
        )));
    }
    Ok(Output {
        status: status.code().unwrap_or(-1),
        stdout,
        stderr,
    })
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn grant(allowed: &[&str]) -> ProcessGrant {
        ProcessGrant {
            allowed: allowed.iter().map(|s| (*s).to_string()).collect(),
            env: vec![],
            timeout: Duration::from_secs(5),
        }
    }

    #[test]
    fn a_program_outside_the_grant_is_denied_not_run() {
        let error = run(&grant(&["echo"]), "cat", &[], None).unwrap_err();
        assert!(matches!(error, ProcessError::Denied(_)), "{error:?}");
        assert!(matches!(
            run(&grant(&[]), "echo", &[], None),
            Err(ProcessError::Denied(_))
        ));
    }

    #[test]
    fn a_granted_program_runs_without_a_shell_and_its_output_comes_back() {
        // Shell metacharacters are plain arguments: nothing interprets them.
        let output = run(&grant(&["echo"]), "echo", &["a; echo b".to_string()], None).unwrap();
        assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "a; echo b");
        assert_eq!(output.status, 0);
    }

    #[test]
    fn standard_input_reaches_the_program() {
        let output = run(&grant(&["cat"]), "cat", &[], Some(b"hello")).unwrap();
        assert_eq!(output.stdout, b"hello");
    }

    #[test]
    fn the_child_sees_only_the_environment_it_was_granted() {
        let mut granted = grant(&["env"]);
        granted.env = vec![("ALLOWED".into(), "yes".into())];
        let output = run(&granted, "env", &[], None).unwrap();
        let text = String::from_utf8_lossy(&output.stdout);
        assert!(text.contains("ALLOWED=yes"), "{text}");
        assert!(!text.contains("CARGO"), "{text}");
    }

    #[test]
    fn a_program_that_outlives_the_time_limit_is_stopped() {
        let mut granted = grant(&["sleep"]);
        granted.timeout = Duration::from_millis(100);
        let error = run(&granted, "sleep", &["5".to_string()], None).unwrap_err();
        assert!(
            matches!(error, ProcessError::Failed(ref m) if m.contains("did not finish")),
            "{error:?}"
        );
    }
}

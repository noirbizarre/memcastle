//! The `run-process` host function: how a source runs a program it was granted, and nothing else.
//!
//! The granted names are matched exactly, there is no shell, the child sees only the environment the manifest listed
//! (plus `PATH`, to be found), and the run is bounded in time and in output.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
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

/// An anonymous scratch file for one of a program's output streams.
fn scratch_file(program: &str) -> Result<File, ProcessError> {
    tempfile::tempfile().map_err(|source| {
        ProcessError::Failed(format!(
            "no scratch file could be created to capture the output of `{program}`: {source}; check that the \
             temporary directory (`TMPDIR`) exists and has free space"
        ))
    })
}

/// A second handle on `file` for the child to write through; both share one offset and one length.
fn clone_file(file: &File, program: &str) -> Result<File, ProcessError> {
    file.try_clone().map_err(|source| {
        ProcessError::Failed(format!(
            "the output of `{program}` could not be captured: {source}"
        ))
    })
}

/// Everything the program wrote to `file`, up to one byte past the cap (so the caller can tell it was exceeded).
fn read_back(file: &mut File, program: &str) -> Result<Vec<u8>, ProcessError> {
    let failed = |source: std::io::Error| {
        ProcessError::Failed(format!(
            "the output of `{program}` could not be read back: {source}"
        ))
    };
    // The handle shares its offset with the child's, which is at the end of what it wrote.
    file.seek(SeekFrom::Start(0)).map_err(failed)?;
    let mut bytes = Vec::new();
    file.take(MAX_OUTPUT_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(failed)?;
    Ok(bytes)
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
    // Output goes to anonymous files, not pipes: a CLI built on Bun (OpenCode) can exit before a large write to a pipe
    // has drained, so the answer is cut at a multiple of 64 KiB while the exit status is still 0, and a source then
    // parses half a document. A file is written synchronously, so what the program printed is what we read. The files
    // are unlinked on creation: there is no path to leak and nothing to clean up, and they are the host's scratch, not
    // something the guest is granted.
    let mut stdout_file = scratch_file(program)?;
    let mut stderr_file = scratch_file(program)?;
    let mut command = Command::new(program);
    command
        .args(args)
        .env_clear()
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::from(clone_file(&stdout_file, program)?))
        .stderr(Stdio::from(clone_file(&stderr_file, program)?));
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
    let too_much = || {
        ProcessError::Failed(format!(
            "`{program}` produced more than {} MiB of output",
            MAX_OUTPUT_BYTES / 1024 / 1024
        ))
    };

    let deadline = Instant::now() + grant.timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(ProcessError::Failed(format!(
                    "`{program}` did not finish within {}s and was stopped",
                    grant.timeout.as_secs()
                )));
            }
            // A file, unlike a pipe, does not make a chatty program wait for a reader, so the cap is enforced while it
            // runs: otherwise an endless writer would fill the disk until the time limit.
            Ok(None)
                if [&stdout_file, &stderr_file]
                    .iter()
                    .any(|file| file.metadata().is_ok_and(|m| m.len() > MAX_OUTPUT_BYTES)) =>
            {
                let _ = child.kill();
                let _ = child.wait();
                return Err(too_much());
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
    let stdout = read_back(&mut stdout_file, program)?;
    let stderr = read_back(&mut stderr_file, program)?;
    // The program may have written its last bytes between the final size check and its exit.
    if stdout.len() as u64 > MAX_OUTPUT_BYTES || stderr.len() as u64 > MAX_OUTPUT_BYTES {
        return Err(too_much());
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

    #[test]
    fn a_granted_program_that_does_not_exist_fails_to_start_and_is_not_a_denial() {
        let error = run(
            &grant(&["memcastle-no-such-program"]),
            "memcastle-no-such-program",
            &[],
            None,
        )
        .unwrap_err();
        assert!(
            matches!(error, ProcessError::Failed(ref m) if m.contains("could not be started")),
            "{error:?}"
        );
    }

    #[test]
    fn more_output_than_the_cap_is_an_error_not_an_unbounded_answer() {
        // 17 MiB of zeros, a MiB over the cap.
        let error = run(
            &grant(&["head"]),
            "head",
            &[
                "-c".to_string(),
                (17 * 1024 * 1024).to_string(),
                "/dev/zero".to_string(),
            ],
            None,
        )
        .unwrap_err();
        assert!(
            matches!(error, ProcessError::Failed(ref m) if m.contains("more than 16 MiB")),
            "{error:?}"
        );
    }

    #[test]
    fn a_large_answer_comes_back_whole_and_so_does_standard_error() {
        // Well past a pipe's 64 KiB buffer, which is where a CLI that exits early used to be cut off.
        let output = run(
            &grant(&["sh"]),
            "sh",
            &[
                "-c".to_string(),
                "head -c 2097152 /dev/zero; echo oops >&2".to_string(),
            ],
            None,
        )
        .unwrap();
        assert_eq!(output.stdout.len(), 2 * 1024 * 1024);
        assert_eq!(String::from_utf8_lossy(&output.stderr).trim(), "oops");
    }

    #[test]
    fn a_program_that_writes_without_end_is_stopped_at_the_cap_not_at_the_time_limit() {
        let mut granted = grant(&["yes"]);
        granted.timeout = Duration::from_secs(60);
        let started = Instant::now();
        let error = run(&granted, "yes", &[], None).unwrap_err();
        assert!(
            matches!(error, ProcessError::Failed(ref m) if m.contains("more than 16 MiB")),
            "{error:?}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(30),
            "the cap was enforced only by the time limit"
        );
    }

    #[test]
    fn a_program_that_fails_reports_its_exit_status_rather_than_an_error() {
        let output = run(&grant(&["false"]), "false", &[], None).unwrap();
        assert_ne!(output.status, 0);
    }
}

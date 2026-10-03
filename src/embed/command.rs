//! The `command` embedding provider: run the operator's program.
//!
//! The protocol is deliberately tiny so any script can speak it. MemCastle
//! writes one JSON object to the program's stdin and reads one from stdout:
//!
//! ```json
//! {"model": "name-or-null", "dimension": 768, "texts": ["...", "..."]}
//! {"embeddings": [[0.1, ...], [0.2, ...]]}
//! ```
//!
//! The program owns credentials and provider choice, which is the point: a
//! daemon that stores API keys is a daemon that has to keep them safe.

use std::process::Stdio;
use std::time::Duration;

use serde::Deserialize;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

use super::{EmbedFuture, Embedder};
use crate::domain::EMBEDDING_DIMENSION;
use crate::error::{Error, Result};

/// How much of the program's stderr an error carries.
const STDERR_LIMIT: usize = 500;

/// Runs a program once per batch.
pub struct CommandEmbedder {
    argv: Vec<String>,
    model: Option<String>,
    timeout: Duration,
}

#[derive(Deserialize)]
struct Answer {
    embeddings: Vec<Vec<f32>>,
}

impl CommandEmbedder {
    /// A provider that runs `argv` (program first, no shell), passing `model`
    /// in the request, and abandons a call after `timeout`.
    #[must_use]
    pub fn new(argv: Vec<String>, model: Option<String>, timeout: Duration) -> Self {
        Self {
            argv,
            model,
            timeout,
        }
    }

    async fn run(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        let (program, args) = self
            .argv
            .split_first()
            .ok_or_else(|| Error::EmbeddingFailed {
                message: "embeddings.command is empty".to_string(),
            })?;
        let request = serde_json::json!({
            "model": self.model,
            "dimension": EMBEDDING_DIMENSION,
            "texts": texts,
        })
        .to_string();

        let mut command = Command::new(program);
        command
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // A call that times out is dropped; without this its process would
            // outlive the daemon's interest in it.
            .kill_on_drop(true);
        // The user's program inherits the environment (it needs PATH and its
        // own credentials) minus MemCastle's own: `MEMCASTLE_AUTH_TOKEN` and
        // the database password must never leak to a script (ADR-014).
        for (name, _) in std::env::vars_os() {
            if name.to_string_lossy().starts_with("MEMCASTLE_") {
                command.env_remove(name);
            }
        }
        let mut child = command.spawn().map_err(|source| Error::EmbeddingFailed {
            message: format!("could not run `{program}`: {source}"),
        })?;

        let mut stdin = child.stdin.take().ok_or_else(|| Error::EmbeddingFailed {
            message: "the embedding command's stdin was not available".to_string(),
        })?;
        // Written from its own task: a program that starts answering before it
        // has read everything would otherwise deadlock against a full pipe.
        let writer = tokio::spawn(async move {
            // A program that exits without reading closes the pipe; that
            // surfaces as its exit status below, not as a write error here.
            let _ = stdin.write_all(request.as_bytes()).await;
        });

        let output = tokio::time::timeout(self.timeout, child.wait_with_output())
            .await
            .map_err(|_| Error::EmbeddingFailed {
                message: format!(
                    "`{program}` did not answer within {} second(s); raise embeddings.timeout_secs \
                     or make the program faster",
                    self.timeout.as_secs()
                ),
            })?
            .map_err(|source| Error::EmbeddingFailed {
                message: format!("waiting for `{program}` failed: {source}"),
            })?;
        let _ = writer.await;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stderr: String = stderr.trim().chars().take(STDERR_LIMIT).collect();
            return Err(Error::EmbeddingFailed {
                message: format!("`{program}` exited with {}: {stderr}", output.status),
            });
        }
        let answer: Answer =
            serde_json::from_slice(&output.stdout).map_err(|source| Error::EmbeddingFailed {
                message: format!(
                    "`{program}` did not print `{{\"embeddings\": [[...]]}}` on stdout: {source}"
                ),
            })?;
        Ok(answer.embeddings)
    }
}

impl Embedder for CommandEmbedder {
    fn embed<'a>(&'a self, texts: &'a [String]) -> EmbedFuture<'a> {
        Box::pin(self.run(texts))
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    /// A provider running `sh -c script`.
    fn sh(script: &str, timeout: Duration) -> CommandEmbedder {
        CommandEmbedder::new(
            vec!["sh".into(), "-c".into(), script.into()],
            Some("m".into()),
            timeout,
        )
    }

    /// A script that answers one 768-long vector per input text, using only
    /// POSIX tools: count the `"` pairs is fragile, so it answers for exactly one text.
    const ONE_VECTOR: &str = "cat >/dev/null; \
        printf '{\"embeddings\":[['; i=0; while [ $i -lt 768 ]; do \
          [ $i -gt 0 ] && printf ','; printf '0.5'; i=$((i+1)); done; printf ']]}'";

    #[tokio::test]
    async fn a_program_speaking_the_protocol_yields_its_vectors() {
        let embedder = sh(ONE_VECTOR, Duration::from_secs(20));
        let vectors = embedder.run(&["hello".to_string()]).await.expect("answers");
        assert_eq!(vectors.len(), 1);
        assert_eq!(vectors[0].len(), EMBEDDING_DIMENSION);
    }

    #[tokio::test]
    async fn the_request_carries_the_model_dimension_and_texts() {
        // The program echoes what it read into its stderr and fails, so the
        // error message shows the request exactly as MemCastle sent it.
        let embedder = sh("cat >&2; exit 3", Duration::from_secs(20));
        let error = embedder
            .run(&["alpha".to_string()])
            .await
            .expect_err("exits non-zero");
        let message = error.to_string();
        assert!(message.contains(r#""model":"m""#), "{message}");
        assert!(message.contains(r#""dimension":768"#), "{message}");
        assert!(message.contains(r#""texts":["alpha"]"#), "{message}");
    }

    #[tokio::test]
    async fn a_failing_program_reports_its_exit_status_and_stderr() {
        let embedder = sh("echo 'quota exceeded' >&2; exit 2", Duration::from_secs(20));
        let message = embedder
            .run(&["x".to_string()])
            .await
            .expect_err("fails")
            .to_string();
        assert!(message.contains("quota exceeded"), "{message}");
    }

    #[tokio::test]
    async fn a_program_that_prints_something_else_is_named_in_the_error() {
        let embedder = sh("cat >/dev/null; echo not-json", Duration::from_secs(20));
        let message = embedder
            .run(&["x".to_string()])
            .await
            .expect_err("bad output")
            .to_string();
        assert!(message.contains("embeddings"), "{message}");
    }

    #[tokio::test]
    async fn a_program_that_hangs_is_abandoned_at_the_timeout() {
        let embedder = sh("sleep 30", Duration::from_millis(300));
        let started = std::time::Instant::now();
        let message = embedder
            .run(&["x".to_string()])
            .await
            .expect_err("times out")
            .to_string();
        assert!(message.contains("did not answer"), "{message}");
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    #[tokio::test]
    async fn a_missing_program_names_it() {
        let embedder = CommandEmbedder::new(
            vec!["/nonexistent/embedder".into()],
            None,
            Duration::from_secs(5),
        );
        let message = embedder
            .run(&["x".to_string()])
            .await
            .expect_err("cannot spawn")
            .to_string();
        assert!(message.contains("/nonexistent/embedder"), "{message}");
    }

    #[tokio::test]
    async fn memcastle_environment_variables_are_not_passed_to_the_program() {
        // SAFETY: a unique variable no other test reads; set before spawning.
        unsafe { std::env::set_var("MEMCASTLE_TEST_SECRET_LEAK", "hunter2") };
        let embedder = sh(
            "cat >/dev/null; echo \"leak=[$MEMCASTLE_TEST_SECRET_LEAK]\" >&2; exit 1",
            Duration::from_secs(20),
        );
        let message = embedder
            .run(&["x".to_string()])
            .await
            .expect_err("exits non-zero")
            .to_string();
        assert!(message.contains("leak=[]"), "{message}");
        assert!(!message.contains("hunter2"), "{message}");
    }
}

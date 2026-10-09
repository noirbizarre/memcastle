//! The `command` extraction provider: run the operator's program.
//!
//! The protocol is deliberately tiny so any script can speak it. MemCastle writes one JSON object to the program's
//! stdin and reads one from stdout:
//!
//! ```json
//! {"model": "name-or-null",
//!  "vocabulary": {"kinds": ["person", "..."], "predicates": ["works_on", "..."]},
//!  "texts": ["...", "..."]}
//! {"extractions": [
//!    {"entities": [{"name": "Ada", "kind": "person"}],
//!     "relations": [{"subject": "Ada", "predicate": "works_on", "object": "MemCastle", "confidence": 0.9}]}]}
//! ```
//!
//! One extraction per text, in order. A kind or predicate outside the vocabulary is read as `other` or
//! `related_to`. The program owns credentials and the model, which is the point: a daemon that stores API keys is a
//! daemon that has to keep them safe.
//! With a configured source preference, an optional `preferences` array aligns with `texts` and carries each
//! input's effective level and matching criterion; providers must still judge assertions from the text.

use std::process::Stdio;
use std::time::Duration;

use serde::Deserialize;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

use super::{ExtractFuture, Extractor, WireGraph};
use crate::domain::{EntityKind, ExtractedGraph, Predicate, PreferenceMatch};
use crate::error::{Error, Result};

/// How much of the program's stderr an error carries.
const STDERR_LIMIT: usize = 500;

/// Runs a program once per batch.
pub struct CommandExtractor {
    argv: Vec<String>,
    model: Option<String>,
    timeout: Duration,
}

#[derive(Deserialize)]
struct Answer {
    extractions: Vec<WireGraph>,
}

impl CommandExtractor {
    /// A provider that runs `argv` (program first, no shell), passing `model` in the request, and abandons a call
    /// after `timeout`.
    #[must_use]
    pub fn new(argv: Vec<String>, model: Option<String>, timeout: Duration) -> Self {
        Self {
            argv,
            model,
            timeout,
        }
    }

    async fn run(
        &self,
        texts: &[String],
        context: &[PreferenceMatch],
    ) -> Result<Vec<ExtractedGraph>> {
        let (program, args) = self
            .argv
            .split_first()
            .ok_or_else(|| Error::ExtractionFailed {
                message: "extraction.command is empty".to_string(),
            })?;
        let mut request = serde_json::json!({
            "model": self.model,
            "vocabulary": {
                "kinds": EntityKind::ALL.map(EntityKind::as_str),
                "predicates": Predicate::ALL.map(Predicate::as_str),
            },
            "texts": texts,
        });
        if !context.is_empty() {
            request["preferences"] = serde_json::json!(context);
        }
        let request = request.to_string();

        let mut command = Command::new(program);
        command
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // A call that times out is dropped; without this its process would outlive the daemon's interest in it.
            .kill_on_drop(true);
        // The user's program inherits the environment (it needs PATH and its own credentials) minus MemCastle's
        // own: `MEMCASTLE_AUTH_TOKEN` and the database password must never leak to a script (ADR-014).
        for (name, _) in std::env::vars_os() {
            if name.to_string_lossy().starts_with("MEMCASTLE_") {
                command.env_remove(name);
            }
        }
        let mut child = command.spawn().map_err(|source| Error::ExtractionFailed {
            message: format!("could not run `{program}`: {source}"),
        })?;

        let mut stdin = child.stdin.take().ok_or_else(|| Error::ExtractionFailed {
            message: "the extraction command's stdin was not available".to_string(),
        })?;
        // Written from its own task: a program that starts answering before it has read everything would otherwise
        // deadlock against a full pipe.
        let writer = tokio::spawn(async move {
            // A program that exits without reading closes the pipe; that surfaces as its exit status below.
            let _ = stdin.write_all(request.as_bytes()).await;
        });

        let output = tokio::time::timeout(self.timeout, child.wait_with_output())
            .await
            .map_err(|_| Error::ExtractionFailed {
                message: format!(
                    "`{program}` did not answer within {} second(s); raise extraction.timeout_secs \
                     or make the program faster",
                    self.timeout.as_secs()
                ),
            })?
            .map_err(|source| Error::ExtractionFailed {
                message: format!("waiting for `{program}` failed: {source}"),
            })?;
        let _ = writer.await;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stderr: String = stderr.trim().chars().take(STDERR_LIMIT).collect();
            return Err(Error::ExtractionFailed {
                message: format!("`{program}` exited with {}: {stderr}", output.status),
            });
        }
        let answer: Answer =
            serde_json::from_slice(&output.stdout).map_err(|source| Error::ExtractionFailed {
                message: format!(
                    "`{program}` did not print `{{\"extractions\": [...]}}` on stdout: {source}"
                ),
            })?;
        Ok(answer
            .extractions
            .into_iter()
            .map(WireGraph::into_graph)
            .collect())
    }
}

impl Extractor for CommandExtractor {
    fn name(&self) -> &'static str {
        "command"
    }

    fn extract<'a>(&'a self, texts: &'a [String]) -> ExtractFuture<'a> {
        Box::pin(self.run(texts, &[]))
    }

    fn extract_with_preferences<'a>(
        &'a self,
        texts: &'a [String],
        context: &'a [PreferenceMatch],
    ) -> ExtractFuture<'a> {
        Box::pin(self.run(texts, context))
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn sh(script: &str) -> CommandExtractor {
        CommandExtractor::new(
            vec!["sh".into(), "-c".into(), script.into()],
            Some("m".into()),
            Duration::from_secs(10),
        )
    }

    #[tokio::test]
    async fn a_program_answering_in_the_protocol_yields_one_graph_per_text() {
        let provider = sh(
            r#"cat >/dev/null; printf '%s' '{"extractions":[{"entities":[{"name":"Ada","kind":"person"}]},{}]}'"#,
        );
        let graphs = provider
            .run(&["a".to_string(), "b".to_string()], &[])
            .await
            .expect("answers");
        assert_eq!(graphs.len(), 2);
        assert_eq!(graphs[0].entities[0].name, "Ada");
        assert!(graphs[1].entities.is_empty());
    }

    #[tokio::test]
    async fn the_program_is_sent_the_texts_the_model_and_the_vocabulary() {
        // Echoes whether the request mentions each piece, so the test does not depend on JSON key order.
        let provider = sh(r#"req=$(cat)
               case "$req" in
                 *works_on*) v=1;; *) v=0;; esac
               case "$req" in *'"model":"m"'*) m=1;; *) m=0;; esac
               case "$req" in *hello*) t=1;; *) t=0;; esac
               if [ "$v$m$t" = 111 ]; then printf '%s' '{"extractions":[{"entities":[{"name":"Seen"}]}]}'; else printf '%s' '{"extractions":[{}]}'; fi"#);
        let graphs = provider.run(&["hello".to_string()], &[]).await.unwrap();
        assert_eq!(graphs[0].entities.len(), 1, "the request lacked a piece");
    }

    #[tokio::test]
    async fn a_configured_preference_reaches_the_program_alongside_its_text() {
        let provider = sh(
            r#"req=$(cat); case "$req" in *'"level":"high"'*'"criterion":"path"'*) printf '%s' '{"extractions":[{"entities":[{"name":"Seen"}]}]}' ;; *) printf '%s' '{"extractions":[{}]}' ;; esac"#,
        );
        let context = [PreferenceMatch {
            level: crate::domain::PreferenceLevel::High,
            source: Some("directory".into()),
            criterion: Some("path".into()),
        }];
        let graphs = provider.run(&["text".into()], &context).await.unwrap();
        assert_eq!(graphs[0].entities.len(), 1);
    }

    #[tokio::test]
    async fn a_failing_program_is_reported_with_its_stderr() {
        let error = sh("echo boom >&2; exit 3")
            .run(&["x".to_string()], &[])
            .await
            .expect_err("fails");
        let message = error.to_string();
        assert!(
            message.contains("boom") && message.contains('3'),
            "{message}"
        );
    }

    #[tokio::test]
    async fn output_that_is_not_the_protocol_is_reported() {
        let error = sh("cat >/dev/null; echo nope")
            .run(&["x".to_string()], &[])
            .await
            .expect_err("not json");
        assert!(matches!(error, Error::ExtractionFailed { .. }), "{error}");
    }

    #[tokio::test]
    async fn a_program_that_never_answers_is_abandoned_at_the_timeout() {
        let provider = CommandExtractor::new(
            vec!["sh".into(), "-c".into(), "sleep 30".into()],
            None,
            Duration::from_millis(200),
        );
        let error = provider
            .run(&["x".to_string()], &[])
            .await
            .expect_err("times out");
        assert!(error.to_string().contains("did not answer"), "{error}");
    }

    #[tokio::test]
    async fn a_missing_program_names_itself() {
        let provider = CommandExtractor::new(
            vec!["/nonexistent/extract-program".into()],
            None,
            Duration::from_secs(5),
        );
        let error = provider
            .run(&["x".to_string()], &[])
            .await
            .expect_err("missing");
        assert!(
            error.to_string().contains("/nonexistent/extract-program"),
            "{error}"
        );
    }
}

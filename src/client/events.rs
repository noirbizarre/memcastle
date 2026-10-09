//! Authenticated reader of the daemon's existing SSE endpoint.
//!
//! Frames carry identifiers only; callers reread job details through the normal API.

use serde::Deserialize;

use super::{DaemonClient, http_client};
use crate::error::{Error, Result};

/// A change notice or a signal to reload all state after a gap.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Notice {
    /// The stream opened; changes before subscription may have been missed.
    Open,
    /// A job changed; fetch its authoritative record.
    Job {
        /// The job ID to reread through the API.
        id: String,
        /// Its fixed kind name (never job input).
        kind: String,
    },
    /// The relay lost events; reload the whole view.
    Resync,
}

#[derive(Deserialize)]
struct JobNotice {
    id: Option<String>,
    job_kind: Option<String>,
}

/// One connected response, with a buffer for frames split across HTTP chunks.
#[derive(Debug)]
pub struct EventStream {
    response: reqwest::Response,
    pending: Vec<u8>,
}

impl DaemonClient {
    /// Open the same authenticated `/api/events` stream used by the web UI.
    /// A whole-response timeout would close an otherwise healthy idle stream.
    pub async fn events(&self) -> Result<EventStream> {
        let http = http_client(self.mode, self.token.as_ref(), None);
        let response = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            http.get(self.api_url(&["events"], None)?).send(),
        )
        .await
        .map_err(|_| Error::Client {
            message: "the event stream did not open within ten seconds".into(),
        })?
        .map_err(|source| {
            if source.is_connect() {
                Error::DaemonNotRunning
            } else {
                Error::from(source)
            }
        })?;
        if !response.status().is_success() {
            let status = response.status();
            let body: serde_json::Value = response.json().await.unwrap_or_default();
            return Err(Error::remote(
                status.as_u16(),
                body.get("code").and_then(serde_json::Value::as_str),
                body.get("error")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or(status.canonical_reason().unwrap_or("stream refused"))
                    .to_string(),
                body.get("help")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string),
            ));
        }
        Ok(EventStream {
            response,
            pending: Vec::new(),
        })
    }
}

impl EventStream {
    /// Return the next relevant notice, or `None` when the daemon closes the stream.
    pub async fn next(&mut self) -> Result<Option<Notice>> {
        loop {
            if let Some(frame) = take_frame(&mut self.pending) {
                if let Some(notice) = parse_frame(&frame) {
                    return Ok(Some(notice));
                }
                continue;
            }
            // A proxy that never ends a frame cannot grow the client's memory without bound.
            if self.pending.len() > 64 * 1024 {
                return Err(Error::invalid_input(
                    "events",
                    "daemon sent an oversized event frame",
                ));
            }
            // A stalled proxy can stop forwarding keep-alives without closing the socket.
            let chunk =
                tokio::time::timeout(std::time::Duration::from_secs(45), self.response.chunk())
                    .await
                    .map_err(|_| Error::Client {
                        message: "event stream timed out waiting for a keep-alive".into(),
                    })?
                    .map_err(Error::from)?;
            match chunk {
                Some(bytes) => self.pending.extend_from_slice(&bytes),
                None => return Ok(None),
            }
        }
    }
}

fn take_frame(pending: &mut Vec<u8>) -> Option<Vec<u8>> {
    let lf = pending
        .windows(2)
        .position(|bytes| bytes == b"\n\n")
        .map(|index| (index, 2));
    let crlf = pending
        .windows(4)
        .position(|bytes| bytes == b"\r\n\r\n")
        .map(|index| (index, 4));
    let (end, size) = match (lf, crlf) {
        (Some(left), Some(right)) => {
            if left.0 < right.0 {
                left
            } else {
                right
            }
        }
        (Some(one), None) | (None, Some(one)) => one,
        (None, None) => return None,
    };
    Some(pending.drain(..end + size).collect())
}

fn parse_frame(frame: &[u8]) -> Option<Notice> {
    let text = std::str::from_utf8(frame).ok()?;
    let name = text
        .lines()
        .find_map(|line| line.trim_end_matches('\r').strip_prefix("event:"))?
        .trim_start();
    match name {
        "open" => Some(Notice::Open),
        "resync" => Some(Notice::Resync),
        "job" => {
            let data = text
                .lines()
                .find_map(|line| line.trim_end_matches('\r').strip_prefix("data:"))?
                .trim_start();
            let job: JobNotice = serde_json::from_str(data).ok()?;
            Some(Notice::Job {
                id: job.id?,
                kind: job.job_kind?,
            })
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_job_notice_uses_the_existing_event_payload() {
        assert_eq!(
            parse_frame(b"event: job\ndata: {\"id\":\"123\",\"job_kind\":\"mine\"}\n\n"),
            Some(Notice::Job {
                id: "123".into(),
                kind: "mine".into()
            })
        );
        assert_eq!(parse_frame(b": keepalive\n\n"), None);
    }

    #[test]
    fn a_frame_split_across_chunks_is_read_only_after_its_terminator() {
        let mut pending =
            b"event:job\r\ndata:{\"id\":\"one\",\"job_kind\":\"mine\"}\r\n\r".to_vec();
        assert!(take_frame(&mut pending).is_none());
        pending.extend_from_slice(b"\nevent:resync\n\n");
        assert_eq!(
            parse_frame(&take_frame(&mut pending).unwrap()),
            Some(Notice::Job {
                id: "one".into(),
                kind: "mine".into()
            })
        );
        assert_eq!(
            parse_frame(&take_frame(&mut pending).unwrap()),
            Some(Notice::Resync)
        );
        assert!(pending.is_empty());
    }
}

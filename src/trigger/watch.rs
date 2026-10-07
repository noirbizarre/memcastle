//! The `watch` trigger: ask for a run when something changes under a path.
//!
//! Changes are coalesced: one request goes out once the path has been quiet for `debounce` (an editor's save, a
//! `git checkout` or a sync tool's burst is one request, not hundreds), and never later than a bounded wait after the
//! first change, so a path that never goes quiet still gets mined. If the watcher cannot be set up or breaks (the path
//! is missing, the operating system's limit on watches is reached, the directory was removed) the failure is reported
//! and the watcher is set up again with a growing wait, which is what makes a watched path that appears later, or a
//! volume that is mounted later, start working without anyone restarting the daemon.

use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use notify::{EventKind, RecursiveMode, Watcher};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::debug;

use crate::domain::TriggerMechanism;

use super::{FireRequest, Host, Note};

/// The wait before the first attempt to set up the watcher again, doubled up to [`MAX_RETRY`].
const FIRST_RETRY: Duration = Duration::from_secs(1);
const MAX_RETRY: Duration = Duration::from_secs(60);
/// How many debounce periods a burst may keep postponing the request for.
const MAX_POSTPONEMENT_FACTOR: u32 = 10;
/// The longest a burst may keep postponing the request, whatever the debounce.
const MAX_POSTPONEMENT: Duration = Duration::from_secs(60);

type Message = Result<notify::Event, notify::Error>;

/// Whether an event is a change worth a request: not a read, and not inside version-control metadata, which changes
/// on every command and says nothing about the files.
fn is_change(event: &notify::Event) -> bool {
    if matches!(event.kind, EventKind::Access(_)) {
        return false;
    }
    event.paths.iter().any(|path| !inside_vcs_metadata(path))
}

fn inside_vcs_metadata(path: &Path) -> bool {
    path.components()
        .any(|part| matches!(part, Component::Normal(name) if name == ".git"))
}

fn start(
    path: &Path,
    recursive: bool,
    tx: mpsc::UnboundedSender<Message>,
) -> Result<notify::RecommendedWatcher, String> {
    if !path.exists() {
        return Err(format!(
            "{} does not exist; create it, or point the trigger's `path` at one that does",
            path.display()
        ));
    }
    let mut watcher = notify::recommended_watcher(move |message: Message| {
        // A closed channel means the task is gone and the watcher is being dropped with it.
        let _ = tx.send(message);
    })
    .map_err(|e| format!("cannot start a file watcher: {e}"))?;
    let mode = if recursive {
        RecursiveMode::Recursive
    } else {
        RecursiveMode::NonRecursive
    };
    watcher
        .watch(path, mode)
        .map_err(|e| format!("cannot watch {}: {e}", path.display()))?;
    Ok(watcher)
}

pub(super) async fn run<H: Host>(
    host: H,
    name: String,
    path: PathBuf,
    debounce: Duration,
    recursive: bool,
    cancel: CancellationToken,
) {
    let mut retry = FIRST_RETRY;
    loop {
        let (tx, mut rx) = mpsc::unbounded_channel::<Message>();
        // Kept alive for the whole inner loop: dropping the watcher is what stops it.
        let watcher = start(&path, recursive, tx);
        let failure = match watcher {
            Err(reason) => reason,
            Ok(_watcher) => {
                host.note(&name, Note::Healthy).await;
                retry = FIRST_RETRY;
                match watch_loop(&host, &name, &path, debounce, &mut rx, &cancel).await {
                    // Cancelled: the trigger was switched off or the daemon is stopping.
                    None => return,
                    Some(reason) => reason,
                }
            }
        };
        host.note(&name, Note::Failed(failure)).await;
        tokio::select! {
            () = cancel.cancelled() => return,
            () = tokio::time::sleep(retry) => {}
        }
        retry = (retry * 2).min(MAX_RETRY);
    }
}

/// Wait for changes and ask for a run for each quiet burst. Returns why the watcher broke, or `None` if cancelled.
async fn watch_loop<H: Host>(
    host: &H,
    name: &str,
    path: &Path,
    debounce: Duration,
    rx: &mut mpsc::UnboundedReceiver<Message>,
    cancel: &CancellationToken,
) -> Option<String> {
    loop {
        // Wait for the first change of a burst.
        loop {
            let message = tokio::select! {
                () = cancel.cancelled() => return None,
                message = rx.recv() => message,
            };
            match message {
                None => return Some("the file watcher stopped".to_string()),
                Some(Err(error)) => return Some(format!("the file watcher failed: {error}")),
                Some(Ok(event)) if is_change(&event) => break,
                Some(Ok(_)) => {}
            }
        }
        // Then wait for it to go quiet, but not forever.
        let give_up = tokio::time::Instant::now()
            + (debounce * MAX_POSTPONEMENT_FACTOR)
                .min(MAX_POSTPONEMENT)
                .max(debounce);
        loop {
            let remaining = give_up.saturating_duration_since(tokio::time::Instant::now());
            let wait = debounce.min(remaining);
            if wait.is_zero() {
                break;
            }
            tokio::select! {
                () = cancel.cancelled() => return None,
                message = tokio::time::timeout(wait, rx.recv()) => match message {
                    // Quiet for the whole debounce.
                    Err(_) => break,
                    Ok(None) => return Some("the file watcher stopped".to_string()),
                    Ok(Some(Err(error))) => return Some(format!("the file watcher failed: {error}")),
                    Ok(Some(Ok(_))) => {}
                },
            }
        }
        // A removed directory ends a watch without an error on some platforms, so the path is checked each time.
        if !path.exists() {
            return Some(format!("{} no longer exists", path.display()));
        }
        debug!(trigger = %name, "a change under the watched path: asking for a run");
        // The host records the outcome and any failure; a refused request is not a reason to set the watcher up again.
        let _ = host
            .fire(FireRequest {
                trigger: name.to_string(),
                via: Some(TriggerMechanism::Watch),
                delivery: None,
            })
            .await;
    }
}

#[cfg(test)]
mod tests {
    use notify::event::{AccessKind, CreateKind};

    use super::*;

    fn event(kind: EventKind, path: &str) -> notify::Event {
        notify::Event::new(kind).add_path(PathBuf::from(path))
    }

    #[test]
    fn a_read_is_not_a_change() {
        assert!(!is_change(&event(
            EventKind::Access(AccessKind::Any),
            "/n/a.md"
        )));
        assert!(is_change(&event(
            EventKind::Create(CreateKind::File),
            "/n/a.md"
        )));
    }

    #[test]
    fn version_control_metadata_is_not_a_change() {
        assert!(!is_change(&event(
            EventKind::Create(CreateKind::File),
            "/n/.git/index.lock"
        )));
        assert!(is_change(&event(
            EventKind::Create(CreateKind::File),
            "/n/.gitignore"
        )));
    }
}

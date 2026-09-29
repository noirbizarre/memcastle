//! Proves multiple clients can hit the same daemon/store concurrently
//! without corrupting state — the concurrency invariant the architecture
//! doc calls out explicitly.

mod common;

use common::{TestDaemon, get_job, wait_for_job_status};
use futures::future::join_all;

#[tokio::test]
async fn concurrent_job_submissions_and_reads_all_land_correctly() {
    const N: usize = 12;
    let daemon = TestDaemon::start_with(N).await;
    let client = reqwest::Client::new();

    // N "clients" submitting demo jobs at the same time, plus interleaved
    // reads — every request goes through the same store/scheduler.
    let submissions = (0..N).map(|i| {
        let client = client.clone();
        let base = daemon.base_url.clone();
        async move {
            client
                .post(format!("{base}/api/jobs"))
                .json(&serde_json::json!({ "type": "demo", "steps": 1, "requested_by": format!("client-{i}") }))
                .send()
                .await
        }
    });
    let results = join_all(submissions).await;
    for result in results {
        let response = result.expect("request succeeded");
        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            panic!("job submission failed: {status}: {body}");
        }
    }

    // Concurrent status reads while jobs are (probably still) running.
    let reads = (0..N).map(|_| {
        let client = client.clone();
        let base = daemon.base_url.clone();
        async move { client.get(format!("{base}/api/status")).send().await }
    });
    for result in join_all(reads).await {
        assert!(result.expect("request succeeded").status().is_success());
    }

    // Poll for every job to reach a terminal state rather than sleeping a
    // fixed duration: a fixed sleep is exactly the kind of assumption that
    // holds on a fast, idle machine and flakes on a loaded CI runner with
    // fewer cores — this instead waits as long as it actually takes, up to
    // a generous ceiling, and only then asserts on the final state.
    let jobs = wait_for_all_jobs_completed(&client, &daemon.base_url, N).await;
    assert_eq!(
        jobs.len(),
        N,
        "every submitted job should be visible exactly once"
    );
    assert!(
        jobs.iter()
            .all(|job| job.status == memcastle::domain::JobStatus::Completed),
        "every demo job should have completed: {jobs:#?}"
    );

    daemon.shutdown().await;
}

async fn wait_for_all_jobs_completed(
    client: &reqwest::Client,
    base_url: &str,
    expected: usize,
) -> Vec<memcastle::domain::Job> {
    for _ in 0..300 {
        let jobs: Vec<memcastle::domain::Job> = client
            .get(format!("{base_url}/api/jobs"))
            .send()
            .await
            .expect("request")
            .json()
            .await
            .expect("json");
        if jobs.len() == expected
            && jobs
                .iter()
                .all(|job| job.status == memcastle::domain::JobStatus::Completed)
        {
            return jobs;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    panic!("not every job reached Completed within 30s");
}

#[tokio::test]
async fn a_critical_checkpoint_is_claimed_before_a_queued_background_mine_job() {
    use memcastle::domain::{Job, JobStatus};

    // A single worker slot makes claim order fully observable: while it's
    // held, both jobs below sit `Queued` at the same time, so which one
    // gets claimed next is decided purely by `priority`, not by "there
    // happened to be a free slot for both".
    let daemon = TestDaemon::start_with(1).await;
    let client = reqwest::Client::new();

    // Occupy the sole slot for a little while (8 steps * 150ms ~= 1.2s —
    // see `jobs::demo`'s `STEP_DELAY`), long enough to reliably submit and
    // observe both jobs below as `Queued` before it frees up.
    let occupier: Job = client
        .post(format!("{}/api/jobs", daemon.base_url))
        .json(&serde_json::json!({ "type": "demo", "steps": 8, "requested_by": "test" }))
        .send()
        .await
        .expect("submit occupier")
        .json()
        .await
        .expect("json");
    wait_for_job_status(&client, &daemon.base_url, occupier.id, JobStatus::Running).await;

    // Background priority (the default for `mine`), submitted first.
    let fixture = tempfile::tempdir().expect("fixture tempdir");
    std::fs::write(fixture.path().join("note.txt"), "irrelevant content")
        .expect("write fixture file");
    let mine: Job = client
        .post(format!("{}/api/jobs", daemon.base_url))
        .json(&serde_json::json!({
            "type": "mine",
            "path": fixture.path(),
            "wing": null,
            "requested_by": "test",
        }))
        .send()
        .await
        .expect("submit mine")
        .json()
        .await
        .expect("json");
    assert_eq!(
        get_job(&client, &daemon.base_url, mine.id).await.status,
        JobStatus::Queued,
        "the mine job must still be queued behind the occupier"
    );

    // Critical priority (`emergency: true`), submitted second.
    let checkpoint: Job = client
        .post(format!("{}/api/jobs", daemon.base_url))
        .json(&serde_json::json!({
            "type": "checkpoint",
            "payload": {
                "items": [{
                    "destination": "general",
                    "content": "an emergency checkpoint",
                    "tags": [],
                    "source": { "kind": "manual", "uri": null, "agent": "test" },
                    "fact": null,
                }],
            },
            "requested_by": "test",
            "emergency": true,
        }))
        .send()
        .await
        .expect("submit checkpoint")
        .json()
        .await
        .expect("json");

    // Once the occupier frees the slot, the checkpoint (Critical, priority
    // 100) must be claimed and complete before the mine job (Background,
    // priority 0) even though mine was submitted first — proving
    // `claim_next_job`'s `ORDER BY priority DESC` decides this, not
    // submission order.
    wait_for_job_status(
        &client,
        &daemon.base_url,
        checkpoint.id,
        JobStatus::Completed,
    )
    .await;
    assert_ne!(
        get_job(&client, &daemon.base_url, mine.id).await.status,
        JobStatus::Completed,
        "the background mine job must not have completed before the critical checkpoint did"
    );

    daemon.shutdown().await;
}

#[tokio::test]
async fn a_disabled_clients_mode_does_not_affect_another_clients_in_flight_job() {
    use memcastle::domain::{Job, JobStatus};

    // Client A occupies the sole worker slot with a multi-step demo job —
    // long enough to reliably observe it `Running` while client B's
    // disabled-mode requests land against the very same daemon.
    let daemon = TestDaemon::start_with(1).await;
    let client = reqwest::Client::new();

    let occupier: Job = client
        .post(format!("{}/api/jobs", daemon.base_url))
        .json(&serde_json::json!({ "type": "demo", "steps": 8, "requested_by": "client-a" }))
        .send()
        .await
        .expect("submit occupier")
        .json()
        .await
        .expect("json");
    wait_for_job_status(&client, &daemon.base_url, occupier.id, JobStatus::Running).await;

    // Client B, in `Disabled` mode, hits every gated endpoint while A's job
    // is running — all must be rejected, and none may touch the store.
    for url in [
        format!("{}/api/search?q=x", daemon.base_url),
        format!("{}/api/recall?q=x", daemon.base_url),
        format!("{}/api/wake-up?agent_identity=client-b", daemon.base_url),
        format!(
            "{}/api/diary?agent_identity=client-b&wing=w",
            daemon.base_url
        ),
    ] {
        let response = client
            .get(&url)
            .header("X-MemCastle-Mode", "disabled")
            .send()
            .await
            .expect("request");
        assert_eq!(
            response.status(),
            reqwest::StatusCode::FORBIDDEN,
            "GET {url} must be rejected for client B's disabled session"
        );
    }
    let diary_write = client
        .post(format!("{}/api/diary", daemon.base_url))
        .header("X-MemCastle-Mode", "disabled")
        .json(&serde_json::json!({
            "agent_identity": "client-b",
            "wing": "w",
            "content": "must never be written",
        }))
        .send()
        .await
        .expect("request");
    assert_eq!(diary_write.status(), reqwest::StatusCode::FORBIDDEN);

    // Client A's occupier must still complete normally — client B's
    // disabled mode is per-session and must never reach the scheduler or
    // any other client's job.
    let completed =
        wait_for_job_status(&client, &daemon.base_url, occupier.id, JobStatus::Completed).await;
    assert_eq!(
        completed.status,
        JobStatus::Completed,
        "client A's job must complete regardless of client B's disabled mode"
    );

    daemon.shutdown().await;
}

//! Proves multiple clients can hit the same daemon/store concurrently
//! without corrupting state — the invariant that one daemon serves many
//! agent sessions at once (see `docs/architecture.md`, "The core idea", and
//! ADR-002's tests of memory-mode isolation).

use crate::common;

use common::{TestDaemon, get_job, wait_for_all_jobs_completed, wait_for_job_status};
use futures::future::join_all;

/// A bounded, real-store load: acquisition can run ahead but HTTP and MCP must still get CPU and DB time.
#[tokio::test]
async fn rest_and_mcp_answer_while_mines_and_embeddings_are_active() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::{Duration, Instant};

    use axum::{Json, Router, routing::post};
    use memcastle::config::EmbeddingProvider;
    use memcastle::domain::{Job, JobStatus};
    use rmcp::model::{CallToolRequestParams, ClientConfig};
    use rmcp::service::serve_client;
    use rmcp::transport::streamable_http_client::StreamableHttpClientTransport;
    use serde_json::{Value, json};

    let first_batch = Arc::new(AtomicBool::new(true));
    let provider = Router::new().route(
        "/v1/embeddings",
        post(move |Json(body): Json<Value>| {
            let first_batch = Arc::clone(&first_batch);
            async move {
                // Stable nonzero vectors without an external model or subprocess.
                let data: Vec<Value> = body["input"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .enumerate()
                    .map(|(index, _)| {
                        let mut vector = vec![0.0f32; 768];
                        vector[index % 768] = 1.0;
                        json!({ "index": index, "embedding": vector })
                    })
                    .collect();
                // Hold one sweep open long enough to exercise both request surfaces during embedding.
                tokio::time::sleep(if first_batch.swap(false, Ordering::SeqCst) {
                    Duration::from_secs(8)
                } else {
                    Duration::from_millis(40)
                })
                .await;
                Json(json!({ "data": data }))
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let provider_url = format!("http://{}/v1", listener.local_addr().unwrap());
    let provider_handle =
        tokio::spawn(async move { axum::serve(listener, provider).await.unwrap() });
    let daemon = TestDaemon::start_configured(move |config| {
        config.jobs.max_concurrency = 4;
        config.jobs.background_concurrency = 2;
        config.embeddings.provider = EmbeddingProvider::Http;
        config.embeddings.url = Some(provider_url);
        config.embeddings.model = Some("stub".into());
    })
    .await;
    let client = reqwest::Client::new();
    let response = client
        .post(format!(
            "{}/api/wings/load/rooms/notes/drawers",
            daemon.base_url
        ))
        .json(&json!({ "content": "a drawer awaiting an embedding" }))
        .send()
        .await
        .unwrap();
    assert!(response.status().is_success());
    let embed = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let jobs: Vec<Job> = client
                .get(format!("{}/api/jobs?kind=embed&limit=5", daemon.base_url))
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            if let Some(job) = jobs.iter().find(|job| job.status == JobStatus::Running) {
                break job.id;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("embedding sweep started");
    let session = tokio::time::timeout(
        Duration::from_secs(5),
        serve_client(
            ClientConfig::default(),
            StreamableHttpClientTransport::from_uri(format!("{}/mcp", daemon.base_url)),
        ),
    )
    .await
    .expect("MCP connects while embedding runs")
    .expect("MCP initializes");
    let status = tokio::time::timeout(
        Duration::from_secs(5),
        client.get(format!("{}/api/status", daemon.base_url)).send(),
    )
    .await
    .expect("REST status while embedding runs")
    .unwrap();
    assert!(status.status().is_success());
    assert_ne!(
        tokio::time::timeout(
            Duration::from_secs(5),
            session
                .peer()
                .call_tool(CallToolRequestParams::new("memcastle_status")),
        )
        .await
        .expect("MCP status while embedding runs")
        .unwrap()
        .is_error,
        Some(true)
    );
    assert_eq!(
        get_job(&client, &daemon.base_url, embed).await.status,
        JobStatus::Running
    );
    let root = tempfile::tempdir().unwrap();
    let mut jobs = Vec::new();
    for run in 0..3 {
        let dir = root.path().join(format!("run-{run}"));
        std::fs::create_dir(&dir).unwrap();
        for n in 0..16 {
            std::fs::write(
                dir.join(format!("{n:03}.txt")),
                format!("document {run} {n} {}", "castle stone ".repeat(80)),
            )
            .unwrap();
        }
        let job: Job = client
            .post(format!("{}/api/jobs", daemon.base_url))
            .json(&json!({ "type": "mine", "path": dir, "requested_by": "load-test" }))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        jobs.push(job.id);
    }
    wait_for_job_status(&client, &daemon.base_url, jobs[0], JobStatus::Running).await;
    // Check the capacity, not *which* of the short mines is queued: one can complete
    // between polls on a fast host, releasing a slot for the third.
    let states: Vec<Job> = client
        .get(format!("{}/api/jobs?kind=mine&limit=3", daemon.base_url))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(states.len(), 3);
    assert!(
        states
            .iter()
            .filter(|job| job.status == JobStatus::Running)
            .count()
            <= 2
    );
    if states.iter().all(|job| job.status != JobStatus::Completed) {
        assert!(states.iter().any(|job| job.status == JobStatus::Queued));
    }

    for _ in 0..3 {
        let started = Instant::now();
        for path in ["/api/health", "/api/status", "/api/jobs?limit=5"] {
            let response = tokio::time::timeout(
                Duration::from_secs(5),
                client.get(format!("{}{path}", daemon.base_url)).send(),
            )
            .await
            .unwrap_or_else(|_| panic!("{path} stalled under mining load"))
            .unwrap();
            assert!(
                response.status().is_success(),
                "{path}: {}",
                response.status()
            );
            tokio::time::timeout(Duration::from_secs(5), response.bytes())
                .await
                .expect("REST body arrived")
                .unwrap();
        }
        let answer = tokio::time::timeout(
            Duration::from_secs(5),
            session
                .peer()
                .call_tool(CallToolRequestParams::new("memcastle_status")),
        )
        .await
        .expect("MCP status did not stall")
        .expect("MCP status answered");
        assert_ne!(answer.is_error, Some(true));
        eprintln!("active-job REST/MCP round elapsed: {:?}", started.elapsed());
    }
    for id in jobs {
        let completed =
            wait_for_job_status(&client, &daemon.base_url, id, JobStatus::Completed).await;
        assert_eq!(completed.result.as_ref().unwrap()["documents"], 16);
    }
    wait_for_job_status(&client, &daemon.base_url, embed, JobStatus::Completed).await;
    let _ = session.cancel().await;
    daemon.shutdown().await;
    provider_handle.abort();
}

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
    let checkpoint = wait_for_job_status(
        &client,
        &daemon.base_url,
        checkpoint.id,
        JobStatus::Completed,
    )
    .await;
    // Compare completion times, not "is mine still incomplete right now":
    // both jobs are near-instant, so on a loaded machine the poll above can
    // return after the mine job has also finished, which says nothing about
    // which one was claimed first.
    let mine = get_job(&client, &daemon.base_url, mine.id).await;
    if mine.status == JobStatus::Completed {
        assert!(
            checkpoint.completed_at <= mine.completed_at,
            "the background mine job completed ({:?}) before the critical checkpoint ({:?})",
            mine.completed_at,
            checkpoint.completed_at
        );
    }

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

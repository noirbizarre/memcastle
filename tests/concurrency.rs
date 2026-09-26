//! Proves multiple clients can hit the same daemon/store concurrently
//! without corrupting state — the concurrency invariant the architecture
//! doc calls out explicitly.

mod common;

use common::TestDaemon;
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

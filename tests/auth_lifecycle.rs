//! The authentication lifecycle across real processes: generate a token while authentication is off,
//! enable it and restart, then drive the daemon through the `memcastle` CLI exactly as a user would
//! (`docs/adr/014-optional-token-authentication.md`).
//!
//! Subprocesses, not an in-process daemon: the verifier must survive a *restart*, and SurrealKV's file lock
//! is not released within one process (see `tests/persistence.rs`).
//! The in-process, HTTP-level behaviour (every route guarded, MCP exclusion, rotation) is in `tests/auth.rs`.

use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};
use std::time::Duration;

use assert_cmd::cargo::cargo_bin;
use memcastle::server::lifecycle::RuntimeInfo;
use tokio::process::{Child, Command};

/// Long enough for a cold, instrumented CI runner.
const STEP_TIMEOUT: Duration = Duration::from_secs(60);

/// Every `MEMCASTLE_*` setting that could leak in from a developer's shell.
const AMBIENT: [&str; 7] = [
    "MEMCASTLE_CONFIG",
    "MEMCASTLE_BIND",
    "MEMCASTLE_PORT",
    "MEMCASTLE_MODE",
    "MEMCASTLE_LOG",
    "MEMCASTLE_AUTH_ENABLED",
    "MEMCASTLE_AUTH_TOKEN",
];

/// Everything one test needs: a root under which the palace, the XDG directories and the daemon's log all live.
struct Sandbox {
    root: tempfile::TempDir,
}

impl Sandbox {
    fn new() -> Self {
        Self {
            root: tempfile::tempdir().expect("tempdir"),
        }
    }

    fn root(&self) -> &Path {
        self.root.path()
    }

    fn palace(&self) -> PathBuf {
        self.root().join("palace")
    }

    fn log(&self) -> PathBuf {
        self.root().join("daemon.log")
    }

    /// A `memcastle` command scoped to this sandbox, presenting `token` (if any) through the environment,
    /// which is the only way the CLI accepts one.
    fn memcastle(&self, token: Option<&str>) -> Command {
        let mut cmd = Command::new(cargo_bin("memcastle"));
        for name in AMBIENT {
            cmd.env_remove(name);
        }
        cmd.env("MEMCASTLE_PALACE_PATH", self.palace())
            .env("XDG_CONFIG_HOME", self.root().join("config"))
            .env("XDG_DATA_HOME", self.root().join("data"))
            .env("XDG_STATE_HOME", self.root().join("state"))
            .stdin(Stdio::null());
        if let Some(token) = token {
            cmd.env("MEMCASTLE_AUTH_TOKEN", token);
        }
        cmd
    }

    /// The registry entry of the daemon started in this sandbox, once it has written one.
    fn registered(&self) -> Option<RuntimeInfo> {
        let run = self.root().join("state/memcastle/run");
        for entry in std::fs::read_dir(run).ok()?.flatten() {
            if let Ok(text) = std::fs::read_to_string(entry.path().join("daemon.json"))
                && let Ok(info) = serde_json::from_str(&text)
            {
                return Some(info);
            }
        }
        None
    }

    /// Start `memcastle serve`, with its log in a file (a piped stderr nobody drains would block a chatty daemon).
    /// `auth_enabled` and `daemon_token` are given through the environment; debug logging is on
    /// so the "the log never contains a token" assertions have something to find.
    async fn start(&self, auth_enabled: bool, daemon_token: Option<&str>) -> Child {
        let log = std::fs::File::create(self.log()).expect("create the daemon log");
        let mut cmd = self.memcastle(daemon_token);
        cmd.arg("serve")
            .env("MEMCASTLE_BIND", "127.0.0.1")
            .env("MEMCASTLE_PORT", "0")
            .env("MEMCASTLE_LOG", "info,memcastle=debug,tower_http=debug")
            .stdout(Stdio::null())
            .stderr(Stdio::from(log))
            .kill_on_drop(true);
        if auth_enabled {
            cmd.env("MEMCASTLE_AUTH_ENABLED", "true");
        }
        let mut child = cmd.spawn().expect("spawn `memcastle serve`");

        for _ in 0..1200 {
            if self.registered().is_some() {
                return child;
            }
            if let Ok(Some(status)) = child.try_wait() {
                panic!("daemon exited with {status}; log:\n{}", self.read_log());
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!(
            "daemon did not register within 60s; log:\n{}",
            self.read_log()
        );
    }

    fn read_log(&self) -> String {
        std::fs::read_to_string(self.log()).unwrap_or_default()
    }

    /// Run the CLI with `args`, presenting `token`.
    async fn run(&self, token: Option<&str>, args: &[&str]) -> Output {
        tokio::time::timeout(STEP_TIMEOUT, self.memcastle(token).args(args).output())
            .await
            .expect("the command finishes in time")
            .expect("run `memcastle`")
    }

    /// Ask the daemon to stop through the CLI and wait for the process to exit,
    /// so the palace lock is released before the next daemon opens it.
    async fn stop(&self, child: &mut Child, token: Option<&str>) {
        let output = self.run(token, &["stop"]).await;
        assert!(
            output.status.success(),
            "`stop` failed: {}",
            text(&output.stderr)
        );
        tokio::time::timeout(Duration::from_secs(30), child.wait())
            .await
            .expect("daemon exits after being asked to stop")
            .expect("wait for daemon");
    }

    /// Every file under the sandbox whose bytes contain `needle`.
    fn files_containing(&self, needle: &str) -> Vec<PathBuf> {
        fn walk(dir: &Path, needle: &[u8], found: &mut Vec<PathBuf>) {
            let Ok(entries) = std::fs::read_dir(dir) else {
                return;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    walk(&path, needle, found);
                } else if let Ok(bytes) = std::fs::read(&path)
                    && bytes.windows(needle.len()).any(|window| window == needle)
                {
                    found.push(path);
                }
            }
        }
        let mut found = Vec::new();
        walk(self.root(), needle.as_bytes(), &mut found);
        found
    }
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// The token `auth generate` printed: exactly one line, nothing else on stdout.
fn generated_token(output: &Output) -> String {
    assert!(
        output.status.success(),
        "`auth generate` failed: {}",
        text(&output.stderr)
    );
    let stdout = text(&output.stdout);
    assert_eq!(
        stdout.lines().count(),
        1,
        "stdout must be the token alone: {stdout:?}"
    );
    let token = stdout.trim().to_string();
    assert!(
        token.starts_with("mc_") && token.len() == "mc_".len() + 64,
        "{token:?}"
    );
    token
}

#[tokio::test]
async fn a_generated_token_survives_a_restart_and_then_gates_every_cli_command() {
    let sandbox = Sandbox::new();

    // 1. Authentication is off: generate a token through the CLI.
    let mut daemon = sandbox.start(false, None).await;
    let generated = sandbox.run(None, &["auth", "generate"]).await;
    let token = generated_token(&generated);
    let guidance = text(&generated.stderr);
    assert!(guidance.contains("shown once"), "{guidance}");
    assert!(
        !guidance.contains(&token),
        "the guidance must not repeat the token"
    );
    assert!(sandbox.run(None, &["status"]).await.status.success());
    sandbox.stop(&mut daemon, None).await;

    // 2. Restart with authentication on and *only* the stored verifier to check against.
    let mut daemon = sandbox.start(true, None).await;

    // 3. The CLI is refused without a token, and the refusal is an authentication failure,
    //    not "no daemon is running" (status exit code 3).
    let anonymous = sandbox.run(None, &["status"]).await;
    assert_eq!(
        anonymous.status.code(),
        Some(1),
        "{}",
        text(&anonymous.stderr)
    );
    let stderr = text(&anonymous.stderr);
    assert!(
        stderr.contains("401") && stderr.contains("memcastle::auth::unauthorized"),
        "{stderr}"
    );
    assert!(!stderr.contains("not running"), "{stderr}");

    let wrong = sandbox
        .run(Some("mc_not_the_token_0000000000"), &["search", "x"])
        .await;
    assert!(!wrong.status.success());
    assert!(
        text(&wrong.stderr).contains("401"),
        "{}",
        text(&wrong.stderr)
    );

    // 4. The generated token, persisted as a verifier across the restart, is accepted.
    let ok = sandbox.run(Some(&token), &["status"]).await;
    assert!(ok.status.success(), "{}", text(&ok.stderr));
    assert!(text(&ok.stdout).contains("enabled"), "{}", text(&ok.stdout));
    assert!(
        sandbox
            .run(Some(&token), &["search", "x"])
            .await
            .status
            .success()
    );

    // 5. Generating another token is itself authenticated, and rotates the first one out.
    let refused = sandbox.run(None, &["auth", "generate"]).await;
    assert!(
        !refused.status.success(),
        "generating must need a token once enabled"
    );
    assert!(
        text(&refused.stdout).is_empty(),
        "a refused generate must print no token"
    );
    let rotated = generated_token(&sandbox.run(Some(&token), &["auth", "generate"]).await);
    assert_ne!(rotated, token);
    let stale = sandbox.run(Some(&token), &["status"]).await;
    assert!(
        !stale.status.success(),
        "the rotated-out token must stop working"
    );
    assert!(
        sandbox
            .run(Some(&rotated), &["status"])
            .await
            .status
            .success()
    );

    // 6. Revocation ends the token at once.
    let revoked = sandbox.run(Some(&rotated), &["auth", "revoke"]).await;
    assert!(revoked.status.success(), "{}", text(&revoked.stderr));
    assert!(text(&revoked.stdout).contains("\"revoked\": true"));
    assert!(
        !sandbox
            .run(Some(&rotated), &["status"])
            .await
            .status
            .success()
    );

    // With nothing left to authenticate against, the daemon cannot be asked to stop:
    // the documented way back in is to restart it with authentication off.
    daemon.kill().await.expect("kill the locked-out daemon");

    // 7. None of it reached the daemon's log, even at debug level.
    let log = sandbox.read_log();
    for secret in [&token, &rotated] {
        assert!(
            !log.contains(secret.as_str()),
            "a token leaked into the daemon log"
        );
        assert!(
            !log.contains(&secret["mc_".len()..]),
            "a token's entropy leaked into the daemon log"
        );
    }
    // The daemon's own messages say "bearer token" in prose, so look for what a
    // logged header would actually contain: its name, or a credential after the scheme.
    let lowered = log.to_lowercase();
    assert!(
        !lowered.contains("authorization"),
        "an Authorization header was logged"
    );
    assert!(
        !lowered.contains("bearer mc_"),
        "a bearer credential was logged"
    );
}

#[tokio::test]
async fn the_database_holds_a_digest_of_a_generated_token_and_never_the_token() {
    let sandbox = Sandbox::new();
    let mut daemon = sandbox.start(false, None).await;
    let token = generated_token(&sandbox.run(None, &["auth", "generate"]).await);
    sandbox.stop(&mut daemon, None).await;

    let leaked = sandbox.files_containing(&token);
    assert!(
        leaked.is_empty(),
        "the plaintext token was persisted in {leaked:?}"
    );
    // Nor the entropy without its prefix (what a hex dump of a file would match).
    let leaked = sandbox.files_containing(&token["mc_".len()..]);
    assert!(
        leaked.is_empty(),
        "the token's entropy was persisted in {leaked:?}"
    );
}

#[tokio::test]
async fn an_environment_secret_is_accepted_and_written_nowhere() {
    const SECRET: &str = "mc_an_environment_secret_that_must_never_be_written_down";
    let sandbox = Sandbox::new();

    let mut daemon = sandbox.start(true, Some(SECRET)).await;

    // The CLI needs it too, and gets it from the environment alone.
    let anonymous = sandbox.run(None, &["status"]).await;
    assert_eq!(anonymous.status.code(), Some(1));
    let with_secret = sandbox.run(Some(SECRET), &["status", "--json"]).await;
    assert!(
        with_secret.status.success(),
        "{}",
        text(&with_secret.stderr)
    );
    assert!(
        !text(&with_secret.stdout).contains(SECRET),
        "status must never print the secret"
    );
    assert!(text(&with_secret.stdout).contains("\"auth_enabled\": true"));

    sandbox.stop(&mut daemon, Some(SECRET)).await;

    // Not in the palace, the registry file, any generated config, or the daemon's log.
    let leaked = sandbox.files_containing(SECRET);
    assert!(
        leaked.is_empty(),
        "the environment secret was written to {leaked:?}"
    );
}

#[tokio::test]
async fn enabling_authentication_with_nothing_to_check_against_refuses_to_start() {
    let sandbox = Sandbox::new();

    let mut cmd = sandbox.memcastle(None);
    let output = tokio::time::timeout(
        STEP_TIMEOUT,
        cmd.arg("serve")
            .env("MEMCASTLE_BIND", "127.0.0.1")
            .env("MEMCASTLE_PORT", "0")
            .env("MEMCASTLE_AUTH_ENABLED", "true")
            .kill_on_drop(true)
            .output(),
    )
    .await
    .expect("the daemon exits instead of serving")
    .expect("run `memcastle serve`");

    assert!(!output.status.success());
    let stderr = text(&output.stderr);
    assert!(
        stderr.contains("memcastle::auth::not_configured"),
        "{stderr}"
    );
    assert!(
        sandbox.registered().is_none(),
        "a refused start must not register as serving"
    );
}

#[tokio::test]
async fn a_too_short_environment_secret_is_rejected_without_echoing_it() {
    let sandbox = Sandbox::new();

    let output = sandbox.run(Some("hunter2"), &["status"]).await;

    assert!(!output.status.success());
    let stderr = text(&output.stderr);
    assert!(stderr.contains("MEMCASTLE_AUTH_TOKEN"), "{stderr}");
    assert!(!stderr.contains("hunter2"), "{stderr}");
}

#[tokio::test]
async fn a_token_used_to_sign_in_to_the_database_endpoint_is_written_nowhere() {
    use futures::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message;

    const SECRET: &str = "mc_a_secret_for_the_database_endpoint_log_check";
    const WRONG: &str = "mc_a_wrong_token_that_must_not_be_logged_either";
    let sandbox = Sandbox::new();
    let mut daemon = sandbox.start(true, Some(SECRET)).await;

    // Opening the console is itself an authenticated, CLI-driven request.
    let started = sandbox
        .run(Some(SECRET), &["db", "serve", "--port", "0", "--json"])
        .await;
    assert!(started.status.success(), "{}", text(&started.stderr));
    let report: serde_json::Value = serde_json::from_slice(&started.stdout).expect("json");
    assert_eq!(report["auth_required"], true, "{report}");
    let addr = report["addr"].as_str().expect("addr").to_string();

    // The way Studio does it: the token travels in-band, as the password.
    let (mut socket, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/rpc"))
        .await
        .expect("connect");
    for password in [WRONG, SECRET] {
        let request = serde_json::json!({
            "id": 1, "method": "signin", "params": [{ "user": "memcastle", "pass": password }],
        });
        socket
            .send(Message::Text(request.to_string().into()))
            .await
            .expect("send");
        socket.next().await.expect("an answer").expect("a frame");
    }
    let query = serde_json::json!({ "id": 2, "method": "query", "params": ["RETURN 1"] });
    socket
        .send(Message::Text(query.to_string().into()))
        .await
        .expect("send");
    let Message::Text(answer) = socket.next().await.expect("an answer").expect("a frame") else {
        panic!("a text answer");
    };
    assert!(
        answer.contains("\"OK\""),
        "signed in with the token: {answer}"
    );
    drop(socket);

    let stopped = sandbox.run(Some(SECRET), &["db", "stop"]).await;
    assert!(stopped.status.success(), "{}", text(&stopped.stderr));
    sandbox.stop(&mut daemon, Some(SECRET)).await;

    let log = sandbox.read_log();
    for token in [SECRET, WRONG] {
        assert!(!log.contains(token), "a token leaked into the daemon log");
        assert!(
            !log.contains(&token["mc_".len()..]),
            "a token's entropy leaked into the daemon log"
        );
        let leaked = sandbox.files_containing(token);
        assert!(leaked.is_empty(), "a token was written to {leaked:?}");
    }
    // The endpoint did log that it was used, so the assertions above had something to scan.
    assert!(
        log.contains("database admin connection opened"),
        "the connection was logged, without its content:\n{log}"
    );
}

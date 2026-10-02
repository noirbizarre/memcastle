//! The memcastle binary.
//!
//! Every subcommand is either `serve`/`daemon` (which runs the actual
//! engine, via `memcastle::server::run`) or a thin `DaemonClient` call — see
//! `memcastle::app`'s doc comment for why that split is the whole point of
//! this architecture. The exceptions: `migrate` opens storage itself (it must
//! work before a daemon exists), and `restart` additionally manages the daemon
//! process (registry file plus respawn) without touching `store` or `jobs`.

#![allow(clippy::result_large_err)]

use std::process::ExitCode;

use clap::Parser;
use miette::MietteHandlerOpts;

mod cli;

use cli::{
    AuditArgs, AuthCommand, CheckpointArgs, Cli, Command, DbCommand, DiaryCommand, JobsCommand,
    MigrateArgs, MineArgs, RecallArgs, RepairArgs, SearchArgs, ServeArgs, StatusArgs, WakeUpArgs,
};
use memcastle::app::{DbEndpointRequest, DbEndpointStatus, WakeUpBudget};
use memcastle::client::{DaemonClient, StatusView};
use memcastle::config::{Config, Overrides};
use memcastle::domain::MemoryMode;
use memcastle::store::SurrealStore;
use memcastle::{Error, Result};

/// Not `#[tokio::main]`: that runs the runtime's `block_on` — and so, for
/// any `.await` chain deep enough to still be on its calling stack rather
/// than truly suspended, the bulk of its stack usage — on the OS's actual
/// main thread. Windows defaults that to 1 MiB (Unix: 8 MiB), and a debug
/// build of `memcastle serve` reliably overflows it during startup:
/// SurrealDB 3.x split what was one crate into a dozen thin layers
/// (`surrealdb` -> `surrealdb-engine-local` -> `surrealdb-core` ->
/// `surrealdb-kvs-any` -> `surrealdb-kvs-surrealkv` -> ...), and an
/// uninlined debug build pays for every one of those layers in stack
/// frames on the way down. Spawning a thread with a generous, explicit
/// stack size — and configuring the same for the runtime's worker
/// threads, since a `tokio::spawn`'d task (the scheduler's dispatch loop,
/// a running job) can hit the same call chain — sidesteps the platform
/// default entirely rather than trying to outsmart exactly how deep it
/// needs to be.
fn main() -> ExitCode {
    const STACK_SIZE: usize = 16 * 1024 * 1024;
    std::thread::Builder::new()
        .stack_size(STACK_SIZE)
        .spawn(|| {
            tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .thread_stack_size(STACK_SIZE)
                .build()
                .expect("build the tokio runtime")
                .block_on(async_main())
        })
        .expect("spawn the main thread")
        .join()
        .expect("main thread panicked")
}

async fn async_main() -> ExitCode {
    let args = Cli::parse();
    let verbose = args.verbose > 0 || std::env::var_os("RUST_BACKTRACE").is_some();
    install_miette_hook(verbose);

    // Config is loaded before tracing so `logging.level` can drive the
    // filter. A config that fails to load still gets a working logger (at
    // the default level) — the failure itself is reported through miette
    // below, not tracing, so nothing is lost by initializing second.
    let config = Config::load(args.config.as_deref(), &overrides_from(&args));
    // A config that failed to load still honours `-v`, on top of the default.
    init_tracing(
        match &config {
            Ok(config) => config.log_filter(args.verbose),
            Err(_) => Config::default().log_filter(args.verbose),
        },
        config
            .as_ref()
            .map(|c| c.logging.format)
            .unwrap_or_default(),
    );

    let outcome = match config {
        Ok(config) => run(args, config).await,
        Err(error) => Err(error),
    };
    match outcome {
        Ok(code) => code,
        Err(error) => {
            eprintln!("{:?}", miette::Report::new(error));
            ExitCode::FAILURE
        }
    }
}

/// The command-line layer of configuration. Built once, before the config is
/// loaded, so that every command resolves the palace and address the same way
/// (`--bind` used to be applied to `serve` alone, after loading).
fn overrides_from(args: &Cli) -> Overrides {
    let (bind, port, assets_dir) = match &args.command {
        Command::Serve(serve) | Command::Restart(serve) => {
            (serve.bind, serve.port, serve.assets_dir.clone())
        }
        _ => (None, None, None),
    };
    Overrides {
        palace: args.palace.clone(),
        bind,
        port,
        assets_dir,
    }
}

/// Dispatch a command to its exit code. Only `status` has more than
/// success/failure to say (see [`cmd_status`]), so it is split off here and
/// every other command keeps returning a plain `Result<()>`.
async fn run(args: Cli, config: Config) -> Result<ExitCode> {
    match args.command {
        Command::Status(status) => cmd_status(&config, args.mode, &status).await,
        command => run_command(command, args.mode, args.config.as_deref(), config)
            .await
            .map(|()| ExitCode::SUCCESS),
    }
}

async fn run_command(
    command: Command,
    mode: Option<MemoryMode>,
    config_file: Option<&std::path::Path>,
    config: Config,
) -> Result<()> {
    match command {
        Command::Serve(_) => memcastle::server::run(config).await,
        Command::Migrate(args) => cmd_migrate(&config, args).await,
        // Handled by `run`, which needs the exit code; the arm exists only so
        // this match stays exhaustive and a new command cannot be forgotten.
        Command::Status(_) => Ok(()),
        Command::Stop => cmd_stop(&config, mode).await,
        Command::Restart(restart_args) => {
            cmd_restart(&config, config_file, mode, restart_args).await
        }
        Command::Search(args) => cmd_search(&config, mode, args).await,
        Command::Recall(args) => cmd_recall(&config, mode, args).await,
        Command::WakeUp(args) => cmd_wake_up(&config, mode, args).await,
        Command::Mine(args) => cmd_mine(&config, mode, args).await,
        Command::Checkpoint(args) => cmd_checkpoint(&config, mode, args).await,
        Command::Audit(args) => cmd_audit(&config, mode, args).await,
        Command::Repair(args) => cmd_repair(&config, mode, args).await,
        Command::Diary(cmd) => cmd_diary(&config, mode, cmd).await,
        Command::Jobs(jobs) => cmd_jobs(&config, mode, jobs).await,
        Command::Auth(auth) => cmd_auth(&config, auth).await,
        Command::Db(db) => cmd_db(&config, db).await,
        Command::Wings => Err(Error::not_implemented("memcastle wings")),
        Command::Rooms => Err(Error::not_implemented("memcastle rooms")),
        Command::Drawers => Err(Error::not_implemented("memcastle drawers")),
        Command::Maintenance => Err(Error::not_implemented("memcastle maintenance")),
    }
}

/// A client for the daemon this palace's config points at, sending `mode`
/// (from `--mode`/`MEMCASTLE_MODE`) with every request when one was given.
///
/// The configured token (`auth.token`/`MEMCASTLE_AUTH_TOKEN`) is presented on
/// every request. It is never a command-line argument, so it cannot end up in
/// shell history or the process list.
fn client(config: &Config, mode: Option<MemoryMode>) -> DaemonClient {
    let daemon = DaemonClient::discover(&config.palace.path, config.server.socket_addr())
        .with_token(config.auth.token.clone());
    match mode {
        Some(mode) => daemon.with_mode(mode),
        None => daemon,
    }
}

/// Print `value` as pretty JSON — or fail loudly. A serialization error used
/// to print an empty line and exit 0, which a script reads as "no results".
fn print_json(value: &impl serde::Serialize) -> Result<()> {
    let text = serde_json::to_string_pretty(value)
        .map_err(|source| Error::serialization("the command's output", source))?;
    println!("{text}");
    Ok(())
}

/// Report on the daemon, and exit with what the report means: 0 healthy,
/// 1 degraded, 3 not running (see `memcastle::client::status`). A stopped
/// daemon is a normal answer here rather than a `not_running` diagnostic, so
/// the report (endpoint, palace, how to start) is printed on stdout.
async fn cmd_status(
    config: &Config,
    mode: Option<MemoryMode>,
    args: &StatusArgs,
) -> Result<ExitCode> {
    let view = StatusView::collect(
        &config.palace.path,
        config.server.socket_addr(),
        mode,
        config.auth.token.clone(),
    )
    .await?;
    if args.json {
        print_json(&view)?;
    } else {
        println!("{}", view.render_human());
    }
    Ok(ExitCode::from(view.exit_code()))
}

/// Connects to storage directly, like `Command::Serve` — a second,
/// narrow exception to "every non-`serve` subcommand only calls
/// `DaemonClient`" (see `memcastle::migrate`'s module doc), since migration
/// must work without, and before, a daemon exists.
async fn cmd_migrate(config: &Config, args: MigrateArgs) -> Result<()> {
    let backend = config.store.clone().into_backend(&config.palace.path);
    let store = SurrealStore::connect(&backend).await?;

    if args.check || args.status {
        let status = memcastle::migrate::status(&store).await?;
        print_json(&status)?;
        if args.check && !status.pending.is_empty() {
            return Err(Error::MigrationsPending {
                count: status.pending.len(),
                pending: status.pending.join(", "),
            });
        }
        return Ok(());
    }

    let report = memcastle::migrate::run(&store).await?;
    print_json(&report)?;
    Ok(())
}

async fn cmd_stop(config: &Config, mode: Option<MemoryMode>) -> Result<()> {
    print_json(&client(config, mode).shutdown().await?)?;
    Ok(())
}

/// Stop the running daemon (if any), start a fresh one with the flags this
/// command was given, and wait until it is actually serving.
///
/// "Actually serving" is the registry file appearing with a live daemon
/// behind it — the same signal every other command discovers the daemon by —
/// not the process merely having been spawned: reporting success before that
/// told users a daemon that had crashed on startup was up. The new daemon
/// gets the original `--config`, `--bind`, `--port` and `--assets-dir` and the resolved `--palace`,
/// because a bare `memcastle serve` would silently come back on the default
/// address with the default config.
async fn cmd_restart(
    config: &Config,
    config_path: Option<&std::path::Path>,
    mode: Option<MemoryMode>,
    args: ServeArgs,
) -> Result<()> {
    let daemon = client(config, mode);
    if daemon.health().await {
        daemon.shutdown().await?;
        // Wait for the old daemon to be *gone*, not merely to stop answering:
        // its listener closes as soon as shutdown begins, but it then drains
        // running jobs (up to `jobs.drain_timeout_secs`) while still holding
        // the palace's file lock, and a new daemon started in that window
        // dies on the lock. The registry file is removed only once the drain
        // is over, which makes it the signal that matters. Bounded, so a
        // stuck shutdown cannot hang this command.
        let registry = memcastle::server::lifecycle::registry_path(&config.palace.path);
        for _ in 0..600 {
            if !registry.exists() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    }

    let exe = std::env::current_exe().map_err(|source| Error::io("current executable", source))?;
    // The old process removes its registry file just before it exits, so
    // its lock can outlive that by a moment: a new daemon that dies at once
    // is retried a couple of times before it is reported as failed.
    const ATTEMPTS: u32 = 3;
    let mut last_failure = String::new();
    for attempt in 1..=ATTEMPTS {
        let mut command = std::process::Command::new(&exe);
        command.arg("serve");
        if let Some(path) = config_path {
            command.arg("--config").arg(path);
        }
        if let Some(bind) = args.bind {
            command.arg("--bind").arg(bind.to_string());
        }
        // Forwarded like `--bind`: without it the respawned daemon would
        // fall back to the configured port while this command waits on the
        // one that was asked for.
        if let Some(port) = args.port {
            command.arg("--port").arg(port.to_string());
        }
        // Forwarded for the same reason: a developer's local asset build
        // would otherwise be silently replaced by the installed one.
        if let Some(dir) = &args.assets_dir {
            command.arg("--assets-dir").arg(dir);
        }
        // Always the resolved palace, not just an explicit `--palace`: the
        // respawned daemon must serve exactly the palace this command
        // found and waits on, whichever layer (file, environment, flag)
        // named it.
        command.arg("--palace").arg(&config.palace.path);
        // Detached from this terminal: the new daemon outlives this command,
        // and an inherited stderr would interleave its log with the prompt.
        let mut child = command
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|source| Error::io("memcastle serve", source))?;

        // A daemon takes a couple of seconds to open SurrealDB and migrate; a
        // debug build on a slow disk takes far longer, hence the generous
        // bound.
        for _ in 0..600 {
            if let Some(info) = memcastle::server::lifecycle::read_if_live(&config.palace.path) {
                println!(
                    "restarted: memcastle is serving on http://{}",
                    info.bind_addr
                );
                return Ok(());
            }
            if let Some(status) = child
                .try_wait()
                .map_err(|source| Error::io("memcastle serve", source))?
            {
                last_failure =
                    format!("the new daemon exited with {status} before it started serving");
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        if last_failure.is_empty() {
            return Err(Error::server(
                "the new daemon did not start serving within 60 seconds; run `memcastle serve` \
                 in the foreground to see what it is waiting on",
            ));
        }
        if attempt < ATTEMPTS {
            last_failure.clear();
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        }
    }
    Err(Error::server(format!(
        "{last_failure} ({ATTEMPTS} attempts); run `memcastle serve` in the foreground to see why"
    )))
}

async fn cmd_search(config: &Config, mode: Option<MemoryMode>, args: SearchArgs) -> Result<()> {
    let hits = client(config, mode)
        .search(
            &args.query,
            args.wing.as_deref(),
            args.room.as_deref(),
            args.limit,
        )
        .await?;
    print_json(&hits)?;
    Ok(())
}

async fn cmd_recall(config: &Config, mode: Option<MemoryMode>, args: RecallArgs) -> Result<()> {
    let hits = client(config, mode)
        .recall(&args.query, args.wing.as_deref(), args.limit)
        .await?;
    print_json(&hits)?;
    Ok(())
}

async fn cmd_wake_up(config: &Config, mode: Option<MemoryMode>, args: WakeUpArgs) -> Result<()> {
    let budget = WakeUpBudget::from_options(args.max_items, args.max_bytes);
    let context = client(config, mode)
        .wake_up(&args.agent_identity, args.wing.as_deref(), budget)
        .await?;
    print_json(&context)?;
    Ok(())
}

async fn cmd_mine(config: &Config, mode: Option<MemoryMode>, args: MineArgs) -> Result<()> {
    // Made absolute here, against *this* shell's working directory: the
    // daemon would otherwise resolve `./project` against its own, which is
    // wherever it was started and usually not where the user is standing.
    let path = std::path::absolute(&args.path)
        .map_err(|source| Error::io(args.path.display().to_string(), source))?;
    let job = client(config, mode).submit_mine(path, args.wing).await?;
    print_json(&job)?;
    Ok(())
}

async fn cmd_checkpoint(
    config: &Config,
    mode: Option<MemoryMode>,
    args: CheckpointArgs,
) -> Result<()> {
    let raw = match &args.payload {
        Some(path) => std::fs::read_to_string(path)
            .map_err(|source| Error::io(path.display().to_string(), source))?,
        None => {
            // No file given: read the payload from stdin, so `memcastle
            // checkpoint` composes with a pipe (`echo '{...}' | memcastle
            // checkpoint`) the same way `--payload -` would on tools that
            // support it.
            use std::io::Read;
            let mut buf = String::new();
            std::io::stdin()
                .read_to_string(&mut buf)
                .map_err(|source| Error::io("<stdin>", source))?;
            buf
        }
    };
    // Parsed client-side, before ever contacting the daemon: a malformed
    // payload should fail fast with a clear local error, not round-trip to
    // the API just to bounce back as a generic 400.
    let payload: memcastle::domain::CheckpointPayload = serde_json::from_str(&raw)
        .map_err(|source| Error::invalid_input("payload", source.to_string()))?;
    let job = client(config, mode)
        .submit_checkpoint(payload, args.emergency)
        .await?;
    print_json(&job)?;
    Ok(())
}

async fn cmd_audit(config: &Config, mode: Option<MemoryMode>, args: AuditArgs) -> Result<()> {
    let job = client(config, mode).submit_audit(args.scope).await?;
    print_json(&job)?;
    Ok(())
}

async fn cmd_repair(config: &Config, mode: Option<MemoryMode>, args: RepairArgs) -> Result<()> {
    // Parsed client-side, before ever contacting the daemon — same
    // reasoning as `parse_job_id`'s other call sites in `cmd_jobs`.
    let based_on_job = args
        .based_on_job
        .as_deref()
        .map(Error::parse_job_id)
        .transpose()?;
    let job = client(config, mode)
        .submit_repair(!args.apply, based_on_job)
        .await?;
    print_json(&job)?;
    Ok(())
}

async fn cmd_diary(config: &Config, mode: Option<MemoryMode>, command: DiaryCommand) -> Result<()> {
    let daemon = client(config, mode);
    match command {
        DiaryCommand::Write {
            agent_identity,
            wing,
            content,
        } => {
            let drawer = daemon.diary_write(&agent_identity, &wing, content).await?;
            print_json(&drawer)?;
        }
        DiaryCommand::Read {
            agent_identity,
            wing,
            limit,
        } => {
            let entries = daemon.diary_read(&agent_identity, &wing, limit).await?;
            print_json(&entries)?;
        }
    }
    Ok(())
}

async fn cmd_jobs(config: &Config, mode: Option<MemoryMode>, command: JobsCommand) -> Result<()> {
    let daemon = client(config, mode);
    match command {
        JobsCommand::List { status } => {
            let status = status.map(|s| Error::parse_job_status(&s)).transpose()?;
            print_json(&daemon.list_jobs(status).await?)?;
        }
        JobsCommand::Show { id } => print_json(&daemon.get_job(Error::parse_job_id(&id)?).await?)?,
        JobsCommand::Pause { id } => {
            print_json(&daemon.pause_job(Error::parse_job_id(&id)?).await?)?;
        }
        JobsCommand::Resume { id } => {
            print_json(&daemon.resume_job(Error::parse_job_id(&id)?).await?)?;
        }
        JobsCommand::Cancel { id } => {
            print_json(&daemon.cancel_job(Error::parse_job_id(&id)?).await?)?;
        }
        JobsCommand::Retry { id } => {
            print_json(&daemon.retry_job(Error::parse_job_id(&id)?).await?)?;
        }
        JobsCommand::Demo { steps } => {
            // The daemon has no "submit a demo job" REST endpoint of its
            // own reachable from here without going through `/api/jobs`
            // with a `demo` kind — reuse the same generic endpoint the
            // `mine` command uses, just with a different JSON body.
            let job: memcastle::domain::Job = daemon.submit_demo(steps).await?;
            print_json(&job)?;
        }
    }
    Ok(())
}

/// Manage the daemon's bearer token. No `--mode`: a memory mode is a session's
/// privilege over memory, and this is an administrative operation on
/// credentials.
async fn cmd_auth(config: &Config, command: AuthCommand) -> Result<()> {
    let daemon = client(config, None);
    match command {
        AuthCommand::Generate => {
            let generated = daemon.auth_generate().await?;
            // The token alone on stdout, so `memcastle auth generate | op item
            // create ...` captures exactly it. It is printed here and nowhere
            // else: not logged, not written to a file, not in any JSON.
            println!("{}", generated.token);
            // Guidance goes to stderr so it never ends up in the captured token.
            eprintln!(
                "Store this token now (in 1Password or another secret manager): it is shown once \
                 and MemCastle keeps only a digest.\n\
                 To require it, set `auth.enabled = true` (or MEMCASTLE_AUTH_ENABLED=true), \
                 provide the token to clients as MEMCASTLE_AUTH_TOKEN, and restart the daemon \
                 (`memcastle restart`)."
            );
        }
        AuthCommand::Revoke => {
            let result = daemon.auth_revoke().await?;
            print_json(&result)?;
        }
    }
    Ok(())
}

/// `memcastle db`: ask the daemon to open, close or report on its database
/// admin endpoint. A plain `DaemonClient` call like every other command: the
/// endpoint lives in the daemon, the one process allowed to open the database.
async fn cmd_db(config: &Config, command: DbCommand) -> Result<()> {
    let daemon = client(config, None);
    match command {
        DbCommand::Serve(args) => {
            let status = daemon
                .db_start(&DbEndpointRequest {
                    bind: args.bind,
                    port: args.port,
                    // `None`, not `Some(false)`: leaving the flag off means
                    // "use the configured value", which may allow it.
                    allow_remote: args.allow_remote.then_some(true),
                    allowed_origins: args.allow_origin,
                })
                .await?;
            print_db_status(&status, args.json)
        }
        DbCommand::Stop => print_db_status(&daemon.db_stop().await?, false),
        DbCommand::Status(args) => print_db_status(&daemon.db_status().await?, args.json),
    }
}

/// Print the admin endpoint's state, as JSON for scripts or as the few lines a
/// person needs to point SurrealDB Studio at it.
fn print_db_status(status: &DbEndpointStatus, json: bool) -> Result<()> {
    if json {
        return print_json(status);
    }
    let Some(url) = status.url.as_deref().filter(|_| status.running) else {
        println!("database admin endpoint: not running (start it with `memcastle db serve`)");
        return Ok(());
    };
    println!("database admin endpoint: listening on {url}");
    println!("  namespace: {}", status.namespace);
    println!("  database:  {}", status.database);
    // Studio's login form wants a user and a password even when there is no
    // token, so say what to type in both cases.
    if status.auth_required {
        println!(
            "  sign in:   user `{}`, password: the MemCastle token",
            status.user
        );
    } else {
        println!(
            "  sign in:   user `{0}`, password `{0}` (loopback only, authentication is disabled)",
            status.user
        );
    }
    if status.remote {
        println!(
            "  warning:   listening beyond loopback; the token crosses the network in cleartext"
        );
    }
    println!("Connect SurrealDB Studio to the URL above. Stop it with `memcastle db stop`.");
    Ok(())
}

/// Install miette's diagnostic handler.
///
/// `--verbose` (or `RUST_BACKTRACE`, so a bug report already carries it)
/// prints the full cause chain; otherwise only the primary diagnostic
/// shows, since a source error the user did not ask for is noise more
/// often than it is the answer. Colour is left to miette's own
/// auto-detection so `NO_COLOR`/`TERM=dumb` still produce plain output.
fn install_miette_hook(verbose: bool) {
    let _ = miette::set_hook(Box::new(move |_| {
        let opts = MietteHandlerOpts::new();
        let opts = if verbose {
            opts.with_cause_chain()
        } else {
            opts.without_cause_chain()
        };
        Box::new(opts.build())
    }));
}

/// Structured logging with `filter` as the `EnvFilter` directive — see
/// `Config::log_filter` for where it comes from and its precedence.
fn init_tracing(filter: String, format: memcastle::config::LogFormat) {
    use std::io::IsTerminal;
    let builder = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::new(filter))
        .with_writer(std::io::stderr)
        // Escape codes would pollute journald and log files.
        .with_ansi(std::io::stderr().is_terminal());
    let _ = match format {
        memcastle::config::LogFormat::Text => builder.try_init(),
        memcastle::config::LogFormat::Json => builder.json().try_init(),
    };
}

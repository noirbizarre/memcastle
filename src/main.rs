//! The memcastle binary.
//!
//! Every subcommand is either `serve` (which runs the actual engine, via
//! `memcastle::server::run`) or a thin `DaemonClient` call — see
//! `memcastle::app`'s doc comment for why that split is the whole point of
//! this architecture. The exceptions: `migrate` opens storage itself (it must
//! work before a daemon exists), and `daemon start`/`daemon restart`
//! additionally manage the daemon process (registry file plus spawning
//! `serve`) without touching `store` or `jobs`. `integration` installs agent
//! integrations from files and the agent's own commands, through
//! `memcastle::integration`, and needs no daemon either.

#![allow(clippy::result_large_err)]

use std::process::ExitCode;
use std::time::Duration;

use clap::{CommandFactory, Parser};
use miette::MietteHandlerOpts;

mod cli;

use cli::{
    AuditArgs, AuthCommand, CheckpointArgs, Cli, Command, CompletionsArgs, DaemonCommand,
    DbCommand, DiaryCommand, DrawerCommand, EmbedArgs, ExtractArgs, IntegrationCommand, JobCommand,
    MigrateArgs, MineArgs, NoteArgs, RecallArgs, RepairArgs, RoomCommand, SearchArgs, ServeArgs,
    SourceCommand, StatusArgs, WakeUpArgs, WingCommand,
};
use memcastle::app::{DbEndpointRequest, DbEndpointStatus, WakeUpBudget};
use memcastle::client::{DaemonClient, StatusView};
use memcastle::config::{Config, Overrides};
use memcastle::domain::{MemoryMode, MiningSource, NameKind, PalacePath, validate_name};
use memcastle::store::SurrealStore;
use memcastle::term::{self, Painter};
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
    // Before the configuration is loaded: a completion script depends on
    // nothing but the command definition, and a broken config file must not
    // stop someone from installing tab completion to help fix it.
    if let Command::Completions(completions) = &args.command {
        return cmd_completions(completions);
    }
    let verbose = args.verbose > 0 || std::env::var_os("RUST_BACKTRACE").is_some();
    install_miette_hook(verbose);
    // Also before the configuration: developing and publishing a source (`init`, `build`, `test`, `package`, `index`,
    // `keygen`) works on a project directory and its archives, needs no daemon and no palace, and a broken config must
    // not get in its way.
    if let Command::Source(
        command @ (SourceCommand::Init(_)
        | SourceCommand::Build(_)
        | SourceCommand::Test(_)
        | SourceCommand::Package(_)
        | SourceCommand::Index(_)
        | SourceCommand::Keygen(_)),
    ) = &args.command
    {
        return match cmd_source_local(command).await {
            Ok(code) => code,
            Err(error) => {
                eprintln!("{:?}", miette::Report::new(error));
                ExitCode::FAILURE
            }
        };
    }

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

    // Read before `run` consumes `args`. Only these two commands open the embedded
    // datastore; every other one is an HTTP client with nothing to wait for.
    let opens_storage = matches!(args.command, Command::Serve(_) | Command::Migrate(_));
    let outcome = match config {
        Ok(config) => run(args, config).await,
        Err(error) => Err(error),
    };
    if opens_storage {
        wait_for_datastore_shutdown().await;
    }
    match outcome {
        Ok(code) => code,
        Err(error) => {
            eprintln!("{:?}", miette::Report::new(error));
            ExitCode::FAILURE
        }
    }
}

/// Keep the runtime alive until the embedded datastore has finished stopping.
///
/// SurrealDB has no explicit close: dropping the last `Surreal` handle makes a
/// detached task cancel the datastore's maintenance tasks, wait for them and
/// flush the storage engine. If the runtime is dropped first it cancels that
/// task and the maintenance tasks mid-way, which logs one `Background task did
/// not shut down cleanly` error per task and can skip the flush.
///
/// By now every handle is gone, so the only tasks left are the datastore's own
/// and they all end once its shutdown completes: waiting for the runtime to
/// hold no task is waiting for exactly that. `block_on`'s own future is not a
/// spawned task and is not counted.
async fn wait_for_datastore_shutdown() {
    /// Upstream bounds its own wait at 30s for tasks plus 60s for the node
    /// archive; this is shorter on purpose, so a stuck pass delays a stop by
    /// seconds, not past a service manager's stop timeout.
    const TIMEOUT: Duration = Duration::from_secs(10);
    const POLL: Duration = Duration::from_millis(5);

    let metrics = tokio::runtime::Handle::current().metrics();
    let deadline = tokio::time::Instant::now() + TIMEOUT;
    while metrics.num_alive_tasks() > 0 {
        if tokio::time::Instant::now() >= deadline {
            tracing::warn!(
                remaining = metrics.num_alive_tasks(),
                "the database did not finish shutting down within {TIMEOUT:?}; exiting anyway"
            );
            return;
        }
        tokio::time::sleep(POLL).await;
    }
}

/// The command-line layer of configuration. Built once, before the config is
/// loaded, so that every command resolves the palace and address the same way
/// (`--bind` used to be applied to `serve` alone, after loading).
fn overrides_from(args: &Cli) -> Overrides {
    let (bind, port, assets_dir) = match &args.command {
        // `daemon start`/`restart` resolve the address like `serve` does: they
        // wait on, and report, the very address the new daemon will listen on.
        Command::Serve(serve)
        | Command::Daemon(DaemonCommand::Start(serve) | DaemonCommand::Restart(serve)) => {
            (serve.bind, serve.port, serve.assets_dir.clone())
        }
        // The integration commands read the assets root and nothing else of the daemon's settings.
        Command::Integration(
            IntegrationCommand::List(cli::IntegrationListArgs { common })
            | IntegrationCommand::Install(cli::IntegrationAgentArgs { common, .. })
            | IntegrationCommand::Update(cli::IntegrationAgentArgs { common, .. })
            | IntegrationCommand::Remove(cli::IntegrationAgentArgs { common, .. }),
        ) => (None, None, common.assets_dir.clone()),
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
        // Like `status`, an update has more to say than success or failure: some sources may be updated while another
        // still waits for consent, and a script needs to tell.
        Command::Source(SourceCommand::Update(update)) => cmd_source_update(&config, &update).await,
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
        Command::Daemon(DaemonCommand::Start(start_args)) => {
            cmd_daemon_start(&config, config_file, mode, start_args).await
        }
        Command::Daemon(DaemonCommand::Stop) => cmd_stop(&config, mode).await,
        Command::Daemon(DaemonCommand::Restart(restart_args)) => {
            cmd_daemon_restart(&config, config_file, mode, restart_args).await
        }
        Command::Search(args) => cmd_search(&config, mode, args).await,
        Command::Recall(args) => cmd_recall(&config, mode, args).await,
        Command::WakeUp(args) => cmd_wake_up(&config, mode, args).await,
        Command::Mine(args) => cmd_mine(&config, mode, args).await,
        Command::Sources => cmd_sources(&config, mode).await,
        Command::Source(command) => cmd_source(&config, mode, command).await,
        Command::Integration(command) => cmd_integration(&config, &command),
        Command::Note(args) => cmd_note(&config, mode, args).await,
        Command::Checkpoint(args) => cmd_checkpoint(&config, mode, args).await,
        Command::Audit(args) => cmd_audit(&config, mode, args).await,
        Command::Embed(args) => cmd_embed(&config, mode, args).await,
        Command::Extract(args) => cmd_extract(&config, mode, args).await,
        Command::Repair(args) => cmd_repair(&config, mode, args).await,
        Command::Diary(cmd) => cmd_diary(&config, mode, cmd).await,
        Command::Job(cmd) => cmd_job(&config, mode, cmd).await,
        Command::Wing(cmd) => cmd_wing(&config, mode, cmd).await,
        Command::Room(cmd) => cmd_room(&config, mode, cmd).await,
        Command::Drawer(cmd) => cmd_drawer(&config, mode, cmd).await,
        Command::Auth(auth) => cmd_auth(&config, auth).await,
        Command::Db(db) => cmd_db(&config, db).await,
        // Handled before the configuration is loaded (see `async_main`); the
        // arm exists only so this match stays exhaustive.
        Command::Completions(_) => Ok(()),
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

/// Print the shell completion script for `args.shell` on stdout.
///
/// Generated into memory and written once, ignoring a write error: with
/// `memcastle completions zsh | head` the reader goes away early, and
/// `clap_complete` writing straight to stdout would panic on the broken pipe
/// instead of just stopping.
fn cmd_completions(args: &CompletionsArgs) -> ExitCode {
    use std::io::Write;
    let mut script = Vec::new();
    clap_complete::generate(args.shell, &mut Cli::command(), "memcastle", &mut script);
    let _ = std::io::stdout().write_all(&script);
    ExitCode::SUCCESS
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
        // Coloured only on a terminal that wants it, so a pipe or a log gets
        // the exact plain text.
        println!("{}", view.render_styled(Painter::for_stdout()));
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

/// Start a background daemon with the flags this command was given, and wait
/// until it is actually serving.
///
/// Refused when a daemon already answers for this palace: a second one would
/// only die on the palace's file lock after the delay of a failed startup, and
/// `daemon restart` is the command that means "replace it".
async fn cmd_daemon_start(
    config: &Config,
    config_path: Option<&std::path::Path>,
    mode: Option<MemoryMode>,
    args: ServeArgs,
) -> Result<()> {
    if client(config, mode).health().await {
        // The registry holds the address the live daemon really bound (which
        // `--port 0` makes differ from the configured one); the configured
        // address is only the fallback for a daemon that left no registry.
        let addr = memcastle::server::lifecycle::read_if_live(&config.palace.path).map_or_else(
            || config.server.socket_addr().to_string(),
            |info| info.bind_addr,
        );
        return Err(Error::DaemonAlreadyRunning { addr });
    }
    spawn_and_wait(config, config_path, &args, "started:").await
}

/// Stop the running daemon (if any), start a fresh one with the flags this
/// command was given, and wait until it is actually serving.
///
/// With no daemon running it simply starts one, so it is also a start that
/// does not mind what was there before.
async fn cmd_daemon_restart(
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
    spawn_and_wait(config, config_path, &args, "restarted:").await
}

/// Make `command`'s process independent of this one and of its terminal, so
/// the daemon it becomes outlives the command that started it.
fn detach(command: &mut std::process::Command) {
    // Detached from this terminal: the new daemon outlives this command,
    // and an inherited stderr would interleave its log with the prompt.
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // Its own process group: a Ctrl-C in the launching terminal signals
        // the foreground group, and would otherwise kill the daemon this
        // command just started along with the shell job.
        command.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        // No console to share (closing the launching one would end the
        // daemon) and its own group, for the same Ctrl-C reason as on Unix.
        command.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
    }
}

/// Spawn a detached `memcastle serve` and wait until it is serving, printing
/// `verb` (`started:`/`restarted:`) and its address when it is.
///
/// "Actually serving" is the registry file appearing with a live daemon
/// behind it — the same signal every other command discovers the daemon by —
/// not the process merely having been spawned: reporting success before that
/// told users a daemon that had crashed on startup was up. The new daemon
/// gets the original `--config`, `--bind`, `--port` and `--assets-dir` and the resolved `--palace`,
/// because a bare `memcastle serve` would silently come back on the default
/// address with the default config.
async fn spawn_and_wait(
    config: &Config,
    config_path: Option<&std::path::Path>,
    args: &ServeArgs,
    verb: &str,
) -> Result<()> {
    let exe = std::env::current_exe().map_err(|source| Error::io("current executable", source))?;
    // A previous process removes its registry file just before it exits, so
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
        // Forwarded like `--bind`: without it the spawned daemon would
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
        // spawned daemon must serve exactly the palace this command
        // found and waits on, whichever layer (file, environment, flag)
        // named it.
        command.arg("--palace").arg(&config.palace.path);
        detach(&mut command);
        let mut child = command
            .spawn()
            .map_err(|source| Error::io("memcastle serve", source))?;

        // A daemon takes a couple of seconds to open SurrealDB and migrate; a
        // debug build on a slow disk takes far longer, hence the generous
        // bound.
        for _ in 0..600 {
            if let Some(info) = memcastle::server::lifecycle::read_if_live(&config.palace.path) {
                let paint = Painter::for_stdout();
                println!(
                    "{} memcastle is serving on {}",
                    paint.ok(verb),
                    paint.accent(&format!("http://{}", info.bind_addr))
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

/// The validated query for `search` or `recall`, built by the one conversion
/// every interface shares so a bad `--ranking` or `--as-of` reads the same everywhere.
fn retrieval_query(
    query: String,
    limit: u32,
    wing: Option<String>,
    room: Option<String>,
    retrieval: cli::RetrievalArgs,
) -> Result<memcastle::search::SearchQuery> {
    memcastle::search::SearchOptions {
        limit: Some(limit),
        wing,
        room,
        ranking: retrieval.ranking,
        tags: retrieval.tags,
        source_kind: retrieval.source_kind,
        as_of: retrieval.as_of,
        from: retrieval.from,
        until: retrieval.until,
        include_historical: retrieval.include_historical,
        expand: retrieval.expand,
    }
    .into_query(query)
}

async fn cmd_search(config: &Config, mode: Option<MemoryMode>, args: SearchArgs) -> Result<()> {
    let query = retrieval_query(args.query, args.limit, args.wing, args.room, args.retrieval)?;
    let hits = client(config, mode).search(&query).await?;
    print_json(&hits)?;
    Ok(())
}

async fn cmd_recall(config: &Config, mode: Option<MemoryMode>, args: RecallArgs) -> Result<()> {
    let query = retrieval_query(args.query, args.limit, args.wing, None, args.retrieval)?;
    let hits = client(config, mode).recall(&query).await?;
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
    let source = match (args.path, args.source) {
        // Made absolute here, against *this* shell's working directory: the
        // daemon would otherwise resolve `./project` against its own, which is
        // wherever it was started and usually not where the user is standing.
        (Some(path), None) => MiningSource::Directory {
            path: std::path::absolute(&path)
                .map_err(|source| Error::io(path.display().to_string(), source))?,
        },
        (_, Some(source)) => MiningSource::Named {
            source,
            locator: args.locator,
        },
        // clap requires one of the two, so this is unreachable from the command line.
        (None, None) => {
            return Err(Error::invalid_input(
                "path",
                "give a directory to mine, or `--source`",
            ));
        }
    };
    let job = client(config, mode)
        .submit_mine(source, args.wing, args.full)
        .await?;
    print_json(&job)?;
    Ok(())
}

async fn cmd_sources(config: &Config, mode: Option<MemoryMode>) -> Result<()> {
    let report = client(config, mode).list_sources().await?;
    print_for_terminal_or_json(
        |painter, width| memcastle::client::table::render_sources(&report, painter, width),
        &report,
    )
}

/// `memcastle source init|build|test|package`: working on a source project, entirely on this machine.
///
/// No daemon and no configuration: these operate on a project directory and touch neither the store nor the jobs,
/// which is what lets someone build a source before they have a palace (AGENTS.md, invariant 1).
async fn cmd_source_local(command: &SourceCommand) -> Result<ExitCode> {
    use memcastle::source::build::Project;
    match command {
        SourceCommand::Init(args) => {
            let parent = match &args.parent {
                Some(parent) => parent.clone(),
                None => std::env::current_dir().map_err(|source| Error::io(".", source))?,
            };
            let (dir, files) =
                memcastle::source::scaffold::init(&parent, &args.name, args.template)?;
            let paint = Painter::for_stdout();
            println!(
                "{} {} ({} template)",
                paint.ok("Created"),
                dir.display(),
                args.template.name()
            );
            for file in files {
                println!("  {file}");
            }
            println!(
                "\nNext: cd {} && memcastle source build && memcastle source test",
                dir.display()
            );
            Ok(ExitCode::SUCCESS)
        }
        SourceCommand::Build(args) => {
            let project = Project::open(&args.path)?;
            let component = project.build()?;
            println!(
                "{} {}",
                Painter::for_stdout().ok("Built"),
                component.display()
            );
            Ok(ExitCode::SUCCESS)
        }
        SourceCommand::Test(args) => {
            let project = Project::open(&args.path)?;
            if !args.no_build {
                project.build()?;
            }
            let report = project
                .test(&memcastle::config::MiningConfig::default())
                .await?;
            let paint = Painter::for_stdout();
            for case in &report.cases {
                if case.failures.is_empty() {
                    println!("{} {}", paint.ok("PASS"), case.name);
                } else {
                    println!("{} {}", paint.error("FAIL"), case.name);
                    for failure in &case.failures {
                        println!("     {failure}");
                    }
                }
            }
            Ok(if report.passed() {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            })
        }
        SourceCommand::Package(args) => {
            let project = Project::open(&args.path)?;
            if !args.no_build {
                project.build()?;
            }
            let (archive, package) = project.package(args.output.as_deref())?;
            println!(
                "{} {}",
                Painter::for_stdout().ok("Packaged"),
                archive.display()
            );
            // The digest a registry index pins, written beside the archive in the format `sha256sum -c` reads, so a
            // publisher has it without a second tool.
            let bytes =
                std::fs::read(&archive).map_err(|e| Error::io(archive.display().to_string(), e))?;
            let archive_digest = memcastle::domain::sha256_hex(&bytes);
            let checksum_file = {
                let mut name = archive.clone().into_os_string();
                name.push(".sha256");
                std::path::PathBuf::from(name)
            };
            let file_name = archive
                .file_name()
                .map_or_else(String::new, |name| name.to_string_lossy().into_owned());
            std::fs::write(&checksum_file, format!("{archive_digest}  {file_name}\n"))
                .map_err(|e| Error::io(checksum_file.display().to_string(), e))?;
            println!("  archive sha256   {archive_digest}");
            println!("  component sha256 {}", package.digest);
            println!(
                "  permissions      {}",
                package.manifest.permissions.describe()
            );
            Ok(ExitCode::SUCCESS)
        }
        SourceCommand::Keygen(args) => {
            let key = memcastle::source::signing::generate()?;
            memcastle::source::signing::write_signing_key(&args.file, &key)?;
            let public = key.verifying_key();
            let paint = Painter::for_stdout();
            println!("{} {}", paint.ok("Wrote"), args.file.display());
            println!(
                "  key id      {}",
                memcastle::source::signing::key_id(&public)
            );
            println!(
                "  public key  {}",
                memcastle::source::signing::public_key_text(&public)
            );
            println!(
                "\nKeep the key file private. Users who trust you add the public key to `mining.trusted_keys`;\n\
                 sign packages with `memcastle source index --sign {}`.",
                args.file.display()
            );
            Ok(ExitCode::SUCCESS)
        }
        SourceCommand::Index(args) => {
            use memcastle::source::publish;
            let key = args
                .sign
                .as_deref()
                .map(memcastle::source::signing::read_signing_key)
                .transpose()?;
            let mut index = publish::read_index(&args.output, args.name.as_deref())?;
            let paint = Painter::for_stdout();
            for archive in &args.archives {
                let bytes = std::fs::read(archive)
                    .map_err(|e| Error::io(archive.display().to_string(), e))?;
                let file_name = archive
                    .file_name()
                    .map_or_else(String::new, |name| name.to_string_lossy().into_owned());
                let url = publish::archive_url(args.base_url.as_deref(), &file_name);
                let (name, version) = publish::add_archive(&mut index, &bytes, &url, key.as_ref())?;
                println!(
                    "{} {name} {version}{}",
                    paint.ok("Added"),
                    if key.is_some() { " (signed)" } else { "" }
                );
            }
            publish::write_index(&args.output, &mut index)?;
            println!("{} {}", paint.ok("Wrote"), args.output.display());
            if args.base_url.is_none() {
                println!("  package URLs are relative: publish the archives beside the index");
            }
            Ok(ExitCode::SUCCESS)
        }
        // Reached only through `async_main`'s check, which sends the daemon-side commands elsewhere.
        _ => Ok(ExitCode::SUCCESS),
    }
}

/// `memcastle source install|list|show|enable|disable|remove`: the daemon's installed sources, over HTTP like every
/// other command.
///
/// No `--mode` for the changes: a memory mode is a session's privilege over memory, and installing a source is an
/// administrative operation on what the daemon may run, like the token.
async fn cmd_source(
    config: &Config,
    mode: Option<MemoryMode>,
    command: SourceCommand,
) -> Result<()> {
    match command {
        SourceCommand::List => cmd_sources(config, mode).await,
        SourceCommand::Show(args) => {
            let source = client(config, mode).show_source(&args.name).await?;
            print_for_terminal_or_json(
                |painter, _| memcastle::client::table::render_source(&source, painter),
                &source,
            )
        }
        SourceCommand::Search(args) => {
            let registry = args.registry.as_deref().map(absolute_location);
            let found = client(config, None)
                .search_registry(args.query.as_deref(), registry.as_deref())
                .await?;
            print_for_terminal_or_json(
                |painter, width| {
                    memcastle::client::table::render_registry_search(&found, painter, width)
                },
                &found,
            )
        }
        SourceCommand::Install(args) => cmd_source_install(config, args).await,
        SourceCommand::Enable(args) => {
            let source = client(config, None)
                .set_source_enabled(&args.name, true)
                .await?;
            print_for_terminal_or_json(
                |painter, _| memcastle::client::table::render_source(&source, painter),
                &source,
            )
        }
        SourceCommand::Disable(args) => {
            let source = client(config, None)
                .set_source_enabled(&args.name, false)
                .await?;
            print_for_terminal_or_json(
                |painter, _| memcastle::client::table::render_source(&source, painter),
                &source,
            )
        }
        SourceCommand::Remove(args) => {
            term::confirm(
                &format!("Remove the source `{}` and its files?", args.name),
                "removing the source",
                args.confirm.yes,
            )?;
            client(config, None).remove_source(&args.name).await?;
            print_json(&serde_json::json!({ "removed": args.name }))
        }
        // Handled before the configuration is loaded (see `async_main`), or by `run` for its exit code.
        SourceCommand::Init(_)
        | SourceCommand::Build(_)
        | SourceCommand::Test(_)
        | SourceCommand::Package(_)
        | SourceCommand::Index(_)
        | SourceCommand::Keygen(_)
        | SourceCommand::Update(_) => Ok(()),
    }
}

/// What `memcastle source install` was asked to install.
enum InstallTarget {
    /// A package archive.
    File(std::path::PathBuf),
    /// A source project, to be built and packaged first.
    Directory(std::path::PathBuf),
    /// A name from the bundle or a registry, with the version if one was pinned.
    Named {
        name: String,
        version: Option<String>,
    },
}

/// Decide what `argument` names. Something that exists on disk is a path; anything else that is shaped like a path is
/// treated as one (so a typo says "no such file"); the rest is a name, optionally `name@version`.
fn classify_install(argument: &str) -> InstallTarget {
    let path = std::path::Path::new(argument);
    let has_separator = argument.contains(['/', '\\']);
    if path.is_file() {
        return InstallTarget::File(path.to_path_buf());
    }
    if path.is_dir() && (has_separator || path.join(memcastle::source::MANIFEST_FILE).is_file()) {
        return InstallTarget::Directory(path.to_path_buf());
    }
    if has_separator || argument.starts_with(['.', '~']) || argument.ends_with(".tar.gz") {
        return InstallTarget::File(path.to_path_buf());
    }
    match argument.split_once('@') {
        Some((name, version)) => InstallTarget::Named {
            name: name.to_string(),
            version: Some(version.to_string()),
        },
        None => InstallTarget::Named {
            name: argument.to_string(),
            version: None,
        },
    }
}

/// A registry written as a path is resolved against this shell's working directory: the daemon has its own, so a
/// relative path would name a different place there.
fn absolute_location(location: &str) -> String {
    if location.contains("://") {
        return location.to_string();
    }
    std::path::absolute(location)
        .map_or_else(|_| location.to_string(), |path| path.display().to_string())
}

/// The consent to send for a source that asks for `permissions`: none when it asks for nothing, the one given on the
/// command line, the user's answer in a terminal, and otherwise none, which makes the daemon refuse. A script never
/// consents on its own behalf.
fn consent_for(
    name: &str,
    version: &str,
    permissions: &memcastle::domain::Permissions,
    digest: String,
    given: Option<String>,
    yes: bool,
) -> Result<Option<String>> {
    if permissions.is_empty() {
        return Ok(None);
    }
    if let Some(given) = given {
        return Ok(Some(given));
    }
    if yes {
        return Ok(Some(digest));
    }
    if term::is_interactive() {
        eprintln!(
            "{} {name} {version} asks to: {}",
            Painter::for_stderr().warn("Source"),
            permissions.describe()
        );
        term::confirm(
            "Install it with these permissions?",
            "installing the source",
            false,
        )?;
        return Ok(Some(digest));
    }
    Ok(None)
}

/// `memcastle source install <file|directory|name[@version]>`.
async fn cmd_source_install(config: &Config, args: cli::SourceInstallArgs) -> Result<()> {
    let render = |installed: &memcastle::app::InstalledSource| {
        print_for_terminal_or_json(
            |painter, _| memcastle::client::table::render_source(&installed.source, painter),
            installed,
        )
    };
    match classify_install(&args.source) {
        InstallTarget::Named { name, version } => {
            let registry = args.registry.as_deref().map(absolute_location);
            let daemon = client(config, None);
            // The daemon downloads and verifies the package first, so what is agreed to is what was actually
            // fetched, not what an index claimed.
            let preview = daemon
                .preview_registry_source(&name, version.as_deref(), registry.as_deref())
                .await?;
            eprintln!(
                "{} {} {} from {} ({}){}",
                Painter::for_stderr().ok("Found"),
                preview.name,
                preview.version,
                preview.registry,
                preview.origin,
                preview
                    .signed_by
                    .as_ref()
                    .map_or_else(String::new, |key| format!(", signed by key {key}"))
            );
            let consent = consent_for(
                &preview.name,
                &preview.version,
                &preview.permissions,
                preview.consent_digest.clone(),
                args.consent,
                args.yes,
            )?;
            let installed = daemon
                .install_registry_source(&memcastle::app::RegistryInstall {
                    name,
                    version: Some(preview.version),
                    registry,
                    consent,
                    enable: args.enable,
                })
                .await?;
            render(&installed)
        }
        target => {
            let archive_path = match target {
                InstallTarget::Directory(dir) => {
                    use memcastle::source::build::Project;
                    let project = Project::open(&dir)?;
                    project.build()?;
                    let (archive, _) = project.package(None)?;
                    eprintln!(
                        "{} {}",
                        Painter::for_stderr().ok("Packaged"),
                        archive.display()
                    );
                    archive
                }
                InstallTarget::File(path) => path,
                InstallTarget::Named { .. } => unreachable!("handled above"),
            };
            let archive = std::fs::read(&archive_path)
                .map_err(|source| Error::io(archive_path.display().to_string(), source))?;
            // Read here only to show what is being agreed to; the daemon reads it again and trusts none of this.
            let package = memcastle::source::package::inspect(&archive)?;
            let name = &package.manifest.source.name;
            let permissions = package.manifest.permissions.normalized();
            let consent = consent_for(
                name,
                &package.manifest.source.version,
                &permissions,
                permissions.consent_digest(name),
                args.consent,
                args.yes,
            )?;
            let installed = client(config, None)
                .install_source(archive, consent.as_deref(), args.enable)
                .await?;
            render(&installed)
        }
    }
}

/// `memcastle source update [name] [--check]`.
///
/// Exits non-zero when something could not be updated, or is waiting for consent to permissions it newly asks for, so
/// a script that updates unattended notices.
async fn cmd_source_update(config: &Config, args: &cli::SourceUpdateArgs) -> Result<ExitCode> {
    use memcastle::app::UpdateStatus;
    let daemon = client(config, None);
    if args.check {
        let check = daemon.check_source_updates().await?;
        print_for_terminal_or_json(
            |painter, width| memcastle::client::table::render_update_check(&check, painter, width),
            &check,
        )?;
        return Ok(ExitCode::SUCCESS);
    }

    let mut outcomes = daemon.update_sources(args.name.as_deref(), None).await?;
    for outcome in &mut outcomes {
        let UpdateStatus::NeedsConsent {
            permissions,
            digest,
        } = &outcome.status
        else {
            continue;
        };
        let (permissions, digest) = (permissions.clone(), digest.clone());
        let agreed = if args.yes {
            true
        } else if term::is_interactive() {
            eprintln!(
                "{} {} {} now asks to: {permissions}",
                Painter::for_stderr().warn("Source"),
                outcome.name,
                outcome.to.as_deref().unwrap_or("?")
            );
            term::confirm(
                "Update it with these permissions?",
                "updating the source",
                false,
            )
            .is_ok()
        } else {
            false
        };
        if agreed
            && let Some(again) = daemon
                .update_sources(Some(&outcome.name), Some(&digest))
                .await?
                .pop()
        {
            *outcome = again;
        }
    }
    let healthy = outcomes.iter().all(|outcome| {
        matches!(
            outcome.status,
            UpdateStatus::Updated | UpdateStatus::Current
        )
    });
    print_for_terminal_or_json(
        |painter, _| memcastle::client::table::render_updates(&outcomes, painter),
        &outcomes,
    )?;
    Ok(if healthy {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

/// The room a note goes to when neither the command line nor the project names one.
const DEFAULT_NOTE_ROOM: &str = "notes";

/// Capture a note under the current project.
///
/// The text and the project scope are resolved locally, before the daemon is contacted, so a missing editor or a
/// broken project file fails as itself and not after a round trip. The daemon is only told the wing and room, as it is
/// by every client (docs/adr/029): it never learns what a project is.
async fn cmd_note(config: &Config, mode: Option<MemoryMode>, args: NoteArgs) -> Result<()> {
    let text = read_note_text(&args)?;
    let directory = std::env::current_dir()
        .map_err(|source| Error::io("<current directory>", source))
        .map(|dir| dir.canonicalize().unwrap_or(dir))?;
    let (wing, room) = note_scope(&directory, args.wing, args.room)?;
    let created = client(config, mode)
        .write_note(&wing, &room, text, Some(&directory.display().to_string()))
        .await?;
    use memcastle::client::palace_view as view;
    print_for_terminal_or_json(
        |painter, _| view::render_note(&created.item, created.created, &wing, &room, painter),
        &created,
    )
}

/// The wing and room a note captured in `directory` is filed under.
///
/// Per field: the flag, else the project context (`MEMCASTLE_WING`/`MEMCASTLE_ROOM`, then `.config/memcastle.toml`),
/// else the directory's name for the wing and `notes` for the room.
/// With both flags given nothing is read from the project, so a broken project file cannot block a note whose
/// destination was stated outright.
fn note_scope(
    directory: &std::path::Path,
    wing: Option<String>,
    room: Option<String>,
) -> Result<(String, String)> {
    let project = if wing.is_some() && room.is_some() {
        None
    } else {
        memcastle::project::resolve(directory)?
    };
    let wing = wing
        .or_else(|| project.as_ref().and_then(|p| p.wing.clone()))
        .or_else(|| memcastle::project::wing_from_directory(directory))
        .ok_or_else(|| {
            Error::invalid_input(
                "wing",
                "cannot name a wing after the current directory, pass `--wing <name>`",
            )
        })?;
    let room = room
        .or_else(|| project.and_then(|p| p.room))
        .unwrap_or_else(|| DEFAULT_NOTE_ROOM.to_string());
    Ok((wing, room))
}

/// The text of a note: the words given, else `--file` (`-` for standard input), else `--edit`, else piped standard
/// input, else the editor when a person is at the terminal.
///
/// Inline words are kept exactly. Text that came from a file, standard input or an editor loses its trailing
/// whitespace, because the final newline is an artefact of how it was produced and not part of the thought.
/// A note with nothing in it is refused rather than stored: a blank drawer is never recallable.
fn read_note_text(args: &NoteArgs) -> Result<String> {
    use std::io::{IsTerminal, Read};
    let from_stdin = || {
        let mut buf = String::new();
        std::io::stdin()
            .read_to_string(&mut buf)
            .map(|_| buf)
            .map_err(|source| Error::io("<stdin>", source))
    };
    let inline = args.text.join(" ");
    let text = match &args.file {
        Some(path) if path.as_os_str() == "-" => from_stdin()?,
        Some(path) => std::fs::read_to_string(path)
            .map_err(|source| Error::io(path.display().to_string(), source))?,
        None if args.edit => term::edit(&inline)?,
        None if !args.text.is_empty() => inline,
        // Piped input is read as is; a terminal means someone is there to type, and a shell prompt that hangs on a
        // forgotten argument is worse than an editor opening.
        None if std::io::stdin().is_terminal() => term::edit("")?,
        None => from_stdin()?,
    };
    let kept = if args.file.is_some() || args.edit || args.text.is_empty() {
        text.trim_end().to_string()
    } else {
        text
    };
    if kept.trim().is_empty() {
        return Err(Error::invalid_input(
            "note",
            "nothing to save, the note is empty: give the text as an argument, pipe it on standard input or use `--edit`",
        ));
    }
    Ok(kept)
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
    let job = client(config, mode).submit_audit(args.wing).await?;
    print_json(&job)?;
    Ok(())
}

async fn cmd_embed(config: &Config, mode: Option<MemoryMode>, args: EmbedArgs) -> Result<()> {
    let job = client(config, mode).submit_embed(args.wing).await?;
    print_json(&job)?;
    Ok(())
}

async fn cmd_extract(config: &Config, mode: Option<MemoryMode>, args: ExtractArgs) -> Result<()> {
    let job = client(config, mode).submit_extract(args.wing).await?;
    print_json(&job)?;
    Ok(())
}

async fn cmd_repair(config: &Config, mode: Option<MemoryMode>, args: RepairArgs) -> Result<()> {
    // Parsed client-side, before ever contacting the daemon — same
    // reasoning as `parse_job_id`'s other call sites in `cmd_job`.
    let based_on_job = args
        .based_on_job
        .as_deref()
        .map(Error::parse_job_id)
        .transpose()?;
    // Only the destructive run asks: a dry run changes nothing. After the
    // argument checks, so a typo in the id fails before any question is asked.
    if args.apply {
        term::confirm(
            "Apply repairs? This permanently changes palace data (run without --apply for a dry run).",
            "applying repairs",
            args.yes,
        )?;
    }
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

async fn cmd_job(config: &Config, mode: Option<MemoryMode>, command: JobCommand) -> Result<()> {
    let daemon = client(config, mode);
    match command {
        JobCommand::List { status } => {
            let status = status.map(|s| Error::parse_job_status(&s)).transpose()?;
            let jobs = daemon.list_jobs(status).await?;
            // A table for a person, JSON for anything else: whatever reads a
            // pipe (`jq`, a script, a test) gets data it can parse without
            // asking for it, and a terminal gets something it can read.
            if term::stdout_is_terminal() {
                println!(
                    "{}",
                    memcastle::client::table::render_jobs(
                        &jobs,
                        Painter::for_stdout(),
                        term::terminal_width()
                    )
                );
            } else {
                print_json(&jobs)?;
            }
        }
        JobCommand::Show { id } => print_json(&daemon.get_job(Error::parse_job_id(&id)?).await?)?,
        JobCommand::Pause { id } => {
            print_json(&daemon.pause_job(Error::parse_job_id(&id)?).await?)?;
        }
        JobCommand::Resume { id } => {
            print_json(&daemon.resume_job(Error::parse_job_id(&id)?).await?)?;
        }
        JobCommand::Cancel { id, yes } => {
            let job_id = Error::parse_job_id(&id)?;
            term::confirm(
                &format!("Cancel job {job_id}?"),
                &format!("cancelling job {job_id}"),
                yes,
            )?;
            print_json(&daemon.cancel_job(job_id).await?)?;
        }
        JobCommand::Retry { id } => {
            print_json(&daemon.retry_job(Error::parse_job_id(&id)?).await?)?;
        }
        JobCommand::Demo { steps } => {
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

/// `memcastle integration`: local, because installing an integration touches the machine's files and the agent, never
/// the daemon, and must work before a daemon has ever run.
///
/// `--assets-dir` has already been folded into `config.assets.dir` (see [`overrides_from`]), so a flag, the environment
/// and the config file all choose the same root.
fn cmd_integration(config: &Config, command: &IntegrationCommand) -> Result<()> {
    use memcastle::integration::{self, Catalog, Context, Locations, SystemRunner, render};

    let locations = Locations::from_process();
    let runner = SystemRunner;
    let ctx = Context::for_process(&locations, &runner);
    let painter = Painter::for_stdout();
    let (json, outcome) = match command {
        IntegrationCommand::List(args) => {
            let catalog = Catalog::open(config.assets.dir.as_deref())?;
            let report = render::list(&catalog, &ctx)?;
            if args.common.json {
                print_json(&report)?;
            } else {
                println!("{}", render::render_list(&report, painter));
            }
            return Ok(());
        }
        IntegrationCommand::Install(args) => {
            let catalog = Catalog::open(config.assets.dir.as_deref())?;
            (
                args.common.json,
                integration::install(catalog.get(&args.agent)?, &catalog, &ctx)?,
            )
        }
        IntegrationCommand::Update(args) => {
            let catalog = Catalog::open(config.assets.dir.as_deref())?;
            (
                args.common.json,
                integration::update(catalog.get(&args.agent)?, &catalog, &ctx)?,
            )
        }
        // Needs no assets: an integration must stay removable after the package that shipped it is gone.
        IntegrationCommand::Remove(args) => {
            (args.common.json, integration::remove(&args.agent, &ctx)?)
        }
    };
    if json {
        print_json(&outcome)
    } else {
        println!("{}", render::render_outcome(&outcome, painter));
        Ok(())
    }
}

/// Print `human` when stdout is a terminal, `json` otherwise: the same rule
/// `job list` follows, so a pipe always gets data it can parse.
fn print_for_terminal_or_json(
    human: impl FnOnce(Painter, Option<u16>) -> String,
    json: &impl serde::Serialize,
) -> Result<()> {
    if term::stdout_is_terminal() {
        println!("{}", human(Painter::for_stdout(), term::terminal_width()));
        Ok(())
    } else {
        print_json(json)
    }
}

/// Ask before a delete, showing what it would remove.
///
/// `stakes` fetches and formats that summary, and only runs when someone can
/// be asked: in a script the summary would go nowhere, and the read would
/// only be wasted work. It goes to stderr with the prompt, so stdout stays
/// what a pipe expects.
async fn confirm_delete<F>(question: &str, action: &str, yes: bool, stakes: F) -> Result<()>
where
    F: std::future::Future<Output = Result<String>>,
{
    if !yes && term::is_interactive() {
        eprintln!("{}\n", stakes.await?);
    }
    term::confirm(question, action, yes)
}

async fn cmd_wing(config: &Config, mode: Option<MemoryMode>, command: WingCommand) -> Result<()> {
    use memcastle::client::palace_view as view;
    let daemon = client(config, mode);
    match command {
        WingCommand::List => {
            let wings = daemon.list_wings().await?;
            print_for_terminal_or_json(
                |painter, width| memcastle::client::table::render_wings(&wings, painter, width),
                &wings,
            )?;
        }
        WingCommand::Show { wing } => {
            let wing = PalacePath::parse_wing(&wing)?;
            let detail = daemon.show_wing(&wing).await?;
            print_for_terminal_or_json(
                |painter, width| view::render_wing(&detail, painter, width),
                &detail,
            )?;
        }
        WingCommand::Create { wing, description } => {
            let name = PalacePath::parse_wing(&wing)?;
            validate_name(NameKind::Wing, &name)?;
            let created = daemon.create_wing(&name, description.as_deref()).await?;
            print_for_terminal_or_json(
                |painter, _| view::render_created("wing", &name, created.created, painter),
                &created,
            )?;
        }
        WingCommand::Delete { wing, yes } => {
            let wing = PalacePath::parse_wing(&wing)?;
            confirm_delete(
                "Delete this wing and all contained data?",
                &format!("deleting wing {wing}"),
                yes,
                async {
                    let detail = daemon.show_wing(&wing).await?;
                    Ok(view::wing_stakes(&detail.wing, Painter::for_stderr()))
                },
            )
            .await?;
            let deleted = daemon.delete_wing(&wing).await?;
            print_for_terminal_or_json(
                |painter, _| view::render_deleted(&format!("wing {wing}"), &deleted, painter),
                &deleted,
            )?;
        }
    }
    Ok(())
}

async fn cmd_room(config: &Config, mode: Option<MemoryMode>, command: RoomCommand) -> Result<()> {
    use memcastle::client::palace_view as view;
    let daemon = client(config, mode);
    match command {
        RoomCommand::List { wing } => {
            let rooms = match wing {
                Some(wing) => daemon.list_rooms(&wing).await?,
                None => {
                    // The daemon lists rooms per wing; across wings is the
                    // union, which keeps the API one resource per route.
                    let mut all = Vec::new();
                    for wing in daemon.list_wings().await? {
                        all.extend(daemon.list_rooms(&wing.wing.name).await?);
                    }
                    all
                }
            };
            print_for_terminal_or_json(
                |painter, width| memcastle::client::table::render_rooms(&rooms, painter, width),
                &rooms,
            )?;
        }
        RoomCommand::Show { room } => {
            let (wing, room) = PalacePath::parse_room(&room)?;
            let summary = daemon.show_room(&wing, &room).await?;
            print_for_terminal_or_json(
                |painter, _| view::render_room(&summary, painter),
                &summary,
            )?;
        }
        RoomCommand::Create { room, description } => {
            let (wing, room) = PalacePath::parse_room(&room)?;
            validate_name(NameKind::Room, &room)?;
            let created = daemon
                .create_room(&wing, &room, description.as_deref())
                .await?;
            print_for_terminal_or_json(
                |painter, _| {
                    view::render_created(
                        "room",
                        &format!("{wing}/{room}"),
                        created.created,
                        painter,
                    )
                },
                &created,
            )?;
        }
        RoomCommand::Delete { room, yes } => {
            let (wing, room) = PalacePath::parse_room(&room)?;
            confirm_delete(
                "Delete this room and all contained data?",
                &format!("deleting room {wing}/{room}"),
                yes,
                async {
                    let summary = daemon.show_room(&wing, &room).await?;
                    Ok(view::room_stakes(&summary, Painter::for_stderr()))
                },
            )
            .await?;
            let deleted = daemon.delete_room(&wing, &room).await?;
            print_for_terminal_or_json(
                |painter, _| {
                    view::render_deleted(&format!("room {wing}/{room}"), &deleted, painter)
                },
                &deleted,
            )?;
        }
    }
    Ok(())
}

async fn cmd_drawer(
    config: &Config,
    mode: Option<MemoryMode>,
    command: DrawerCommand,
) -> Result<()> {
    use memcastle::client::palace_view as view;
    let daemon = client(config, mode);
    match command {
        DrawerCommand::List { room, limit } => {
            let (wing, room) = PalacePath::parse_room(&room)?;
            let drawers = daemon.list_drawers(&wing, &room, limit).await?;
            print_for_terminal_or_json(
                |painter, width| memcastle::client::table::render_drawers(&drawers, painter, width),
                &drawers,
            )?;
        }
        DrawerCommand::Show { drawer } => {
            let (wing, room, drawer) = PalacePath::parse_drawer(&drawer)?;
            let found = daemon.show_drawer(&wing, &room, &drawer).await?;
            print_for_terminal_or_json(|painter, _| view::render_drawer(&found, painter), &found)?;
        }
        DrawerCommand::Create {
            drawer,
            content,
            file,
        } => {
            let (wing, room, name) = PalacePath::parse_drawer(&drawer)?;
            validate_name(NameKind::Drawer, &name)?;
            // Read before contacting the daemon: an unreadable file should
            // fail as a local I/O error, not after a round trip.
            let content = read_drawer_content(content, file.as_deref())?;
            let created = daemon
                .create_drawer(&wing, &room, Some(&name), content)
                .await?;
            print_for_terminal_or_json(
                |painter, _| view::render_created("drawer", &drawer, created.created, painter),
                &created,
            )?;
        }
        DrawerCommand::Supersede {
            drawer,
            content,
            file,
            invalidate,
        } => {
            let (wing, room, name) = PalacePath::parse_drawer(&drawer)?;
            if !invalidate && content.is_none() && file.is_none() {
                return Err(Error::invalid_input(
                    "content",
                    "give `--content <text>` or `--file <path>` for the replacement, \
                     or `--invalidate` to end the drawer without one",
                ));
            }
            // Read before contacting the daemon, like `drawer create`.
            let replacement = if invalidate {
                None
            } else {
                Some(read_drawer_content(content, file.as_deref())?)
            };
            // The REST route takes an id (a search hit already carries one), so
            // the path is resolved to it first.
            let found = daemon.show_drawer(&wing, &room, &name).await?;
            let outcome = daemon
                .supersede_drawer(&found.id.to_string(), replacement)
                .await?;
            print_json(&outcome)?;
        }
        DrawerCommand::History { drawer } => {
            let (wing, room, name) = PalacePath::parse_drawer(&drawer)?;
            // The REST route takes an id, so the path is resolved to it first
            // (a superseded version has no name left: its UUID resolves too).
            let found = daemon.show_drawer(&wing, &room, &name).await?;
            let history = daemon.drawer_history(&found.id.to_string()).await?;
            print_for_terminal_or_json(
                |painter, _| view::render_history(&history, painter),
                &history,
            )?;
        }
        DrawerCommand::Mention { drawer, name, kind } => {
            let (wing, room, drawer_name) = PalacePath::parse_drawer(&drawer)?;
            let found = daemon.show_drawer(&wing, &room, &drawer_name).await?;
            let link = daemon
                .link_drawer_entity(&found.id.to_string(), &name, &kind)
                .await?;
            print_json(&link)?;
        }
        DrawerCommand::Delete { drawer, yes } => {
            let (wing, room, name) = PalacePath::parse_drawer(&drawer)?;
            confirm_delete(
                "Delete this drawer?",
                &format!("deleting drawer {drawer}"),
                yes,
                async {
                    let found = daemon.show_drawer(&wing, &room, &name).await?;
                    Ok(view::drawer_stakes(&drawer, &found, Painter::for_stderr()))
                },
            )
            .await?;
            let deleted = daemon.delete_drawer(&wing, &room, &name).await?;
            print_for_terminal_or_json(
                |painter, _| view::render_deleted(&format!("drawer {drawer}"), &deleted, painter),
                &deleted,
            )?;
        }
    }
    Ok(())
}

/// The content for `drawer create`: `--content`, else `--file` (`-` for
/// standard input), else standard input.
///
/// A terminal on standard input is refused rather than waited on: a user who
/// forgot the flag would otherwise see the command hang with no prompt.
fn read_drawer_content(content: Option<String>, file: Option<&std::path::Path>) -> Result<String> {
    use std::io::{IsTerminal, Read};
    let from_stdin = || {
        if std::io::stdin().is_terminal() {
            return Err(Error::invalid_input(
                "content",
                "give `--content <text>` or `--file <path>`, or pipe the content on standard input",
            ));
        }
        let mut buf = String::new();
        std::io::stdin()
            .read_to_string(&mut buf)
            .map_err(|source| Error::io("<stdin>", source))?;
        Ok(buf)
    };
    match (content, file) {
        (Some(content), _) => Ok(content),
        (None, Some(path)) if path.as_os_str() == "-" => from_stdin(),
        (None, Some(path)) => std::fs::read_to_string(path)
            .map_err(|source| Error::io(path.display().to_string(), source)),
        (None, None) => from_stdin(),
    }
}

/// Manage the daemon's bearer token. No `--mode`: a memory mode is a session's
/// privilege over memory, and this is an administrative operation on
/// credentials.
async fn cmd_auth(config: &Config, command: AuthCommand) -> Result<()> {
    let daemon = client(config, None);
    match command {
        AuthCommand::Generate(confirm) => {
            // Asked on stderr, so the token on stdout is still all a pipe sees.
            term::confirm(
                "Generate a new token? Any token generated before stops working.",
                "generating a new token",
                confirm.yes,
            )?;
            let generated = daemon.auth_generate().await?;
            // The token alone on stdout, so `memcastle auth generate | op item
            // create ...` captures exactly it. It is printed here and nowhere
            // else: not logged, not written to a file, not in any JSON.
            println!("{}", generated.token);
            // Guidance goes to stderr so it never ends up in the captured token.
            let paint = Painter::for_stderr();
            eprintln!(
                "{} (in 1Password or another secret manager): it is shown once \
                 and MemCastle keeps only a digest.\n\
                 To require it, set `auth.enabled = true` (or MEMCASTLE_AUTH_ENABLED=true), \
                 provide the token to clients as MEMCASTLE_AUTH_TOKEN, and restart the daemon \
                  (`memcastle daemon restart`).",
                paint.warn("Store this token now")
            );
        }
        AuthCommand::Revoke(confirm) => {
            term::confirm(
                "Revoke the generated token? Clients using it are refused immediately.",
                "revoking the token",
                confirm.yes,
            )?;
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
        DbCommand::Start(args) => {
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
    // Coloured only on a terminal that wants it; the words are the same plain.
    let paint = Painter::for_stdout();
    let Some(url) = status.url.as_deref().filter(|_| status.running) else {
        println!(
            "database admin endpoint: {} (start it with {})",
            paint.warn("not running"),
            paint.accent("`memcastle db start`")
        );
        return Ok(());
    };
    // A repeated `db start` is not an error, but say so: otherwise the output
    // reads as if this command had just opened it.
    if status.already_running {
        println!(
            "database admin endpoint: {} on {}",
            paint.warn("already running"),
            paint.accent(url)
        );
    } else {
        println!(
            "database admin endpoint: {} on {}",
            paint.ok("listening"),
            paint.accent(url)
        );
    }
    println!("  {} {}", paint.dim("namespace:"), status.namespace);
    println!("  {}  {}", paint.dim("database:"), status.database);
    // Studio's login form wants a user and a password even when there is no
    // token, so say what to type in both cases.
    if status.auth_required {
        println!(
            "  {}   user `{}`, password: the MemCastle token",
            paint.dim("sign in:"),
            status.user
        );
    } else {
        println!(
            "  {}   user `{1}`, password `{1}` (loopback only, authentication is disabled)",
            paint.dim("sign in:"),
            status.user
        );
    }
    if status.remote {
        println!(
            "  {}   {}",
            paint.error("warning:"),
            paint.warn("listening beyond loopback; the token crosses the network in cleartext")
        );
    }
    println!(
        "Connect SurrealDB Studio to the URL above. Stop it with {}.",
        paint.accent("`memcastle db stop`")
    );
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

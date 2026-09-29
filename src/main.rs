//! The memcastle binary.
//!
//! Every subcommand is either `serve`/`daemon` (which runs the actual
//! engine, via `memcastle::server::run`) or a thin `DaemonClient` call — see
//! `memcastle::app`'s doc comment for why that split is the whole point of
//! this architecture.

#![allow(clippy::result_large_err)]

use std::process::ExitCode;
use std::str::FromStr;

use clap::Parser;
use miette::MietteHandlerOpts;

mod cli;

use cli::{
    AuditArgs, CheckpointArgs, Cli, Command, DiaryCommand, JobsCommand, MigrateArgs, MineArgs,
    RecallArgs, RepairArgs, SearchArgs, WakeUpArgs,
};
use memcastle::app::WakeUpBudget;
use memcastle::client::DaemonClient;
use memcastle::config::Config;
use memcastle::domain::JobId;
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
    let config = Config::load(args.config.as_deref());
    init_tracing(
        config
            .as_ref()
            .map_or_else(|_| "info".to_string(), Config::log_filter),
    );

    let outcome = match config {
        Ok(config) => run(args, config).await,
        Err(error) => Err(error),
    };
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{:?}", miette::Report::new(error));
            ExitCode::FAILURE
        }
    }
}

async fn run(args: Cli, mut config: Config) -> Result<()> {
    match args.command {
        Command::Serve(serve_args) => {
            if let Some(bind) = serve_args.bind {
                config.server.bind = bind;
            }
            memcastle::server::run(config).await
        }
        Command::Migrate(args) => cmd_migrate(&config, args).await,
        Command::Status => cmd_status(&config).await,
        Command::Stop => cmd_stop(&config).await,
        Command::Restart => cmd_restart(&config).await,
        Command::Search(args) => cmd_search(&config, args).await,
        Command::Recall(args) => cmd_recall(&config, args).await,
        Command::WakeUp(args) => cmd_wake_up(&config, args).await,
        Command::Mine(args) => cmd_mine(&config, args).await,
        Command::Checkpoint(args) => cmd_checkpoint(&config, args).await,
        Command::Audit(args) => cmd_audit(&config, args).await,
        Command::Repair(args) => cmd_repair(&config, args).await,
        Command::Diary(cmd) => cmd_diary(&config, cmd).await,
        Command::Jobs(jobs) => cmd_jobs(&config, jobs).await,
        Command::Wings => Err(Error::not_implemented("memcastle wings")),
        Command::Rooms => Err(Error::not_implemented("memcastle rooms")),
        Command::Drawers => Err(Error::not_implemented("memcastle drawers")),
        Command::Maintenance => Err(Error::not_implemented("memcastle maintenance")),
    }
}

fn client(config: &Config) -> DaemonClient {
    DaemonClient::discover(&config.palace.path, config.server.bind)
}

/// Print `value` as pretty JSON — or fail loudly. A serialization error used
/// to print an empty line and exit 0, which a script reads as "no results".
fn print_json(value: &impl serde::Serialize) -> Result<()> {
    let text = serde_json::to_string_pretty(value)
        .map_err(|source| Error::serialization("the command's output", source))?;
    println!("{text}");
    Ok(())
}

async fn cmd_status(config: &Config) -> Result<()> {
    let status = client(config).status().await?;
    print_json(&status)?;
    Ok(())
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
                versions: format!("{:?}", status.pending),
            });
        }
        return Ok(());
    }

    let report = memcastle::migrate::run(&store).await?;
    print_json(&report)?;
    Ok(())
}

async fn cmd_stop(config: &Config) -> Result<()> {
    print_json(&client(config).shutdown().await?)?;
    Ok(())
}

async fn cmd_restart(config: &Config) -> Result<()> {
    let daemon = client(config);
    if daemon.health().await {
        daemon.shutdown().await?;
        // Best-effort: poll until the port frees up, or give up after a few
        // seconds rather than hanging indefinitely on a stuck shutdown.
        for _ in 0..50 {
            if !daemon.health().await {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    }

    let exe = std::env::current_exe().map_err(|source| Error::io("current executable", source))?;
    std::process::Command::new(exe)
        .arg("serve")
        .spawn()
        .map_err(|source| Error::io("memcastle serve", source))?;
    println!("restarted");
    Ok(())
}

async fn cmd_search(config: &Config, args: SearchArgs) -> Result<()> {
    let hits = client(config)
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

async fn cmd_recall(config: &Config, args: RecallArgs) -> Result<()> {
    let hits = client(config)
        .recall(&args.query, args.wing.as_deref(), args.limit)
        .await?;
    print_json(&hits)?;
    Ok(())
}

async fn cmd_wake_up(config: &Config, args: WakeUpArgs) -> Result<()> {
    let budget = WakeUpBudget::from_options(args.max_items, args.max_bytes);
    let context = client(config)
        .wake_up(&args.agent_identity, args.wing.as_deref(), budget)
        .await?;
    print_json(&context)?;
    Ok(())
}

async fn cmd_mine(config: &Config, args: MineArgs) -> Result<()> {
    let job = client(config).submit_mine(args.path, args.wing).await?;
    print_json(&job)?;
    Ok(())
}

async fn cmd_checkpoint(config: &Config, args: CheckpointArgs) -> Result<()> {
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
        .map_err(|source| Error::invalid_input("checkpoint payload", source.to_string()))?;
    let job = client(config).checkpoint(payload, args.emergency).await?;
    print_json(&job)?;
    Ok(())
}

async fn cmd_audit(config: &Config, args: AuditArgs) -> Result<()> {
    let job = client(config).submit_audit(args.scope).await?;
    print_json(&job)?;
    Ok(())
}

async fn cmd_repair(config: &Config, args: RepairArgs) -> Result<()> {
    // Parsed client-side, before ever contacting the daemon — same
    // reasoning as `parse_job_id`'s other call sites in `cmd_jobs`.
    let based_on_job = args.based_on_job.as_deref().map(parse_job_id).transpose()?;
    let job = client(config)
        .submit_repair(!args.apply, based_on_job)
        .await?;
    print_json(&job)?;
    Ok(())
}

async fn cmd_diary(config: &Config, command: DiaryCommand) -> Result<()> {
    let daemon = client(config);
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

async fn cmd_jobs(config: &Config, command: JobsCommand) -> Result<()> {
    let daemon = client(config);
    match command {
        JobsCommand::List { status } => {
            let status = status.map(|s| parse_status(&s)).transpose()?;
            print_json(&daemon.list_jobs(status).await?)?;
        }
        JobsCommand::Show { id } => print_json(&daemon.get_job(parse_job_id(&id)?).await?)?,
        JobsCommand::Pause { id } => {
            print_json(&daemon.pause_job(parse_job_id(&id)?).await?)?;
        }
        JobsCommand::Resume { id } => {
            print_json(&daemon.resume_job(parse_job_id(&id)?).await?)?;
        }
        JobsCommand::Cancel { id } => {
            print_json(&daemon.cancel_job(parse_job_id(&id)?).await?)?;
        }
        JobsCommand::Retry { id } => {
            print_json(&daemon.retry_job(parse_job_id(&id)?).await?)?;
        }
        JobsCommand::Demo { steps } => {
            // The daemon has no "submit a demo job" REST endpoint of its
            // own reachable from here without going through `/api/jobs`
            // with a `demo` kind — reuse the same generic endpoint the
            // `mine` command uses, just with a different JSON body.
            let job: memcastle::domain::Job = daemon.demo(steps).await?;
            print_json(&job)?;
        }
    }
    Ok(())
}

fn parse_job_id(raw: &str) -> Result<JobId> {
    JobId::from_str(raw).map_err(|_| Error::invalid_job_id(raw))
}

fn parse_status(raw: &str) -> Result<memcastle::domain::JobStatus> {
    serde_json::from_value(serde_json::Value::String(raw.to_string()))
        .map_err(|_| Error::invalid_input("status", format!("unknown job status `{raw}`")))
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
fn init_tracing(filter: String) {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::new(filter))
        .with_writer(std::io::stderr)
        .try_init();
}

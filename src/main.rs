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

use cli::{Cli, Command, JobsCommand, MineArgs, SearchArgs};
use memcastle::client::DaemonClient;
use memcastle::config::Config;
use memcastle::domain::JobId;
use memcastle::{Error, Result};

#[tokio::main]
async fn main() -> ExitCode {
    let args = Cli::parse();
    let verbose = args.verbose > 0 || std::env::var_os("RUST_BACKTRACE").is_some();
    install_miette_hook(verbose);
    init_tracing();

    match run(args).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{:?}", miette::Report::new(error));
            ExitCode::FAILURE
        }
    }
}

async fn run(args: Cli) -> Result<()> {
    let mut config = Config::load(args.config.as_deref())?;

    match args.command {
        Command::Serve(serve_args) => {
            if let Some(bind) = serve_args.bind {
                config.server.bind = bind;
            }
            memcastle::server::run(config).await
        }
        Command::Status => cmd_status(&config).await,
        Command::Stop => cmd_stop(&config).await,
        Command::Restart => cmd_restart(&config).await,
        Command::Search(args) => cmd_search(&config, args).await,
        Command::Mine(args) => cmd_mine(&config, args).await,
        Command::Jobs(jobs) => cmd_jobs(&config, jobs).await,
        Command::Wings | Command::Rooms | Command::Drawers | Command::Maintenance => {
            Err(Error::config(
                "not implemented in this bootstrap — the architecture reserves this command, see docs/architecture.md",
            ))
        }
    }
}

fn client(config: &Config) -> DaemonClient {
    DaemonClient::discover(&config.palace.path, config.server.bind)
}

fn print_json(value: &impl serde::Serialize) {
    println!(
        "{}",
        serde_json::to_string_pretty(value).unwrap_or_default()
    );
}

async fn cmd_status(config: &Config) -> Result<()> {
    let status = client(config).status().await?;
    print_json(&status);
    Ok(())
}

async fn cmd_stop(config: &Config) -> Result<()> {
    client(config).shutdown().await?;
    println!("shutdown requested");
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
    let hits = client(config).search(&args.query, args.limit).await?;
    print_json(&hits);
    Ok(())
}

async fn cmd_mine(config: &Config, args: MineArgs) -> Result<()> {
    let job = client(config).submit_mine(args.path, args.wing).await?;
    print_json(&job);
    Ok(())
}

async fn cmd_jobs(config: &Config, command: JobsCommand) -> Result<()> {
    let daemon = client(config);
    match command {
        JobsCommand::List { status } => {
            let status = status.map(|s| parse_status(&s)).transpose()?;
            print_json(&daemon.list_jobs(status).await?);
        }
        JobsCommand::Show { id } => print_json(&daemon.get_job(parse_job_id(&id)?).await?),
        JobsCommand::Pause { id } => {
            daemon.pause_job(parse_job_id(&id)?).await?;
            println!("pause requested");
        }
        JobsCommand::Resume { id } => {
            daemon.resume_job(parse_job_id(&id)?).await?;
            println!("resumed");
        }
        JobsCommand::Cancel { id } => {
            daemon.cancel_job(parse_job_id(&id)?).await?;
            println!("cancel requested");
        }
        JobsCommand::Retry { id } => {
            daemon.retry_job(parse_job_id(&id)?).await?;
            println!("retried");
        }
        JobsCommand::Demo { steps } => {
            // The daemon has no "submit a demo job" REST endpoint of its
            // own reachable from here without going through `/api/jobs`
            // with a `demo` kind — reuse the same generic endpoint the
            // `mine` command uses, just with a different JSON body.
            let job: memcastle::domain::Job = daemon.demo(steps).await?;
            print_json(&job);
        }
    }
    Ok(())
}

fn parse_job_id(raw: &str) -> Result<JobId> {
    JobId::from_str(raw).map_err(|_| Error::JobNotFound {
        id: raw.to_string(),
    })
}

fn parse_status(raw: &str) -> Result<memcastle::domain::JobStatus> {
    serde_json::from_value(serde_json::Value::String(raw.to_string()))
        .map_err(|_| Error::config(format!("unknown job status `{raw}`")))
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

/// Structured logging, level from `MEMCASTLE_LOG`/`RUST_LOG`, defaulting to
/// `info`. Set up before config loading so a config-load failure is itself
/// logged consistently with everything after it.
fn init_tracing() {
    let filter = std::env::var("MEMCASTLE_LOG")
        .or_else(|_| std::env::var("RUST_LOG"))
        .unwrap_or_else(|_| "info".to_string());
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::new(filter))
        .with_writer(std::io::stderr)
        .try_init();
}

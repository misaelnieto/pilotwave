//! pilotwave-worker — drains the `email` queue of `django-tasks-db`.
//!
//! Thin bootstrap: parse the CLI, load config, dispatch without a DB (`log`,
//! `stop`, the `--bg` parent) and delegate to `worker::Worker` for oneshots
//! (`status`/`inspect`/`list`/single `run`) and loops (`--fg`/`--bg-child`).
//! `status` shows the daemon section even when the DB is down; `is-alive`
//! prints its one-line probe (`fresh|stale|down|error`, exits 0/1/2) even
//! with bad config or a dead DB, so a cron keep-alive stays machine-readable.

mod attachments;
mod cli;
mod config;
mod contract;
mod daemon;
mod db;
mod heartbeat;
mod log;
mod logging;
mod mailer;
mod proc;
mod retry;
mod worker;

use std::process::ExitCode;

use clap::Parser;

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    match run().await {
        Ok(code) => code,
        Err(err) => {
            logging::error(&format!("fatal: {err}"));
            ExitCode::from(1)
        }
    }
}

async fn run() -> worker::Result<ExitCode> {
    let cli = cli::Cli::parse();
    logging::set_verbose(cli.verbose);

    // Parse `.env` without exporting it (see `config::Env`), then build the
    // typed config. Real env vars win over the file. `is-alive` is a cron
    // probe: even bad config reports as its one-line `error` (exit 2)
    // instead of the generic `fatal` (exit 1), so the line stays
    // machine-readable; the detail goes to stderr.
    let is_alive = matches!(cli.command, cli::Command::IsAlive(_));
    let bad_config = |msg: String| -> worker::Result<ExitCode> {
        if is_alive {
            eprintln!("error: {msg}");
            println!("{}", cli::render_is_alive_error("bad_config"));
            return Ok(ExitCode::from(2));
        }
        Err(msg.into())
    };
    let env = match config::Env::load() {
        Ok(env) => env,
        Err(msg) => return bad_config(msg),
    };
    let config = match config::Config::from_env(&env) {
        Ok(config) => config,
        Err(msg) => return bad_config(msg),
    };
    // `log` never touches the DB and its stdout carries ONLY log content: it
    // is served before any banner (`starting ...` goes to stdout).
    if let cli::Command::Log(args) = &cli.command {
        log::run_log(&config, args).await?;
        return Ok(ExitCode::SUCCESS);
    }
    // `Debug` redacts `database_url` and `smtp_password`: safe for logs.
    // (`is-alive` stays silent on stdout: its only stdout line is the probe
    // result, so even `-v` diagnostics must not pollute command substitution.
    // The suppression lives here, not in `logging`, to keep every other
    // subcommand byte-identical.)
    if !is_alive {
        logging::debug(&format!("config: {config:?}"));

        // Never log `database_url`: it carries the DB password.
        logging::info(&format!(
            "starting worker_id={} queue={} backend={} tasks={}",
            config.worker_id,
            config.queue_name,
            config.backend_name,
            config.task_paths.join(",")
        ));
    }

    // `--bg` parent: idempotent probe + spawn, without touching the DB.
    // Presentation (stdout/stderr/logging) lives here; `daemon` only
    // returns the outcome.
    if matches!(&cli.command, cli::Command::Run(args) if args.bg && !args.bg_child) {
        return match daemon::run_bg_parent(&config, cli.verbose) {
            Ok(daemon::BgOutcome::AlreadyRunning(pid)) => {
                logging::info(&format!("already running pid={pid}"));
                Ok(ExitCode::SUCCESS)
            }
            Ok(daemon::BgOutcome::Started(pid)) => {
                logging::info(&format!(
                    "daemon pid_file={} log_file={}",
                    config
                        .pid_file
                        .as_ref()
                        .map(|p| p.display().to_string())
                        .unwrap_or_default(),
                    config
                        .log_file
                        .as_ref()
                        .map(|p| p.display().to_string())
                        .unwrap_or_default()
                ));
                println!("started pid={pid}");
                logging::info(&format!("daemon started pid={pid}"));
                Ok(ExitCode::SUCCESS)
            }
            Err(err) => {
                eprintln!("error: {err}");
                Err(err)
            }
        };
    }

    // `stop` never touches the DB: served before connecting.
    if matches!(cli.command, cli::Command::Stop) {
        return match daemon::run_stop(&config) {
            Ok(daemon::StopOutcome::AlreadyStopped) => {
                println!("{}", cli::render_stop_not_running());
                logging::info("stop: daemon no corriendo (idempotente)");
                Ok(ExitCode::SUCCESS)
            }
            Ok(daemon::StopOutcome::Stopped { pid, forced }) => {
                println!("{}", cli::render_stop_done(pid, forced));
                logging::info(&format!("stop: daemon detenido pid={pid} forced={forced}"));
                Ok(ExitCode::SUCCESS)
            }
            Err(err) => {
                eprintln!("error: {err}");
                Err(err)
            }
        };
    }

    // Persistent loop: `--fg` foreground or `--bg-child` daemon (same loop,
    // the child additionally holds the PID-file lock until death).
    let is_loop = matches!(&cli.command, cli::Command::Run(args) if args.fg || args.bg_child);
    if is_loop {
        let is_bg_child = matches!(&cli.command, cli::Command::Run(args) if args.bg_child);
        // Recycling exits 75 WITHOUT `fatal:` (not an error); clean
        // shutdown exits 0. Both go through the pool drop.
        return match worker::Worker::run_loop(config, is_bg_child).await? {
            worker::LoopEnd::Shutdown => Ok(ExitCode::SUCCESS),
            worker::LoopEnd::Recycle => Ok(ExitCode::from(75)),
        };
    }

    // One-shot: single attempt, fail loud (`?` -> exit 1).
    // `status` resolves the daemon BEFORE touching the DB: the daemon section
    // prints even when Postgres is down (DB down => daemon on stdout +
    // detail on stderr + exit 1). `is-alive` resolves it the same way so
    // `down`/`error` still print when Postgres is down (read-only probe:
    // never writes, never signals). Without `PILOTWAVE_PID_FILE` there is no
    // crash: the section reports "not configured".
    let daemon_info = if matches!(cli.command, cli::Command::Status | cli::Command::IsAlive(_)) {
        Some(daemon::describe(&config))
    } else {
        None
    };
    // Read-only subcommands use the reader DSN when configured (roadmap R-7).
    let read_only = matches!(
        &cli.command,
        cli::Command::Status
            | cli::Command::IsAlive(_)
            | cli::Command::Inspect(_)
            | cli::Command::List(_)
    );
    let dsn = if read_only {
        config.read_database_url()
    } else {
        &config.database_url
    };
    let pool = match db::connect(dsn).await {
        Ok(pool) => pool,
        Err(err) => {
            if matches!(cli.command, cli::Command::IsAlive(_)) {
                // Cron probe: DB unreachable is `error` (exit 2: alert,
                // never blind-restart against a dead Postgres). The daemon
                // was already resolved above without touching the DB.
                eprintln!("error: base de datos no disponible: {err}");
                println!("{}", cli::render_is_alive_error("db_unavailable"));
                return Ok(ExitCode::from(2));
            }
            if let Some(info) = &daemon_info {
                // Only `status` reaches here with the daemon resolved.
                println!("{}", cli::render_daemon_status(info));
                eprintln!("error: base de datos no disponible: {err}");
                return Err(worker::invalid_input(
                    "base de datos no disponible".to_owned(),
                ));
            }
            return Err(Box::new(err) as Box<dyn std::error::Error>);
        }
    };
    // `is-alive` stays silent on stdout (single probe line only).
    if !is_alive {
        logging::info("postgres connected (max_connections=1)");
    }
    // Least-privilege check (roadmap R-6): fail loud when the role lacks the
    // minimum `.env.example` GRANTs. Write paths only (`run`): `status` /
    // `inspect` / `list` use the reader DSN and never write.
    if matches!(&cli.command, cli::Command::Run(_))
        && let Err(err) = db::verify_least_privilege(&pool, config.skip_grant_check).await
    {
        return Err(worker::invalid_input(err));
    }
    let worker_id = config.worker_id.clone();
    let scope = db::QueueScope::from(&config);
    let worker = worker::Worker::new(config, db::PostgresTaskStore::new(pool, scope, worker_id));

    match cli.command {
        cli::Command::Status => {
            worker.run_status(daemon_info.as_ref()).await?;
        }
        cli::Command::IsAlive(args) => {
            // Explicit codes, never `?` (`main` maps `Err` to 1):
            // fresh => 0, stale/down => 1, error => 2.
            return worker.run_is_alive(args.max_age_secs).await;
        }
        cli::Command::Inspect(args) => {
            worker.run_inspect(&args.id, args.show_pii).await?;
        }
        cli::Command::List(args) => {
            let query = args
                .command
                .to_list_query()
                .map_err(worker::invalid_input)?;
            worker.run_list(&query).await?;
        }
        // `stop` returns before connecting (never touches the DB): unreachable.
        cli::Command::Stop => {
            unreachable!("stop se atiende antes de conectar a la BD")
        }
        // `log` likewise (file tail, no DB): unreachable.
        cli::Command::Log(_) => {
            unreachable!("log se atiende antes de conectar a la BD")
        }
        cli::Command::Run(args) => {
            worker.run_once(args.id.as_deref()).await?;
        }
    }
    // The pool drops here (no explicit close).
    Ok(ExitCode::SUCCESS)
}

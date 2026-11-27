//! pilotwave-worker — drains the `email` queue of `django-tasks-db`.
//!
//! Thin bootstrap: parse the CLI, load config, dispatch without a DB (`log`,
//! `stop`, the `--bg` parent) and delegate to `worker::Worker` for oneshots
//! (`status`/`is-alive`/`inspect`/`list`/single `run`) and loops (`--fg`/`--bg-child`).
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

    // Parse `.env` without exporting it (see `config::Env`). Real env vars
    // win over the file. `is-alive` is a cron probe on the reader DSN only
    // (see `run_is_alive_probe`): even bad config reports as its one-line
    // `error` (exit 2) instead of the generic `fatal` (exit 1), so the line
    // stays machine-readable; the detail goes to stderr.
    let is_alive = matches!(cli.command, cli::Command::IsAlive(_));
    // Threshold parsing runs before config/DB so a malformed
    // `--max-age-secs` always reports `invalid_max_age_secs` (exit 2)
    // instead of being masked by `bad_config` or `db_unavailable`.
    if let cli::Command::IsAlive(args) = &cli.command
        && let Err(reason) = cli::parse_max_age_secs(&args.max_age_secs)
    {
        eprintln!("error: invalid --max-age-secs: {:?}", args.max_age_secs);
        println!("{}", cli::render_is_alive_error(&reason));
        return Ok(ExitCode::from(2));
    }
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
    // `is-alive` is a read-only cron probe: it runs on the reader DSN only
    // (roadmap R-7) and must not require worker/SMTP settings
    // (`PILOTWAVE_DATABASE_URL`, `EMAIL_*`, `DEFAULT_FROM_EMAIL`, …).
    // Load just the reader DSN + pidfile, then probe without a full `Config`.
    if let cli::Command::IsAlive(args) = &cli.command {
        return run_is_alive_probe(&env, args).await;
    }
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
                logging::info("stop: daemon not running (idempotent)");
                Ok(ExitCode::SUCCESS)
            }
            Ok(daemon::StopOutcome::Stopped { pid, forced }) => {
                println!("{}", cli::render_stop_done(pid, forced));
                logging::info(&format!("stop: daemon stopped pid={pid} forced={forced}"));
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
    // detail on stderr + exit 1). Without `PILOTWAVE_PID_FILE` there is no
    // crash: the section reports "not configured".
    let daemon_info = if matches!(cli.command, cli::Command::Status) {
        Some(daemon::describe(&config))
    } else {
        None
    };
    // Read-only subcommands use the reader DSN when configured (roadmap R-7).
    // (`is-alive` never reaches here: it probes via `run_is_alive_probe`
    // on the reader DSN without a full `Config`.)
    let read_only = matches!(
        &cli.command,
        cli::Command::Status | cli::Command::Inspect(_) | cli::Command::List(_)
    );
    let dsn = if read_only {
        config.read_database_url()
    } else {
        &config.database_url
    };
    // Defense in depth: the selected DSN must require TLS outside
    // loopback before any credential or heartbeat traffic leaves the
    // process. `Config::from_env` already rejects a remote reader DSN
    // without `sslmode=require`; re-check the exact string `db::connect`
    // is about to open so a future caller cannot bypass it (A02).
    if !config::dsn_requires_ssl(dsn) && !config::dsn_host_is_loopback(dsn) {
        let var_name = if read_only && config.database_url_ro.is_some() {
            "PILOTWAVE_DATABASE_URL_RO"
        } else {
            "PILOTWAVE_DATABASE_URL"
        };
        let msg = format!(
            "{var_name} must include sslmode=require outside localhost \
             (A02: credentials would travel in cleartext)"
        );
        return Err(worker::invalid_input(msg));
    }
    let pool = match db::connect(dsn).await {
        Ok(pool) => pool,
        Err(err) => {
            if let Some(info) = &daemon_info {
                // Only `status` reaches here with the daemon resolved.
                println!("{}", cli::render_daemon_status(info));
                eprintln!("error: database unavailable: {err}");
                return Err(worker::invalid_input("database unavailable".to_owned()));
            }
            return Err(Box::new(err) as Box<dyn std::error::Error>);
        }
    };
    logging::info("postgres connected (max_connections=1)");
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
        cli::Command::IsAlive(_) => {
            unreachable!("is-alive is served by run_is_alive_probe before connecting")
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
            unreachable!("stop is handled before connecting to the DB")
        }
        // `log` likewise (file tail, no DB): unreachable.
        cli::Command::Log(_) => {
            unreachable!("log is handled before connecting to the DB")
        }
        cli::Command::Run(args) => {
            worker.run_once(args.id.as_deref()).await?;
        }
    }
    // The pool drops here (no explicit close).
    Ok(ExitCode::SUCCESS)
}

/// `is-alive` cron probe on the reader DSN only (no full `Config`).
///
/// Loads just `PILOTWAVE_DATABASE_URL_RO` (fallback: worker DSN) plus the
/// pidfile, resolves the local daemon without touching the DB, then runs
/// the shared heartbeat check. Never writes, never signals, never starts
/// anything. Explicit codes, never `?` (`main` maps `Err` to 1):
/// fresh => 0, stale/down => 1, error => 2. Stdout carries only the probe
/// line; diagnostics go to stderr.
async fn run_is_alive_probe(
    env: &config::Env,
    args: &cli::IsAliveArgs,
) -> worker::Result<ExitCode> {
    let probe = match config::IsAliveConfig::from_env(env) {
        Ok(probe) => probe,
        Err(msg) => {
            eprintln!("error: {msg}");
            println!("{}", cli::render_is_alive_error("bad_config"));
            return Ok(ExitCode::from(2));
        }
    };
    // The local daemon gates `fresh`: a fresh shared heartbeat from another
    // host (or left by a recently stopped local daemon) must not mask a
    // stopped local daemon, or cron would skip the restart.
    let daemon_info = daemon::describe_pid_file(probe.pid_file.as_ref());
    // Defense in depth: the DSN must require TLS outside loopback before
    // any credential or heartbeat traffic leaves the process.
    // `IsAliveConfig::from_env` already rejects it; re-check the exact
    // string `db::connect` is about to open (A02).
    let dsn = probe.database_url.as_str();
    if !config::dsn_requires_ssl(dsn) && !config::dsn_host_is_loopback(dsn) {
        eprintln!(
            "error: {} must include sslmode=require outside localhost \
             (A02: credentials would travel in cleartext)",
            probe.database_url_var
        );
        println!("{}", cli::render_is_alive_error("bad_config"));
        return Ok(ExitCode::from(2));
    }
    let pool = match db::connect(dsn).await {
        Ok(pool) => pool,
        Err(err) => {
            // DB unreachable is `error` (exit 2: alert, never blind-restart
            // against a dead Postgres). The daemon was already resolved
            // above without touching the DB.
            eprintln!("error: database unavailable: {err}");
            println!("{}", cli::render_is_alive_error("db_unavailable"));
            return Ok(ExitCode::from(2));
        }
    };
    // The heartbeat `SELECT` ignores queue scope: the probe only needs a
    // store to read it. Dummy scope/worker id, never used for writes.
    let scope = db::QueueScope::new(String::new(), String::new(), Vec::new());
    let store = db::PostgresTaskStore::new(pool, scope, "is-alive".to_owned());
    // Explicit codes, never `?` (`main` maps `Err` to 1).
    worker::Worker::probe_is_alive(&store, Some(&daemon_info), &args.max_age_secs).await
}

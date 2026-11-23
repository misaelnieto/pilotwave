//! pilotwave-worker — drains the `email` queue of `django-tasks-db`.
//!
//! Bootstrap fino: parsea CLI, carga config, despacha sin BD (`log`,
//! `stop`, padre `--bg`) y delega en `worker::Worker` para oneshot
//! (`status`/`inspect`/`list`/`run` único) y loop (`--fg`/`--bg-child`).
//! `status` muestra la sección daemon aun si la BD está caída.

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
    // typed config. Real env vars win over the file.
    let env = config::Env::load()?;
    let config = config::Config::from_env(&env)?;
    // `log` no toca la BD y su stdout lleva SOLO contenido del log: se
    // atiende antes de cualquier banner (`starting ...` va a stdout).
    if let cli::Command::Log(args) = &cli.command {
        log::run_log(&config, args).await?;
        return Ok(ExitCode::SUCCESS);
    }
    // `Debug` redacta `database_url` y `smtp_password`: seguro para logs.
    logging::debug(&format!("config: {config:?}"));

    // Never log `database_url`: it carries the DB password.
    logging::info(&format!(
        "starting worker_id={} queue={} backend={} tasks={}",
        config.worker_id,
        config.queue_name,
        config.backend_name,
        config.task_paths.join(",")
    ));

    // Padre `--bg`: sonda idempotente + spawn, sin tocar la BD.
    // La presentación (stdout/stderr/logging) vive aquí; `daemon` solo
    // devuelve el outcome.
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

    // `stop` no toca la BD: se atiende antes de conectar.
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

    // Loop persistente: `--fg` foreground o `--bg-child` daemon (mismo loop,
    // el hijo además sostiene el lock del PID file hasta morir).
    let is_loop = matches!(&cli.command, cli::Command::Run(args) if args.fg || args.bg_child);
    if is_loop {
        let is_bg_child = matches!(&cli.command, cli::Command::Run(args) if args.bg_child);
        // §3.9: el reciclaje es exit 75 SIN `fatal:` (no es un error); el
        // shutdown limpio es exit 0. Ambos pasan por el drop del pool.
        return match worker::Worker::run_loop(config, is_bg_child).await? {
            worker::LoopEnd::Shutdown => Ok(ExitCode::SUCCESS),
            worker::LoopEnd::Recycle => Ok(ExitCode::from(75)),
        };
    }

    // One-shot: una sola oportunidad y fallo alto (`?` → exit 1).
    // `status` resuelve el daemon ANTES de tocar la BD: la sección daemon
    // se imprime aun si Postgres está caído (BD KO => daemon en stdout +
    // detalle en stderr + exit 1). Sin `PILOTWAVE_PID_FILE` no hay crash:
    // la sección dice "no configurado".
    let daemon_info = if matches!(cli.command, cli::Command::Status) {
        Some(daemon::describe(&config))
    } else {
        None
    };
    // R-7: los subcomandos de solo lectura usan el DSN de lectura si existe.
    let read_only = matches!(
        &cli.command,
        cli::Command::Status | cli::Command::Inspect(_) | cli::Command::List(_)
    );
    let dsn = if read_only {
        config.read_database_url()
    } else {
        &config.database_url
    };
    let pool = match db::connect(dsn).await {
        Ok(pool) => pool,
        Err(err) => {
            if let Some(info) = &daemon_info {
                // Solo `status` llega aquí con daemon resuelto.
                println!("{}", cli::render_daemon_status(info));
                eprintln!("error: base de datos no disponible: {err}");
                return Err(worker::invalid_input(
                    "base de datos no disponible".to_owned(),
                ));
            }
            return Err(Box::new(err) as Box<dyn std::error::Error>);
        }
    };
    logging::info("postgres connected (max_connections=1)");
    // R-6 (audit 001 §A07): fail loud si el rol no tiene los GRANT minimos
    // de `.env.example`. Solo en caminos de escritura (`run`): `status` /
    // `inspect` / `list` usan el DSN de lectura y no escriben.
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
        // `stop` retorna antes de conectar (no toca la BD): inalcanzable.
        cli::Command::Stop => {
            unreachable!("stop se atiende antes de conectar a la BD")
        }
        // `log` idem (tail del archivo, sin BD): inalcanzable.
        cli::Command::Log(_) => {
            unreachable!("log se atiende antes de conectar a la BD")
        }
        cli::Command::Run(args) => {
            worker.run_once(args.id.as_deref()).await?;
        }
    }
    // El pool cae por drop aquí (sin close explícito).
    Ok(ExitCode::SUCCESS)
}

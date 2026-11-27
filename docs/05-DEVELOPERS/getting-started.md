# Getting Started

## Requirements

- Python 3.14 (managed by `uv`)
- Django 6.x (installed from `pyproject.toml`)
- Rust toolchain with `x86_64-unknown-linux-musl`
- Docker with the Compose plugin (development Postgres only)
- `just`, `uv`, `cargo`

## Setup

```bash
cp .env.example .env
just pre-install-check   # optional: verifies host tools
just sync                # uv sync + cargo fetch
```

## Everyday commands

```bash
just dev-up              # start local Postgres
just migrate             # apply Pilotwave migrations
just run-worker          # claim + send one READY task (oneshot)
just worker-status       # queue counts + daemon + heartbeat
just is-alive            # liveness probe: one line, exits 0/1/2 (cron keep-alive)
just test                # Python + Rust tests
just lint                # ruff + cargo fmt/clippy (readonly)
just format              # autofix/format both languages
just build               # wheel with the musl binary
just dev-down            # stop local Postgres
```

No command needs a direct `uv`, `cargo`, `maturin` or `docker compose`
invocation. `just sync`, `just test`, `just lint` and `just build` run
identically in CI (same recipes, same order).

## Local operation

```bash
just dev-up              # 1. Postgres up (dev only, never a deploy mechanism)
just migrate             # 2. apply Pilotwave migrations
just run-worker          # 3. drain one READY task through the real binary
just worker-status       # 4. check counts; also shows daemon + heartbeat
just log                 # 5. tail the daemon log (PILOTWAVE_LOG_FILE)
just stop                # 6. stop the --bg daemon (idempotent, no DB writes)
```

Variants (flags are the real `pilotwave-worker` flags, passed through):

```bash
just run-worker -- --id <UUID>   # drain one specific task
just run-worker -- --fg          # persistent foreground loop (Ctrl-C stops)
just run-worker -- --bg          # detached daemon (writes PILOTWAVE_PID_FILE)
just list -- ready               # ready | running | done | failed
just list -- failed --limit 10 --offset 0
just inspect <UUID>              # every column of one queue row
just log -- -n 50 -f             # last 50 lines, then follow appends
```

`status`, `run`, `list`, `inspect` and `is-alive` need
`PILOTWAVE_DATABASE_URL` plus a live Postgres; the read-only ones reuse
`PILOTWAVE_DATABASE_URL_RO` when it is set. `log` and `stop` never touch
the database (`stop` only signals the daemon via `PILOTWAVE_PID_FILE`).
`inspect --show-pii` prints full PII (recipients, bodies, traceback)
and is operators-only. Run any binary with `--help`
(`just` passes it through the same way, e.g. `just log -- --help`) for the
full flag reference.

## Operator topics (SSOT in the user guide)

`is-alive` + cron, attachment roots and the Mailcrab env block are
documented for operators in
[03-DEPLOYMENT.md](../07-USER-GUIDE/03-DEPLOYMENT.md)
([ADR-004](../02-DECISIONS/004-is-alive-subcommand.md)) and
[02-CONFIGURATION.md](../07-USER-GUIDE/02-CONFIGURATION.md)
([ADR-003](../02-DECISIONS/003-anonymous-smtp-opt-in.md)). Fixed cron
one-liner (only exit 1 restarts, never exit 2; lock path arbitrary):

```cron
*/5 * * * * . /etc/pilotwave/worker.env && flock -n /run/pilotwave/keepalive.lock sh -c 'pilotwave-worker is-alive --max-age-secs 360; [ $? -eq 1 ] && pilotwave-worker run --bg'
```

Dev-only notes: `just dev-up` provides that Mailcrab (`:1025` SMTP,
`:1080` UI); `just is-alive` and `just run-worker -- --bg` wrap the
same probe and daemon.

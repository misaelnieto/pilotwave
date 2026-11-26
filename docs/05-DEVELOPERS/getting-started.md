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

`status`, `run`, `list`, `inspect` and `is-alive` need `PILOTWAVE_DATABASE_URL` plus a
live Postgres; `log` and `stop` never touch the database (`stop` only
signals the daemon via `PILOTWAVE_PID_FILE`). Run any binary with `--help`
(`just` passes it through the same way, e.g. `just log -- --help`) for the
full flag reference.

## Cron keep-alive (`is-alive`, ADR-004)

`just worker-status` prints a human table; a cron probe needs a
machine-readable signal. `is-alive` prints **exactly one line** on stdout
(diagnostics stay on stderr, so command substitution never breaks):

```bash
just is-alive
# fresh age_secs=3 pid=1234     -> exit 0 (nothing to do)
# stale age_secs=400 pid=1234   -> exit 1 (restartable)
# down reason=no_heartbeat      -> exit 1 (restartable)
# error reason=db_unavailable   -> exit 2 (alert, never blind-restart)
```

Exit 1 means stale/down: the caller restarts (`is-alive ... || run --bg`;
`run --bg` is idempotent, already-running is a no-op exit 0). Exit 2 means
bad config or an unreachable DB: page, don't respawn against a dead
Postgres. The probe is strictly read-only (reader DSN when configured,
single heartbeat `SELECT`; never writes, never signals or starts anything).

The restart threshold lives with the caller via `--max-age-secs` (default
90, the `fresh`/`stale` meaning from `heartbeat.rs`). A 5-minute cron with
one full interval plus margin uses 360:

```bash
flock -n /run/pilotwave.keepalive pilotwave-worker is-alive --max-age-secs 360 || pilotwave-worker run --bg
```

Even with a dead DB or bad config the probe still prints its one line
(`error reason=..`, exit 2); only a missing subcommand/flag usage error
goes to stderr (clap, exit 2).

## Configuration

All connection settings come from environment variables; see the documented
[`.env.example`](../../.env.example). The real worker uses
`PILOTWAVE_DATABASE_URL` with a least-privilege role, never `DATABASE_URL`.

### Attachment roots (required, no fallback)

`PILOTWAVE_ATTACHMENT_ROOTS` has **no `MEDIA_ROOT` fallback, not even in
dev**. An empty/missing value means deny-all: every send with attachments
fails closed with `path_policy_violation`. Wiring explicit absolute roots
is the Developer/Integrator's responsibility:

```bash
# .env — comma-separated absolute prefixes owned by the producer project
PILOTWAVE_ATTACHMENT_ROOTS=/home/user/apps/myapp/media,/home/user/apps/myapp/uploads
```

Checklist: create the directories in the producer project, mount/copy the
same paths where the worker runs, keep them absolute, never point them at
`/`, and restart the worker after changing them. The rule is enforced by
`pilotwave-worker`; no Python fallback sender ships in v1 (P-5 decided).

### Dev SMTP without AUTH (Mailcrab)

Mailcrab (`just dev-up`, SMTP `:1025`) advertises no AUTH extension, so the
worker cannot log in to it. For local dev only, run without credentials:
leave `EMAIL_HOST_USER`/`EMAIL_HOST_PASSWORD` empty (whitespace = absent)
and set both opt-ins:

```dotenv
EMAIL_HOST=localhost
EMAIL_PORT=1025
EMAIL_USE_TLS=false
PILOTWAVE_ALLOW_PLAINTEXT_SMTP=true
PILOTWAVE_ALLOW_ANONYMOUS_SMTP=true
```

Rules (fail loud, see ADR `003-anonymous-smtp-opt-in`): user+pass go together
(only one set = startup error); no AUTH is accepted only when the host is
loopback (`localhost`/`127.*`/`::1`) **and** `EMAIL_USE_TLS=false` (with the
plaintext opt-in) **and** `PILOTWAVE_ALLOW_ANONYMOUS_SMTP=true`. A typo in the
host fails closed instead of sending unauthenticated mail elsewhere. The
worker logs a `WARNING ... sin AUTH` on stderr at startup. Never enable the
anonymous flag outside dev/loopback.

Booleans are `django-environ`-style (case-insensitive): `true`/`1`/`yes`/`y`/
`on` and `false`/`0`/`no`/`n`/`off`; any other value is a startup error.

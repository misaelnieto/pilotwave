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

`status`, `run`, `list` and `inspect` need `PILOTWAVE_DATABASE_URL` plus a
live Postgres; `log` and `stop` never touch the database (`stop` only
signals the daemon via `PILOTWAVE_PID_FILE`). Run any binary with `--help`
(`just` passes it through the same way, e.g. `just log -- --help`) for the
full flag reference.

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

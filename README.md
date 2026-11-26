# Pilotwave

> Enqueue email in Django, deliver it from a tiny Rust daemon. Postgres is the only queue. No Redis, no RabbitMQ, no second Django in RAM.

[![Python 3.14](https://img.shields.io/badge/python-3.14-blue)](pyproject.toml)
[![Django 6](https://img.shields.io/badge/django-6.x-green)](pyproject.toml)
[![License: MIT](https://img.shields.io/badge/license-MIT-yellow)](LICENSE)
[![Postgres 18](https://img.shields.io/badge/postgres-18-blue)](compose.yaml)

Pilotwave is a two-part email outbox for Django:

1. **Producer (Python):** your view calls `mail_notification.enqueue(...)` — a single Postgres `INSERT` via `django.tasks` + `django-tasks-db`. The request returns immediately, no SMTP in the request cycle.
2. **Worker (Rust):** the static `pilotwave-worker` binary polls Postgres with `FOR UPDATE SKIP LOCKED`, sends via SMTP, marks the row `SUCCESSFUL`/`FAILED`, and upserts a heartbeat row.

Django renders, Pilotwave delivers.

## Why this exists

Small VPS / shared hosting where you can afford **Django + Postgres + Nginx** and little else:

- A resident Python worker (`db_worker`, Celery worker) costs a full Django RSS just to poll — typically 80–180 MB.
- Redis also needs its own daemon and memory (we saw ~100 MB growth trigger the OOM-killer on a shared host), plus backup/monitoring.
- Pilotwave replaces both with one ~5 MB static binary that speaks Postgres and SMTP directly.

Measured on this repo (`just benchmark`, 100 identical mails, fake SMTP, `VmRSS` from `/proc`):

| worker | RSS max | RSS idle mean | PSS max |
| --- | --- | --- | --- |
| minimal Python poller (stdlib + psycopg, no Django) | 49,428 kB | 49,428 kB | 41,007 kB |
| `pilotwave-worker` release | 5,028 kB | 5,028 kB | 3,012 kB |
| **ratio** | **9.8x less** | **9.8x less** | **13.6x less** |

Details, scripts and caveats: [benchmark/](benchmark/). The Python baseline is deliberately best-case (no validation, no Django); a real `db_worker` uses *more*.

**Non-goals:** worker speed/throughput, sub-second latency, general task queue. Poll interval defaults to 1.5–5 s. If you need high-volume or precise scheduling, use Celery.

## Features

- **No broker to operate.** If you have Postgres, you already have the queue.
- **Small idle footprint.** Single-threaded Tokio, 1 Postgres connection, `opt-level=z + lto + strip`.
- **Crash-safe claim.** Atomic `UPDATE … FOR UPDATE SKIP LOCKED`, ordered by `priority DESC, run_after, enqueued_at`. Multiple workers can coexist.
- **Fail-closed safety.** Attachment roots required (`PILOTWAVE_ATTACHMENT_ROOTS`, no `MEDIA_ROOT` fallback), header/injection checks, secret redaction, least-privilege DB roles.
- **Observable.** `pilotwave_heartbeat` row (`last_seen`, counters, `last_error_code`), plus `status` / `list` / `inspect` CLI (read-only DSN supported).
- **Contract-tested.** Python and Rust share [contracts/CONTRACT.md](contracts/CONTRACT.md); drift fails the test suite.

## Limitations (read before adopting)

> **Status: v0.1.0 alpha.** Email-only, API unstable, production hardening in progress.

- **At-least-once delivery.** Crash after send + zombie-reaper can duplicate a mail. Producers must be idempotent by `reference` (same reference = same content). No dedupe table in v1.
- **Email-only, Postgres-only.** No Python fallback ships in v1 — only `pilotwave-worker` drains the `pilotwave.email` queue.
- **Polling, not push.** Expect seconds of delay, 2 MiB row limit, 10 MiB attachments total, 6 MiB per file.
- **One machine, one benchmark.** Numbers above are from a single host/run — re-run `just benchmark` before quoting.

## How it works

```mermaid
flowchart LR
    View[Django view] -->|mail_notification.enqueue: INSERT| PG[(Postgres: django_tasks_database_dbtaskresult)]
    PG -->|claim FOR UPDATE SKIP LOCKED| RW[pilotwave-worker Rust]
    RW -->|SMTP| SMTP[(mail server)]
    RW -->|SUCCESSFUL/FAILED| PG
    RW -->|upsert last_seen + counters| HB[(pilotwave_heartbeat)]
```

## Requirements

- Python 3.14, Django 6.x
- Postgres 15+ (dev compose uses `postgres:18`)
- SMTP server (any; dev uses Mailcrab on `:1025`, UI on `:1080`)

## Install

```bash
uv add pilotwave
# provides both the Django app and the pilotwave-worker binary in .venv/bin/
```

Or from source:

```bash
cp .env.example .env
just sync    # uv + cargo deps
just dev-up  # local Postgres + Mailcrab (dev only)
just migrate # Django migrations
```

## Quickstart (5 minutes)

**1. Settings** (`settings.py`):

```python
INSTALLED_APPS = [
    "django_tasks_db",
    "pilotwave",
    # …
]
TASKS = {
    "default": {"BACKEND": "django_tasks_db.DatabaseBackend", "QUEUES": ["default", "pilotwave.email"]}
}
```

**2. Enqueue from your view** (PRG so refresh never double-enqueues):

```python
from django.db import transaction

from pilotwave.tasks import mail_notification

reference = f"order-{order.id}"
# `enqueue` INSERTs immediately (no on_commit deferral): inside a
# transaction, defer it explicitly so a rollback never publishes the row.
transaction.on_commit(
    lambda: mail_notification.enqueue(
        to="user@example.com",
        subject="Your order shipped",
        reference=reference,
        text_body="Hi, your order is on its way.",
    )
)
```

The task is `pilotwave.tasks.mail_notification` (`@task(queue_name="pilotwave.email")`) — shipped by the package, no `tasks.py` to write in your app. Its body never runs in production (Rust reads the Postgres row directly); it only runs in tests / eager mode.

See `example/demo/` for a runnable app.

> What gets stored: `args_kwargs = {"args": [], "kwargs": {reference, to, subject, text_body/html_body, …}}`. Full rules: [contracts/CONTRACT.md](contracts/CONTRACT.md).

**3. Run the worker** (needs `PILOTWAVE_DATABASE_URL` + `EMAIL_*` from `.env`):

```bash
pilotwave-worker run --fg   # foreground loop (systemd/Docker)
pilotwave-worker run        # oneshot: claim one READY task, then exit
pilotwave-worker status     # queue counts + daemon + heartbeat
```

Minimal `.env`:

```dotenv
PILOTWAVE_DATABASE_URL=postgres://pilotwave_worker:CHANGEME@localhost:5432/pilotwave
PILOTWAVE_ATTACHMENT_ROOTS=/srv/app/media
EMAIL_HOST=smtp.example.com
EMAIL_PORT=587
EMAIL_USE_TLS=true
EMAIL_HOST_USER=you@example.com
EMAIL_HOST_PASSWORD=secret
DEFAULT_FROM_EMAIL="App <noreply@example.com>"
```

Full reference: [.env.example](.env.example).

### Dev without SMTP AUTH (Mailcrab)

Mailcrab (`just dev-up`, SMTP on `:1025`, UI on `:1080`) advertises no
AUTH extension. For local dev only, leave `EMAIL_HOST_USER`/`EMAIL_HOST_PASSWORD`
empty and set both opt-ins (loopback + `EMAIL_USE_TLS=false` required, anything
else fails loud — see ADR [003](docs/02-DECISIONS/003-anonymous-smtp-opt-in.md)):

```dotenv
EMAIL_HOST=localhost
EMAIL_PORT=1025
EMAIL_USE_TLS=false
PILOTWAVE_ALLOW_PLAINTEXT_SMTP=true
PILOTWAVE_ALLOW_ANONYMOUS_SMTP=true
```

Never set `PILOTWAVE_ALLOW_ANONYMOUS_SMTP=true` in production: without AUTH,
anyone who can reach the SMTP host can send mail as you (egress note in
[SECURITY.md](SECURITY.md)).

## Configuration

| Key | Purpose | Default |
| --- | --- | --- |
| `PILOTWAVE_DATABASE_URL` | Worker DSN (least-privilege role, never `DATABASE_URL`) | required |
| `PILOTWAVE_DATABASE_URL_RO` | Read-only DSN for `status/list/inspect` | falls back to worker DSN |
| `PILOTWAVE_TASK_PATHS` | Task allowlist (defaults to shipped task) | `pilotwave.tasks.mail_notification` |
| `PILOTWAVE_QUEUE_NAME` / `PILOTWAVE_BACKEND_NAME` | Claim scope | `pilotwave.email` / `default` |
| `PILOTWAVE_POLL_MIN_MS` / `MAX_MS` | Backoff when queue empty | `1500` / `5000` |
| `PILOTWAVE_MAX_ATTEMPTS` | Transient SMTP retries before `smtp_exhausted` | `5` |
| `PILOTWAVE_ATTACHMENT_ROOTS` | Absolute prefixes; empty = deny all | fail-closed, no fallback |
| `EMAIL_HOST/PORT/USE_TLS/HOST_USER/HOST_PASSWORD` | SMTP used by Rust worker (user+pass together; blank = absent) | required, unless dev no-AUTH below |
| `PILOTWAVE_ALLOW_PLAINTEXT_SMTP` | Allow `USE_TLS=false` (dev only, fail-loud in prod) | `false` |
| `PILOTWAVE_ALLOW_ANONYMOUS_SMTP` | Allow SMTP without AUTH, loopback + `USE_TLS=false` only (dev only, Mailcrab) | `false` |
| `PILOTWAVE_PID_FILE` / `LOG_FILE` | `--bg` daemon files | unset (foreground) |

Production roles (see [.env.example](.env.example)):

```sql
GRANT SELECT, UPDATE ON django_tasks_database_dbtaskresult TO pilotwave_worker;
GRANT INSERT, UPDATE ON pilotwave_heartbeat TO pilotwave_worker;
```

Outside `localhost`, non-TLS DSNs and plaintext SMTP fail loud — set `?sslmode=require`.

## Production notes

- Run `pilotwave-worker run --fg` under systemd/Docker with `Restart=always`. `--bg` daemon mode exists but foreground + supervisor is preferred.
- Scale horizontally with more workers — `SKIP LOCKED` prevents double-claim. Expect duplicates only on crash-after-send (at-least-once).
- Monitor `pilotwave_heartbeat` (`key='pilotwave_worker'`): alert if `last_seen` is stale or `metadata.last_error_code` is `smtp_transient`/`smtp_exhausted`.
- Attachments: declare `attachment_paths` + `attachment_sizes` (max 10 files, 6 MiB each, 10 MiB total); paths must canonicalize under `PILOTWAVE_ATTACHMENT_ROOTS`, no symlinks, no `/proc|sys|dev|etc`.
- Backpressure: the worker is the authority — there is no producer-side flood guard. Monitor queue depth and shed load (429) in your view before enqueuing.

## Development

```bash
cp .env.example .env
just pre-install-check
just sync
just dev-up       # Postgres 18 + Mailcrab
just migrate
just test         # pytest (33) + cargo test (84)
just lint
just run-worker -- --fg
just benchmark    # memory benchmark, writes benchmark/results/
just build        # maturin wheel with musl binary
```

Upgrading Postgres major versions needs a fresh volume: `docker compose down -v && just dev-up`.

Layout: `python/` (Django app) · `rust-worker/` (binary) · `contracts/` (boundary contract) · `docs/` (design docs) · `benchmark/` (RSS harness) · `example/` (demo app) · `compose.yaml` (dev only).

Docs entrypoint: [docs/README.md](docs/README.md). Contract: [contracts/CONTRACT.md](contracts/CONTRACT.md).

## License

MIT — see [LICENSE](LICENSE).

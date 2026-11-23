# Pilotwave example app

Minimal Django host that demonstrates the Pilotwave producer API. It is
self-contained: SQLite lives at `example/db.sqlite3`, so it runs with zero
external services. Only the dependencies already in `pyproject.toml`
(`django`, `django-tasks-db`, `psycopg`) are used.

## What it demonstrates

- Enqueueing an email via `pilotwave.tasks.mail_notification.enqueue(...)`
  (never by building `args_kwargs` by hand). The task is
  `pilotwave.tasks.mail_notification` (`@task(queue_name='pilotwave.email')`),
  carrying `reference` / `to` / `subject` plus a fixed test body.
  The worker validates everything.
- Reading queue depth with a plain `DBTaskResult` query.
- Reading the worker heartbeat (`PilotwaveHeartbeat`: `key` / `last_seen` /
  `metadata`).
- PRG: after POST the view redirects to `/?sent=<reference>`, so a page
  refresh never double-enqueues; the `reference` is the idempotency key.
  The enqueue is deferred with `transaction.on_commit` (`Task.enqueue`
  INSERTs immediately and never defers for you).

No attachments — the example stays minimal on purpose. For attachment
roots (`PILOTWAVE_ATTACHMENT_ROOTS`, explicit, no `MEDIA_ROOT` fallback —
P-3), see `docs/05-DEVELOPERS/getting-started.md`.

## Run it

```bash
cd example
uv run python manage.py migrate   # creates db.sqlite3 (queue + heartbeat tables)
uv run python manage.py check     # sanity check
uv run python manage.py runserver # open http://127.0.0.1:8000/
```

Press **Enqueue test email**: the page reloads with `Enqueued. reference:
example-…` and the READY depth grows by one.

## Draining the queue (needs the real worker)

Only `pilotwave-worker` (Rust) drains the `pilotwave.email` queue — no Python
fallback ships (P-5). **Without a live worker the
enqueued rows stay `READY` forever**; that is expected, not a bug. To drain
against a real Postgres host project:

```bash
just run-worker            # oneshot: claim + send one READY task, then exit
just run-worker -- --fg    # persistent foreground loop
```

`just worker-status` shows queue counts plus the heartbeat the worker
writes. This SQLite demo and the Postgres worker do not share a database,
so the demo page shows "No heartbeat yet" until it runs against a
database the worker also writes to.

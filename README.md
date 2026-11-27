# Pilotwave

> Enqueue email in Django, deliver it from a tiny Rust daemon. Postgres is
> the only queue. No Redis, no RabbitMQ, no second Django in RAM.

Pilotwave is a two-part email outbox for Django:

1. **Producer (Python):** your view calls `mail_notification.enqueue(...)`
   — a single Postgres `INSERT` via `django.tasks` + `django-tasks-db`.
   The request returns immediately, no SMTP in the request cycle.
2. **Worker (Rust):** the static `pilotwave-worker` binary polls Postgres
   with `FOR UPDATE SKIP LOCKED`, sends via SMTP, marks the row
   `SUCCESSFUL`/`FAILED`, and upserts a heartbeat row.

```text
Django view --enqueue--> Postgres --claim--> pilotwave-worker --SMTP--> mail server
                                    |                |
                             SUCCESSFUL/FAILED    heartbeat (last_seen + counters)
                                    |                |
                                    v                v
                                 Postgres <-- pilotwave_heartbeat
```

## Why this exists

Small VPS / shared hosting where you can afford **Django + Postgres +
Nginx** and little else:

- A resident Python worker (`db_worker`, Celery worker) costs a full
  Django RSS just to poll — typically 80–180 MB.
- Redis needs its own daemon and memory (~100 MB growth triggered the
  OOM-killer on our shared host), plus backup and monitoring.
- Pilotwave replaces both with one ~5 MB static binary that speaks
  Postgres and SMTP directly.

Measured in this repo: the release worker holds ≈10x less RSS than a
minimal Python baseline doing the same 100-mail run — full table, method
and raw numbers in [benchmark/](benchmark/) (see
[results](benchmark/results/summary.txt)).

That run reproduces only against the benchmark's fake SMTP on `:1026`
(with AUTH, not Mailcrab `:1025`), needs Docker plus a release build,
and there idle RSS equals max. One machine, one run: re-run
`just benchmark` before quoting. We know the benchmark's limits (see
[caveats](benchmark/README.md#honest-caveats));
[feedback is welcome](https://github.com/misaelnieto/pilotwave/issues).

## How does it look like?

Install the wheel, configure `TASKS`, enqueue from your view, drain with
`run` and check with `status` — full walkthrough in
[01-QUICKSTART.md](docs/07-USER-GUIDE/01-QUICKSTART.md).

That example is deliberately simple; production code needs more careful
patterns (see
[Transactions](https://docs.djangoproject.com/en/6.1/topics/tasks/#transactions)
in Django's tasks docs). In particular, if the view runs in an atomic
transaction, defer the enqueue with `transaction.on_commit` so the
worker never claims the row before it is committed.

Interested?

- A quick start introduction is on [docs/07-USER-GUIDE/](docs/07-USER-GUIDE/).
- A runnable app lives in [example/README.md](example/README.md).

## Limitations (alpha)

> **Status: alpha.** Email-only, API unstable, hardening in
> progress.

- **At-least-once delivery.** A crash after send can duplicate a mail.
  Producers must be idempotent by `reference`. No dedupe table in place yet.
- **Email-only, Postgres-only.** Only `pilotwave-worker` drains the
  `pilotwave.email` queue; no Python fallback ships.
- **Polling, not push.** Expect seconds of delay; size limits are
  documented in [02-CONFIGURATION.md](docs/07-USER-GUIDE/02-CONFIGURATION.md),
  SSOT [contracts/CONTRACT.md](contracts/CONTRACT.md).
- **One benchmark.** Numbers above come from a single host/run — re-run
  `just benchmark` before quoting.

## Where to go next

- **Try it:** full quickstart in
  [01-QUICKSTART.md](docs/07-USER-GUIDE/01-QUICKSTART.md), settings in
  [02-CONFIGURATION.md](docs/07-USER-GUIDE/02-CONFIGURATION.md), and
  production notes (systemd, heartbeat, `is-alive` + cron, attachments,
  backpressure) in [03-DEPLOYMENT.md](docs/07-USER-GUIDE/03-DEPLOYMENT.md).
- **Develop:** start at
  [getting-started](docs/05-DEVELOPERS/getting-started.md), then the
  [docs map](docs/README.md).
- **Benchmarks:** method, scripts, and results in
  [benchmark/README.md](benchmark/README.md).
- **Contract:** the Python/Rust boundary in
  [contracts/CONTRACT.md](contracts/CONTRACT.md).

## License

MIT — see [LICENSE](LICENSE).

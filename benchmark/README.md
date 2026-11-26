# Memory benchmark: Rust worker vs minimal Python baseline

## What it measures

Resident memory (VmRSS from `/proc/<pid>/status`, plus Pss from
`/proc/<pid>/smaps_rollup`) of two queue-draining workers doing the
**same job**: claim one READY row at a time from
`django_tasks_database_dbtaskresult` (`FOR UPDATE SKIP LOCKED`), send it
via SMTP to the benchmark's accept-all fake SMTP server
(`benchmark/fake_smtp.py` on port 1026), mark it `SUCCESSFUL`, upsert
the `pilotwave_heartbeat` row.

Why a fake SMTP instead of mailcrab:1025? Mailcrab advertises no AUTH
extension; since ADR 003 the worker can drain it with
`PILOTWAVE_ALLOW_ANONYMOUS_SMTP=true` (loopback + plaintext opt-ins), but
the benchmark keeps the fake server so it keeps exercising the AUTH path.
The fake advertises `AUTH PLAIN LOGIN` and accepts anything (archiving
deliveries under `benchmark/results/mail/`); it also accepts
unauthenticated senders for the Python baseline.

- **Phase A (python):** `benchmark/reference_worker.py` run as
  `.venv/bin/python` — stdlib + psycopg only, no Django, no `uv run`.
- **Phase B (rust):** release build
  (`rust-worker/target/release/pilotwave-worker run --fg`), never
  `cargo run` (debug).

Each phase records an RSS/Pss timeseries (1 Hz) covering **idle**
(empty queue, 60 s) **and** the drain of the same 100 tasks, then
`benchmark/results/summary.txt` reports peak/mean RSS overall and idle,
plus the python/rust ratio.

## How to run

```sh
just benchmark
# or directly:
./benchmark/run_benchmark.sh
# knobs: IDLE_SECS=60 TASKS_CSV=benchmark/tasks_100.csv ./benchmark/run_benchmark.sh
```

The script brings up `db` + `mail` via docker compose, applies
`benchmark/schema.sql` (benchmark-only DDL — see below), builds the
release binary, loads 100 rows, measures phase A, resets the queue,
measures phase B, and writes
`benchmark/results/{python,rust}.csv` + `summary.txt`
(+ `*.time` peak-RSS from `/usr/bin/time -v` when available).

Quick smoke test (5 rows, no full run):

```sh
docker compose up -d db mail
docker compose exec -T db psql -U pilotwave -d pilotwave < benchmark/schema.sql
DATABASE_URL=postgres://pilotwave:pilotwave@localhost:5432/pilotwave \
  .venv/bin/python benchmark/load_csv.py --count 5
```

Why `schema.sql` instead of `just migrate`? The benchmark workers talk
raw SQL (psycopg / sqlx) and never import Django, so Django migrations
are unnecessary scaffolding. `benchmark/schema.sql` creates exactly the
two tables the benchmark touches and is column-compatible with the
contract mirrors (`contracts/sql/001-*.sql`, `002-*.sql`). Development/production
schema management still goes through Django migrations (dev compose now
uses `postgres:18`).

## How to interpret

Compare `rss_idle_mean_kb` (steady-state footprint on a shared/VPS-like
host) and `rss_max_kb` (worst case while draining). A ratio clearly > 1
means the Rust worker holds less resident memory doing the same work.
Pss additionally discounts shared pages, so it is the fairer number when
both workers run side by side with the same libc/Postgres client libs.

## Honest caveats

- Postgres-as-queue + at-least-once: the benchmark inherits the
  contract's semantics (P-4). Duplicates are possible on crash; the
  script never crashes mid-phase, so counts stay exact.
- Fake SMTP: the accept-all server on port 1026 performs no TLS,
  Real-world TLS handshakes/retries cost memory too, on both sides.
- Not production: loopback DSN without `sslmode=require`, superuser DSN
  (so the grant check passes), tiny 100 ms poll interval to keep the run
  short, single queue, no attachments, no concurrency.
- Best-case Python: the baseline omits Pilotwave's validation,
  path-policy, retry/backoff and zombie-reaper on purpose — a real
  Python worker would only use *more* memory.
- One machine, one run: treat small differences as noise; re-run or
  raise `TASKS_CSV` row counts before quoting numbers.

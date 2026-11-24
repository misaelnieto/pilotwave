# Pilotwave boundary contract

- **Queue table:** `django_tasks_database_dbtaskresult` (owned by `django-tasks-db==0.13.0`)
- **Heartbeat table:** `pilotwave_heartbeat` (owned by Pilotwave)
- **Status literals:** `READY`, `RUNNING`, `SUCCESSFUL`, `FAILED`
- **Latency queue:** `pilotwave.email` (default, configurable)
- **Shipped task:** `pilotwave.tasks.mail_notification` (only `task_path` the worker drains by default)
- **Backend name:** `default` (default, configurable)

## Invariants

### Queue row

| Column                 | Type          | Worker use                              |
| ---------------------- | ------------- | --------------------------------------- |
| `id`                   | `uuid`        | claim / finalize key                    |
| `status`               | `varchar`     | claim + finalize                        |
| `task_path`            | `text`        | allowlist filter                        |
| `args_kwargs`          | `jsonb`       | payload (see below)                     |
| `priority`             | `integer`     | `ORDER BY DESC`, range −100..100        |
| `run_after`            | `timestamptz` | due filter + retry scheduling           |
| `queue_name`           | `varchar(32)` | queue filter                            |
| `backend_name`         | `varchar(32)` | backend filter                          |
| `enqueued_at`          | `timestamptz` | FIFO tiebreak                           |
| `started_at`           | `timestamptz` | claim timestamp                         |
| `finished_at`          | `timestamptz` | finalize timestamp                      |
| `worker_ids`           | `jsonb` (array) | attempt counter (append on claim)     |
| `return_value`         | `jsonb`       | unused (email returns nothing)          |
| `exception_class_path` | `text`        | failure code on `FAILED`               |
| `traceback`            | `text`        | sanitized + truncated detail            |

**Sentinel:** `run_after = 9999-01-01T00:00:00Z` means "not deferred". A due
check must be `(run_after <= NOW() OR run_after = sentinel)` — a bare
`run_after <= NOW()` misses every non-deferred row.

**Claim** is one atomic statement: `FOR UPDATE SKIP LOCKED`, ordered by
`priority DESC, run_after ASC, enqueued_at ASC`, appending the worker id to
`worker_ids` as **JSON** (`worker_ids || to_jsonb($worker_id::text)`).

### Payload (`args_kwargs`)

`django-tasks-db==0.13.0` writes `args_kwargs` as `{"args": [...], "kwargs":
{...}}`. The stored row looks like:

```json
{
  "args": [],
  "kwargs": {
    "reference": "<opaque idempotency key>",
    "to": "team@example.com",
    "subject": "Nuevo formulario: Contacto",
    "text_body": "...",
    "html_body": "...",
    "attachment_paths": ["/absolute/media/path.pdf"],
    "attachment_sizes": [123456]
  }
}
```

A `schema` key is ignored wherever it appears (old rows carrying one keep
draining). There is no version gate.

Rules (all enforced by the worker, never delegated to the producer):

- `to`: single addr-spec, printable ASCII, exactly one `@`, no spaces, `<`,
  `>` or controls. Violation → `pilotwave_worker.header_violation`, no retry.
- `subject`: ≤ 500 characters, no controls (`\r`, `\n`, `\u0000`–`\u001f`,
  `\u007f`). Same disposition.
- At least one of `text_body` / `html_body` is required. Each ≤ 1 MiB;
  the whole `args_kwargs` ≤ 2 MiB.
- `reference` is an opaque idempotency/audit key. It is echoed in logs only —
  **never** in heartbeat metadata.
- Attachments: `attachment_paths` and `attachment_sizes` must be arrays of
  equal length. Max 10 attachments, 6 MiB per file, 10 MiB total. Declared
  sizes are checked **before touching disk**.
- Path policy: canonicalize, reject symlinks and `/proc`, `/sys`, `/dev`,
  `/etc`, require the result under the configured attachment roots.
  `ENOENT`/`EACCES` → `pilotwave_worker.attachment_missing` (permanent).

### Failure codes

All permanent failures are written verbatim to `exception_class_path`:

- `pilotwave_worker.schema_error`
- `pilotwave_worker.header_violation`
- `pilotwave_worker.path_policy_violation`
- `pilotwave_worker.attachment_missing`
- `pilotwave_worker.smtp_permanent`

Transient failures are re-queued (`status = READY`, `run_after = NOW() + delay`)
up to 5 attempts; at the cap → `pilotwave_worker.smtp_exhausted` (persisted to
`exception_class_path`).

`smtp_transient` (`pilotwave_worker.smtp_transient`) is the only code that is
never persisted: after a requeue it is returned as the cycle's error code and
is valid only in heartbeat `metadata.last_error_code`. It never appears in
`exception_class_path` or `traceback`.

`traceback` always passes through secret redaction (`sanitize_detail`) +
truncation to exactly 4096 bytes (`TRACEBACK_MAX_BYTES=4096`), cutting on a
UTF-8 char boundary so multibyte sequences are never split.

#### Claim / zombie reaper

Every `drain_once` first runs the crash-recovery reaper, then a generic claim:

```sql
UPDATE django_tasks_database_dbtaskresult
SET status = 'READY'
WHERE status = 'RUNNING'
  AND queue_name = $1
  AND backend_name = $2
  AND task_path = ANY($3)
  AND started_at < NOW() - INTERVAL '5 minutes'
  AND worker_ids->>-1 LIKE 'pilotwave-%'
```

- Window: `started_at` older than 5 minutes — a live claim never stays
  `RUNNING` that long without finalizing; anything older is a crashed
  worker's orphan.
- Filter `worker_ids->>-1 LIKE 'pilotwave-%'`: only rows last claimed by this
  worker family are recycled. Rows last claimed by any other worker family
  (different `worker_ids` tail) are never touched. v1 ships no other
  claimer: the `pilotwave.email` queue is drained only by `pilotwave-worker` (P-5).
- Scope (`queue_name`, `backend_name`, `task_path` allowlist) matches the
  claim scope, so the reaper cannot steal another queue's work.
- Motive is crash-recovery under at-least-once semantics (P-4): a reaped row
  may already have been sent before the crash, so producers must stay
  idempotent by `reference`. Duplicates are possible; losses are not.

### Heartbeat

Single row with `key = 'pilotwave_worker'`. Written by the worker with
`INSERT ... ON CONFLICT (key) DO UPDATE`, `last_seen = NOW()` set by the
database. `metadata` carries counters and operational identity only
(`version`, `queue_depth_seen`, `tasks_processed`, `last_task_id`,
`last_error_code`, `pid`, `uptime_secs`, `rss_kb`) — never PII.

## Pending decisions

> All items are now **decided** -- no open bets remain. The table below is
> the frozen record; behaviour follows the Decision column.
>
> Decisions (normative): P-1/P-2/P-3 decided 2026-11-21
> (traceback is exactly 4096 bytes; `smtp_transient` lives only in
> `metadata.last_error_code`; the zombie reaper stays as specified).
> P-4/P-5 decided late in the plan:
> at-least-once confirmed with no dedupe table in v1 (reopen only on a real
> duplicate incident); Python fallback eliminated -- the `pilotwave.email` queue is
> drained only by `pilotwave-worker`. P-6 dropped: no version gate, any
> `schema` key is ignored.

| ID  | Decision | Proposal (with justification) | Rationale | Status |
| --- | -------- | ----------------------------- | --------- | ------ |
| P-1 | **Mirror vs. own queue table.** Long term, do we keep depending on `django-tasks-db`'s table or introduce a Pilotwave-owned table with an adapter? | Keep the mirror for v1: `django.tasks` is the stable producer API and the ORM round-trip test catches drift. Revisit only if a backend upgrade breaks the consumer contract twice. | An owned table would pay fork/adapter cost now; drift stays bounded by the pinned dependency plus the round-trip test. | decided |
| P-2 | **Heartbeat ownership.** Should the heartbeat table remain a Django model/Rust-write hybrid, or be a plain table defined only in `contracts/sql/`? | Keep the Django model in v1: it gives admin visibility and a schema test for free. The Rust writer is the explicit exception to "no Django tables from Rust". | Keeps admin visibility and a schema test; the split is explicit: DDL lives in Django, the worker only writes. | decided |
| P-3 | **Attachment roots default.** Default to `MEDIA_ROOT` (producer's) or require an explicit `PILOTWAVE_ATTACHMENT_ROOTS` always? | Require explicit `PILOTWAVE_ATTACHMENT_ROOTS` always -- no `MEDIA_ROOT` fallback, not even in development. Fail closed when empty/missing. Wiring roots is the Developer/Integrator's responsibility (see `docs/05-DEVELOPERS/getting-started.md`). | A silent default would grant implicit filesystem access; fail-closed keeps the read boundary explicit. | decided |
| P-4 | **Delivery semantics.** At-least-once (current) vs. adding a dedupe table keyed by `reference`. | At-least-once confirmed for v1; producers must be idempotent by `reference`. No dedupe table in v1: a second write in the hot path plus a retention/GC problem, and it would not cover crash-after-send. Reopen only on a real duplicate incident. | A dedupe table adds a second write on the hot path plus a retention/GC burden, and still would not cover crash-after-send. | decided |
| P-5 | **Worker-to-Django fallback.** Should the standalone repo ship a Python fallback task at all, or leave fallback to the consuming project? | Eliminated: no Python fallback ships in v1. The `pilotwave.email` queue is drained only by `pilotwave-worker`. | Two senders would drift apart; a single drainer keeps one behavior to test. | decided |
| P-6 | **Payload version gate.** Dropped: no `schema` check. Any `schema` key is ignored; old rows carrying one keep draining. | Dropped for simplicity (less features): single task, single team shipping both sides. Revisit only with a real v2 payload. | A version gate adds mechanism with no second payload version to gate. | dropped |

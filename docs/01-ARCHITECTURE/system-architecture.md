---
title: Pilotwave System Architecture
type: architecture
status: accepted
date: 2026-11-19
---

# System Architecture

## Goal

Deliver pre-rendered transactional email with single-digit-second latency
without keeping a Python worker resident. Django owns rendering and business
rules; the Rust worker owns delivery.

## Components

### Python application (`python/`)

An installable Django app exposing:

- `mail_notification(...)`: the shipped task enqueued directly via the
  Django Tasks API (`mail_notification.enqueue(...)`), on the
  `pilotwave.email` queue. The worker validates every row.
- `PilotwaveHeartbeat`: the one model Pilotwave owns.
- No Python sender ships in v1 (P-5 decided owner, paso 8): the `pilotwave.email`
  queue is drained only by `pilotwave-worker`.

### Rust binary (`rust-worker/`)

`pilotwave-worker` claims rows from
`django_tasks_database_dbtaskresult`, validates the payload against
`contracts/CONTRACT.md`, sends via SMTP and finalizes the row. It writes the
heartbeat row as an explicit, test-covered exception.

The binary is compiled for `x86_64-unknown-linux-musl` and shipped inside the
Python wheel by maturin (`bindings = "bin"`).

### Contract (`contracts/`)

The SQL mirror, payload examples and rules. Both test suites include the
contract fixtures: Python via an ORM round-trip, Rust via `include_str!`.

## Data flow

1. Producer renders bodies and calls `mail_notification.enqueue(...)` with
   primitives and an opaque `reference` (inside `transaction.on_commit`
   when already in a transaction — `enqueue` INSERTs immediately).
2. `django-tasks-db` inserts a `READY` row with `run_after = 9999-01-01`
   sentinel and `args_kwargs = {"args": [], "kwargs": {...}}`.
3. Worker selects due rows with `FOR UPDATE SKIP LOCKED`, ordered by priority,
   run_after and enqueue time; appends its id to `worker_ids`.
4. Worker validates headers, bodies and attachment path policy.
5. Success → `SUCCESSFUL`; transient → `READY` with backoff (max 5 attempts);
   permanent → `FAILED` with a sanitized failure code.
6. Heartbeat is upserted with a database-generated `last_seen`.

## Trust boundary

The queue table is untrusted: no HMAC, no constraint on `task_path`, a plain
`INSERT`. Therefore the worker treats every row as hostile, uses a
least-privilege DB role, restricts the task-path allowlist, and never reads
host Django model tables.

## Platform

Wheels target Linux x86_64 only. The Rust target is hardcoded to musl in
`rust-worker/.cargo/config.toml` for maximum compatibility.

---
title: is-alive liveness subcommand for cron keep-alive
type: decision
status: accepted
date: 2026-11-26
---

# Decision 004: `is-alive` liveness subcommand for cron keep-alive

## Context

One use case for Pilotwave is to be able to run on shared hosting
environments which usually offer a regular user account with no
systemd and no root and are mostly isolated from other users, so process
supervision is cron + scripts that need a way to query the running
status of the pilotwave daemon. The current `status` command
prints a human table, so a cron script would either parse fragile
prose or require `jq` for JSON — not guaranteed on minimal hosts.
What is missing is a machine-readable liveness signal.

Assumptions: keep-alive every 5 min, single instance via the `--bg` PID lock,
restart threshold ~360 s (one full interval + margin), alert threshold
90-120 s. The building blocks already exist: `daemon::check()`
(lock + `kill -0` + cmdline identity, never stale), idempotent
`run --bg` (already-running is a no-op exit 0), and heartbeat
freshness codified in `heartbeat.rs` (90 s).

## Decision

New top-level, strictly read-only subcommand:

    pilotwave-worker is-alive [--max-age-secs N]   (default N = 90)

Contract:

- stdout: exactly one line, first word machine-matchable:
  `fresh age_secs=.. pid=..`, `stale age_secs=.. pid=..`,
  `down reason=..`, `error reason=..`. Existing WARNING diagnostics
  stay on stderr (`eprintln!`), so command substitution never breaks.
- exit: 0 fresh; 1 stale/down (restartable — cron runs
  `is-alive ... || run --bg`); 2 error (bad config / DB
  unreachable — alert, never blind-restart: respawning against a
  dead Postgres helps nobody).
- Read-only: uses the reader DSN when configured, never writes,
  never signals or starts anything. Like `status`, resolves the
  daemon section before touching the DB, so `down`/`error` still
  print when Postgres is down.
- `--max-age-secs` keeps restart policy with the integrator
  (aiph-crm uses 360 for its 5-min cron); the default preserves the
  current `fresh`/`stale` meaning from `heartbeat.rs`.
- `main() -> ExitCode` maps every `Err` to 1, so stale/down/error
  must return explicit `Ok(ExitCode::from(..))` values.

## Alternatives discarded

- **Flag `status --is-alive`:** same logic, but edits the `Status`
  unit variant into `Status(Args)` plus 3 `matches!` sites in
  `main.rs`, and breaks the existing clap parse test. Hidden in
  `status --help`; `status --is-alive` stutters at the call site.
- **`--check`:** vague in 3 years ("check what?"). In Nagios-land
  the whole command is named `check_*`; here it would dangle off
  an already-checking `status`.
- **`--health`:** overpromises (SMTP, queue depth, disk) against an
  actual daemon+heartbeat scope; a heartbeat-only failure would
  erode trust in the flag instead of the worker.
- **`--porcelain`:** git jargon; unreadable in a justfile without
  opening docs. Rejected on readability.
- **`--liveness`:** k8s jargon; same readability objection.
- **JSON output:** needs `jq`/parsing on minimal hosts, defeating
  a single-purpose check.

## Consequences

- Client keep-alive scripts become a one-liner
  (`pilotwave-worker is-alive --max-age-secs 360 || run --bg`);
  thresholds stay in client scripts, where policy
  belongs.
- New surface: clap help entry, unit + CLI tests, and mandatory
  docs (integration docs, `README.md`) plus a `CHANGELOG.md`
  entry. Audit surface grows by one read-only path (no new writes
  or grants).
- `stop`/`--bg` semantics are untouched; `is-alive` never kills.
  The alive-but-stuck (zombie) restart decision stays a cron
  policy, not worker behaviour.

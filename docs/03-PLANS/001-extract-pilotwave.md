---
title: Extract Pilotwave as standalone project
type: plan
status: done
date: 2026-11-19
---

# PLAN 001: Extract Pilotwave as standalone project

## Objective

Turn the working internal prototype into a
standalone, installable project without losing the contract knowledge that was
learned there.

## Steps

- [x] 1. Write `contracts/`: SQL mirror of `django-tasks-db==0.13.0`,
      heartbeat DDL, payload examples, rules and pending decisions.
- [x] 2. Create the Python scaffold: installable app, model + migration,
      producer API, settings for tests/dev (no Python fallback sender ships,
      per P-5 decision).
- [x] 3. Create the Rust scaffold: crate, musl target config, contract parser
      with tests that read the contract fixtures.
- [x] 4. Create the `justfile` with `sync`, `dev-up/down`, `migrate`,
      `run-worker`, `test`, `lint`, `build`.
- [x] 5. Create `.env.example`, `compose.yaml`, CI workflow and docs.
- [x] 6. **Verify `just sync`, `just test` and `just build` green locally**
      (14 Python + 5 Rust tests; wheel `manylinux1_x86_64` with a static musl
      binary; `just lint` also green).
- [x] 7. Port the full worker loop: claim SQL, retries/backoff, SMTP,
      heartbeat, CLI (`status`, `run`, `list`, `inspect`, `log`).

### Step 7 breakdown (implementation detail)

Ported from the internal prototype in three parts, then reviewed and wired
up:

- [x] 7.1 Prototype inventory (read-only). Deliverable: module-to-duty map,
  SQL literals, constants, dependencies, tests. No code written.
- [x] 7.2 Python side, contract-driven. Files: `python/`, `tests/`.
  Adjusted to the contract: `schema` key ignored everywhere (P-6), no
  host-app imports, no fallback sender (P-5 decision).
- [x] 7.3 Rust persistence core. Files: `rust-worker/src/db/**`,
  `rust-worker/src/contract.rs`, `rust-worker/Cargo.toml`. Ported the queue
  access layer and wired the real dependencies. Must compile with
  `cargo check`.
- [x] 7.4 Rust execution + CLI. Files: `mailer.rs`, `attachments.rs`,
  `retry.rs`, `heartbeat.rs`, `worker.rs`, `daemon.rs`, `cli.rs`, `log.rs`,
  `logging.rs`, `proc.rs`, `config.rs`, `main.rs` in `rust-worker/src/`.
- [x] 7.5 Adversarial review of plan + P-1..P-6. Markdown only (`docs/`,
  `contracts/CONTRACT.md`). No code changes. Verdicts recorded in
  `docs/02-DECISIONS/002-pending-decisions.md` and `contracts/CONTRACT.md`:
  confirm P-1, P-2 (with constraint), P-3, P-4 (no dedupe), P-6 (ignore any
  `schema` key); P-5 pending at that point (confirm only as host-wired
  opt-in, otherwise drop before 1.0). **Follow-up decision applied
  2026-11-21 (see `contracts/CONTRACT.md`):** traceback exactly 4096 bytes;
  `smtp_transient` only in `metadata.last_error_code`; zombie reaper every
  `drain_once` (5 min window + `pilotwave-%` filter); P-6 is strict
  kwargs-only, no root-level compat key.
- [x] 7.6 `justfile` + CI + developer docs. Files: `justfile`, `.github/`,
  `docs/05-DEVELOPERS/`. Working `run-worker`, `worker-status`, `stop`,
  `log` recipes; explicit musl target in `just build`.

  Launch order: 7.1 + 7.2 + 7.3 + 7.5 in parallel; then 7.4 once the
  inventory and the persistence core were ready; finally 7.6. Final
  integration check: `just sync` -> `just test` -> `just lint` ->
  `just build` (done 2026-11-20: 60 Python + 75 Rust green, manylinux1
  x86_64 wheel with static musl binary). Note: the early `contract`/`check`
  subcommands never shipped (real CLI: status/run/list/inspect/stop/log);
  verification = `pilotwave-worker --help` (exit 0) + `contract.rs` tests
  reading `contracts/payloads/email-v1.json`.
- [x] 8. Confirm the pending decisions (P-1 to P-6) -- all
  `decided` 2026-11-21 (P-4 at-least-once, dedupe deferred to roadmap R-0;
  P-5 fallback eliminated, Rust worker only).

## Non-goals for this plan

- Campaign email (stays cron-driven in the host project).
- Python versions before 3.14 or Django versions before 6.x.
- Deploy mechanisms: docker-compose is development-only; production uses the
  wheel + released binary.
- Wheels for platforms other than Linux x86_64.

---
title: Pending decisions
type: decision
status: accepted
date: 2026-11-19
---

# Pending decisions

The brief explicitly said not to assume these answers. All items have been
decided (P-1/P-2/P-3/P-6 on 2026-11-21; P-4/P-5 late in the plan):
**0 open bets**. The table below is the frozen record.
The canonical, most up-to-date table is
[contracts/CONTRACT.md § Pending decisions](../../contracts/CONTRACT.md).

## Decisions applied 2026-11-21 (normative)

1. **Traceback = exactly 4096 bytes.** `TRACEBACK_MAX_BYTES=4096`;
   `sanitize_detail` redacts secrets first, then truncates on a UTF-8 char
   boundary. (Replaces every "4 KiB" wording.)
2. **`smtp_transient` stays and is documented.** `pilotwave_worker.smtp_transient`
   is the only non-persisted code: valid only in heartbeat
   `metadata.last_error_code` after a requeue, never in
   `exception_class_path` or `traceback`.
3. **Zombie reaper stays and is documented.** Each `drain_once` recycles
   `RUNNING` rows with `started_at < NOW() - 5min` and
   `worker_ids->>-1 LIKE 'pilotwave-%'` (crash-recovery, at-least-once;
   rows from other worker families untouched). See `contracts/CONTRACT.md § Claim / zombie
   reaper`.
4. **P-6 dropped.** No version gate: any `schema` key is ignored; old rows carrying one keep draining.
5. **P-4 decided: at-least-once confirmed.** No dedupe table
   in v1; producers stay idempotent by `reference`. Reopen only on a real
   duplicate incident.
6. **P-5 decided: Python fallback eliminated.** No Python
   sender ships; the `email` queue is drained only by
   `pilotwave-worker`.

| ID  | Question | Proposal | Rationale | Status |
| --- | -------- | -------- | --------- | ------ |
| P-1 | Mirror the `django-tasks-db` table or own a Pilotwave table? | Mirror for v1; `django.tasks` is the stable API and the round-trip test catches drift. | An owned table would pay fork/adapter cost now; drift stays bounded by the pinned dependency plus the round-trip test. | decided |
| P-2 | Heartbeat as Django model/Rust write, or plain SQL table? | Django model in v1 for admin + schema test; Rust write is the explicit exception. | Keeps admin visibility and a schema test; the split is explicit: DDL lives in Django, the worker only writes. | decided |
| P-3 | Attachment roots default. | Require explicit `PILOTWAVE_ATTACHMENT_ROOTS` always -- no `MEDIA_ROOT` fallback, not even in dev; fail closed when empty/missing. Wiring roots is the Developer/Integrator's responsibility (install docs). | A silent default would grant implicit filesystem access; fail-closed keeps the read boundary explicit. | decided |
| P-4 | At-least-once vs. dedupe table keyed by `reference`. | Keep at-least-once; producers must be idempotent by `reference`. No dedupe table in v1; reopen only on a real duplicate incident. | A dedupe table adds a second write on the hot path plus a retention/GC burden, and still would not cover crash-after-send. | decided |
| P-5 | Ship the Python fallback task here or in the host project? | Eliminated -- no Python fallback ships; the `email` queue is drained only by `pilotwave-worker`. | Two senders would drift apart; a single drainer keeps one behavior to test. | decided |
| P-6 | **Payload version gate.** Dropped: no `schema` check. Any `schema` key is ignored; old rows carrying one keep draining. | Dropped for simplicity (less features): single task, single team shipping both sides. Revisit only with a real v2 payload. | A version gate adds mechanism with no second payload version to gate. | dropped |

## Historical review note (adversarial; all items decided)

1-line: extraction valid, 0 open bets: P-4 decided (at-least-once,
no dedupe in v1) and P-5 decided (Python fallback eliminated);
P-6 no longer applies (version gate removed). The one that would have hurt
most in production if unresolved was payload validation, followed by P-3
(fail-closed) and P-5 (dead or drifting fallback).
Scale traps not budgeted: `worker_ids` grows unbounded per claim;
claim `ORDER BY` without a partial index `(queue,backend,status)`; TOCTOU on
attachments (declared size vs. real bytes); blocking sleep in any fallback
blocks the worker.
Rule since 2026-11-21: traceback exactly 4096 bytes; `smtp_transient`
only in `metadata.last_error_code`; zombie reaper every `drain_once` (5 min
window + `pilotwave-%` filter; no other claimer in v1 -- only
`pilotwave-worker` drains the queue, P-5 decided).

## Note on P-6 (dropped: no version gate)

The version check was removed for simplicity. The worker ignores any `schema` key; only real fields are validated.

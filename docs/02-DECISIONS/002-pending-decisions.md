---
title: Pending decisions
type: decision
status: pending
date: 2026-11-19
---

# Pending decisions

The brief explicitly said not to assume these answers. The owner has now
confirmed every item (P-1/P-2/P-3/P-6 on 2026-11-21; P-4/P-5 in paso 8):
**0 apuestas abiertas**. The table below is the frozen record.
The canonical, most up-to-date table is
[contracts/CONTRACT.md § Pending decisions](../../contracts/CONTRACT.md).

## Owner decisions applied 2026-11-21 (normative, no sunset)

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
5. **P-4 decided (owner, paso 8): at-least-once confirmed.** No dedupe table
   in v1; producers stay idempotent by `reference`. Reopen only on a real
   duplicate incident.
6. **P-5 decided (owner, paso 8): Python fallback eliminated.** No Python
   sender ships; the `email` queue is drained only by
   `pilotwave-worker`.

| ID  | Question                                                   | Proposal                                                                                   | Status  | Recomendación 7.5 (histórica; todo decidido por el owner en paso 8) |
| --- | ---------------------------------------------------------- | ------------------------------------------------------------------------------------------ | ------- | --------------------------------------------------------------- |
| P-1 | Mirror the `django-tasks-db` table or own a Pilotwave table? | Mirror for v1; `django.tasks` is the stable API and the round-trip test catches drift.      | decided (owner) | **Confirmar** mirror v1. Tabla propia = fork/adapter/doble-write, coste alto ahora; drift acotado por pin `==0.13.0` + round-trip test. |
| P-2 | Heartbeat as Django model/Rust write, or plain SQL table?   | Django model in v1 for admin + schema test; Rust write is the explicit exception.           | decided (owner) | **Confirmar con constraint**: DDL solo Django, Rust solo `INSERT..ON CONFLICT`; test fija `db_table`. Excepción estrecha y documentada. |
| P-3 | Attachment roots default.                                  | Require explicit `PILOTWAVE_ATTACHMENT_ROOTS` always — no `MEDIA_ROOT` fallback, not even in dev; fail closed when empty/missing. Wiring roots is the Developer/Integrator's responsibility (install docs). | decided (owner) | **Confirmar** fail-closed sin fallback. Eliminar = grant implícito de FS (impacto alto/seguridad). |
| P-4 | At-least-once vs. dedupe table keyed by `reference`.        | Keep at-least-once; producers must be idempotent by `reference`. No dedupe table in v1; reopen only on a real duplicate incident. | decided (owner) | **Confirmar** at-least-once; **eliminar** dedupe-table en v1. Dedupe = 2º write en hot path + retención + no cubre crash-tras-send; beneficio no medido. |
| P-5 | Ship the Python fallback task here or in the host project?  | Eliminated — no Python fallback ships; the `email` queue is drained only by `pilotwave-worker`. | decided (owner) | **Eliminar** (mayor riesgo de drift: dos senders). Sin fallback Python en v1; la cola solo la drena `pilotwave-worker`. |
| P-6 | **Payload version gate.** Dropped: no `schema` check. Any `schema` key is ignored; old rows carrying one keep draining. | Dropped for simplicity (less features): single task, single team shipping both sides. Revisit only with a real v2 payload. | dropped | — |

## Veredicto Shark 7.5 (adversarial; histórico — todo decidido por el owner en paso 8)

1-line: extracción válida, 0 apuestas abiertas: P-4 decidida (at-least-once,
sin dedupe en v1) y P-5 decidida (fallback Python eliminado) por el owner
en paso 8; P-6 ya no aplica (version gate eliminado). La única que habría matado en producción de no resolverse era la validación de payload, seguida de P-3
(fail-closed) y P-5 (fallback muerto o con drift).
Trampas de escala no presupuestadas: `worker_ids` crece sin cota por claim;
claim `ORDER BY` sin índice parcial `(queue,backend,status)`; TOCTOU en
adjuntos (size declarado vs. bytes reales); `time.sleep` del fallback bloquea
`db_worker`.
Norma desde 2026-11-21: traceback exactamente 4096 bytes; `smtp_transient`
solo en `metadata.last_error_code`; reaper zombie cada `drain_once` (ventana
5 min + filtro `pilotwave-%`; sin otro claimer en v1 — solo
`pilotwave-worker` drena la cola, P-5 decidida en paso 8).

## Note on P-6 (dropped: no version gate)

The version check was removed for simplicity. The worker ignores any `schema` key; only real fields are validated.

---
title: Extract Pilotwave from AIPH CRM
type: plan
status: done
date: 2026-11-19
---

# PLAN 001: Extract Pilotwave from AIPH CRM

## Objective

Turn the working prototype in the `feat/rust-email-worker` worktree into a
standalone, installable project without losing the contract knowledge that was
learned there.

## Steps

- [x] 1. Write `contracts/`: SQL mirror of `django-tasks-db==0.13.0`,
      heartbeat DDL, payload examples, rules and pending decisions.
- [x] 2. Create the Python skeleton: installable app, model + migration,
      producer API, settings for tests/dev (the skeleton's fallback task was
      eliminated in paso 8 per P-5 owner decision).
- [x] 3. Create the Rust skeleton: crate, musl target config, contract parser
      with tests that read the contract fixtures.
- [x] 4. Create the `justfile` with `sync`, `dev-up/down`, `migrate`,
      `run-worker`, `test`, `lint`, `build`.
- [x] 5. Create `.env.example`, `compose.yaml`, CI workflow and docs.
- [x] 6. **Verify `just sync`, `just test` and `just build` green locally**
      (14 Python + 5 Rust tests; wheel `manylinux1_x86_64` with a static musl
      binary; `just lint` also green).
- [x] 7. Port the full worker loop: claim SQL, retries/backoff, SMTP,
      heartbeat, CLI (`status`, `run`, `list`, `inspect`, `log`).

### Step 7 breakdown (owners + dependencies — no two lots write same files)

- [x] 7.1 INVENTARIO del prototipo (owner: `explore`). Solo lectura del
  prototipo en `<worktree>/feat/rust-email-worker/pilotwave-worker/src/`.
  Entregable = reporte: mapa modulo -> responsabilidad, SQL literals,
  constantes, deps Cargo, tests. No escribe codigo. Sin dependencias.
- [x] 7.2 Parte PYTHON (owner: `python-architect`, depende solo del contrato,
  no de 7.1). Dueno exclusivo: `python/`, `tests/`. Revisar/ajustar al
  contrato: `schema` en kwargs (P-6), round-trip, sin imports de apps del
  host (fallback task eliminada en paso 8 per P-5 owner decision).
- [x] 7.3 NUCLEO RUST de persistencia (owner: `general`, independiente de
  7.1/7.2 salvo contrato). Dueno: `rust-worker/src/db/**`,
  `rust-worker/src/contract.rs`, `rust-worker/Cargo.toml`. Portar `db/*` del
  prototipo + integrar `contract.rs` + deps reales. Debe compilar con
  `cargo check`.
- [x] 7.4 EJECUCION Y CLI Rust (owner: `general`, depende de 7.3 + reporte
  7.1). Dueno: `mailer.rs`, `attachments.rs`, `retry.rs`, `heartbeat.rs`,
  `worker.rs`, `daemon.rs`, `cli.rs`, `log.rs`, `logging.rs`, `proc.rs`,
  `config.rs`, `main.rs` en `rust-worker/src/`.
- [x] 7.5 Revision adversarial PLAN + P-1..P-6 (owner: `biz-hawk`, sin
  dependencias). SOLO edita markdown de `docs/` y `contracts/CONTRACT.md`.
  No toca codigo. Propone eliminar/confirmar opciones con justificacion.
  **Nota 7.5 (hecho):** veredictos en `docs/02-DECISIONS/002-pending-decisions.md`
  + columna "Recomendación 7.5" en `contracts/CONTRACT.md`. Status sigue
  `pending` en todo; decide el owner en paso 8. Resumen: confirmar P-1, P-2
  (con constraint), P-3, P-4 (sin dedupe), P-6 (dual-accept, sunset raíz en
  2.0); P-5 queda pendiente (confirmar solo como opt-in cableado, si no
  eliminar antes de 1.0). **Decisión owner posterior 2026-11-21 (aplica sin
  discutir, ver `contracts/CONTRACT.md`):** traceback exactamente 4096
  bytes; `smtp_transient` solo en `metadata.last_error_code`; reaper zombie
  cada `drain_once` (5 min + filtro `pilotwave-%`); P-6 pasa a kwargs-only
  estricto — compat de raíz eliminada ahora, sin sunset en 2.0.
- [x] 7.6 justfile + CI + docs developers (owner: `general`, depende de 7.4).
  Dueno: `justfile`, `.github/`, `docs/05-DEVELOPERS/`. Recetas honestas
  `run-worker`, `worker-status`, `stop`, `log`; target musl explicito en
  `just build`.

  Orden de lanzamiento: en paralelo 7.1 + 7.2 + 7.3 + 7.5; cuando 7.1 y 7.3
  reporten -> 7.4; finalmente 7.6. Integracion y verificacion final por el
  coordinador (PASO 2): `just sync` -> `just test` -> `just lint` ->
  `just build` (hecho 2026-11-20: 60 Python + 75 Rust verdes, wheel
  manylinux1_x86_64 con binario musl static-pie x86-64). Nota: los
  subcomandos `contract`/`check` de la era skeleton no existen ni en el
  prototipo ni en el port (CLI real: status/run/list/inspect/stop/log);
  verificacion = `pilotwave-worker --help` (exit 0) + tests de
  `contract.rs` que leen `contracts/payloads/email-v1.json`.
- [x] 8. Confirm the pending decisions (P-1…P-6) with the owner — all
  `decided (owner)` 2026-11-21 (P-4 at-least-once, dedupe → roadmap R-0;
  P-5 fallback eliminated, Rust worker only).

## Non-goals for this plan

- Campaign email (stays cron-driven in the host project).
- Python versions before 3.14 or Django versions before 6.x.
- Deploy mechanisms: docker-compose is development-only; production uses the
  wheel + released binary.
- Wheels for platforms other than Linux x86_64.

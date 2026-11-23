# Architecture

| Document                                   | Description                                      |
| ------------------------------------------ | ------------------------------------------------ |
| [System Architecture](system-architecture.md) | Components, data flow, trust boundary          |

Pilotwave has two processes and one boundary:

```text
 Host Django project                pilotwave-worker (musl)
 ┌──────────────────┐               ┌────────────────────────┐
 │ producer app     │               │ claim (SKIP LOCKED)    │
 │ django.tasks ────┼── Postgres ───┤ validate payload       │
 │ pre-render mail  │   queue table │ SMTP send              │
 └──────────────────┘               │ heartbeat write        │
                                    └────────────────────────┘
```

The queue table is the trust boundary: rows are unsigned, untrusted input.
All validation is enforced in the worker, even for rows the producer wrote.

See [contracts/CONTRACT.md](../../contracts/CONTRACT.md) for the column and
payload tables.

# Contracts

This directory is the **single source of truth for the boundary between Django
and `pilotwave-worker`**. Both sides depend on it; neither side may invent
columns, status literals or payload keys that are not written here.

## Layout

| Path                                       | Purpose                                                          |
| ------------------------------------------ | ---------------------------------------------------------------- |
| [`CONTRACT.md`](CONTRACT.md)               | Version, invariants, pending decisions                           |
| [`sql/001-django-tasks-db-0.13.sql`](sql/001-django-tasks-db-0.13.sql) | Reference DDL mirroring `django-tasks-db==0.13.0` (read-only copy) |
| [`sql/002-pilotwave-heartbeat.sql`](sql/002-pilotwave-heartbeat.sql) | DDL of the one table Pilotwave owns                               |
| [`payloads/email-v1.json`](payloads/email-v1.json) | Minimal `schema: 1` email payload                         |
| [`payloads/email-v1-attachments.json`](payloads/email-v1-attachments.json) | Email payload with declared attachments                     |

## Rules

1. Changing a contract requires a bump in `CONTRACT.md` **and** a test on both
   sides. A schema change without the Python round-trip test and the Rust
   parser test is not a contract change, it is drift.
2. `django-tasks-db` owns the queue table. The SQL in `sql/` is a **reference
   mirror**, not a migration Pilotwave applies: the table is created and
   migrated by Django. Never add a column to the mirror that Django 6.x + the
   pinned backend do not create.
3. Rows in the queue table are **untrusted input**. Validation is enforced in
   the worker regardless of what the producer believes it wrote.
4. Only primitives cross the boundary: strings, numbers, arrays of strings and
   arrays of integers. No templates, no Django objects, no Base64 blobs.

---
title: Extract Pilotwave contract-first
type: decision
status: accepted
date: 2026-11-19
---

# Decision 001: Extract Pilotwave contract-first

## Context

The `pilotwave-worker` prototype was ported from an internal prototype. It works and is extracted into its own repo.
Django and Rust both encode assumptions about the queue schema and payload;
changing either side first creates drift.

## Decision

Start the new repository with `contracts/` as the single source of truth, then
build the Python and Rust skeletons and the `justfile`, and verify `just sync`,
`just test` and `just build` before porting functionality.

## Justification

- Both consumers are generated from one written contract and one set of
  fixtures (`include_str!` in Rust, JSON reads in Python).
- An ORM round-trip test catches semantic drift (status renames, sentinel
  changes) that a column listing would miss.
- `just` is the only developer/CI entry point, so local and CI behaviour
  cannot diverge.

## Consequences

- The first deliverable is deliberately thin: no claim loop or SMTP yet.
- The queue table is mirrored, not owned; a backend upgrade must be reviewed
  against `contracts/sql/001-django-tasks-db-0.13.sql`.

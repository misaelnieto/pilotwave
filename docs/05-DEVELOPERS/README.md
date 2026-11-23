# Developer Guide

| Document                              | Description                                  |
| ------------------------------------- | -------------------------------------------- |
| [Getting Started](getting-started.md) | Local setup and the `just` recipes           |
| [CI and Build](ci-and-build.md)       | Wheels, musl target and CI parity            |

The self-contained demo host lives in [`example/`](../../example/README.md):
a one-page Django app (SQLite) that enqueues via `mail_notification.enqueue(...)`
and shows queue depth plus the worker heartbeat.

Everything goes through `just`. Run `just --list` for the full recipe list.

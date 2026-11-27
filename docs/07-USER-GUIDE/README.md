# Pilotwave User Guide

This guide is for Django developers who installed the `pilotwave` wheel
and want to send mail. No Rust toolchain, no repo clone needed. By the
end of `01` you will have sent one mail; `02` is the settings reference;
`03` is how to run it in production.

If you want to contribute, benchmark, or change the worker, see the
[developer docs](../05-DEVELOPERS/getting-started.md) instead — this
guide never uses `just` or Rust.

## Contents

| Guide | You will… |
| --- | --- |
| [01-QUICKSTART.md](01-QUICKSTART.md) | Install via `uv`/`pip`, configure Django, enqueue one mail, `run` + `status`, verify it arrived. |
| [02-CONFIGURATION.md](02-CONFIGURATION.md) | Full `PILOTWAVE_*` / `EMAIL_*` reference, attachment roots, dev SMTP without AUTH, read-only DSN. |
| [03-DEPLOYMENT.md](03-DEPLOYMENT.md) | `run --fg` under systemd/Docker, `is-alive` + cron keep-alive, heartbeat monitoring. |

> [!NOTE]
> The `pilotwave-worker` binary ships Linux-only (musl); macOS and
> Windows builds are not shipped yet.

Prerequisites: Python 3.14, Django 6.x, Postgres 15+, and an SMTP server —
details in the [quickstart](01-QUICKSTART.md).

A runnable app lives in [example/README.md](../../example/README.md).

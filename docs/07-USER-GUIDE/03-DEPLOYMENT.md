# Deploying Pilotwave

This guide is for the operator: you installed the `pilotwave` wheel,
finished the [quickstart](01-QUICKSTART.md), tuned the settings in
the [configuration reference](02-CONFIGURATION.md), and now want the
worker running day and night. It treats Pilotwave as a black box:
what to run, what to watch, and examples you can copy.

> [!NOTE]
> Pilotwave is alpha software in the process of hardening. If you
> find a problem, please
> [open a bug](https://github.com/misaelnieto/pilotwave/issues)
> with information about how to reproduce it.

Every setting below (`PILOTWAVE_*`, `EMAIL_*`) is described in full
in [02-CONFIGURATION.md](02-CONFIGURATION.md) — this guide names
them but never repeats their values or validation rules. Start from
the complete production-shaped example there.

## Run it under a supervisor (preferred)

The production shape is one command kept alive by something else:

```bash
pilotwave-worker run --fg
```

It loops forever in the foreground and stops cleanly on SIGTERM or
Ctrl-C. Put it under systemd or Docker with `Restart=always` so a
crash — or the worker's deliberate self-restart after many tasks —
comes back on its own. (A bare `pilotwave-worker run` with no flags
only drains what is waiting and exits; handy for a manual check,
not for production.)

Example systemd unit (adjust paths and user to your machine):

```ini
[Unit]
Description=Pilotwave email worker
After=network.target

[Service]
Type=simple
User=pilotwave
Group=pilotwave
WorkingDirectory=/srv/app
EnvironmentFile=/etc/pilotwave/worker.env
ExecStart=/srv/app/.venv/bin/pilotwave-worker run --fg
Restart=always
RestartSec=5

[Install]
WantedBy=multi-user.target
```

```bash
systemctl enable --now pilotwave-worker
systemctl status pilotwave-worker
journalctl -u pilotwave-worker -f   # live logs
```

Example Docker Compose entry (your image already contains the venv):

```yaml
services:
  worker:
    image: my-app:latest
    command: ["pilotwave-worker", "run", "--fg"]
    env_file: ["worker.env"]
    restart: always
```

```bash
docker compose up -d worker
docker logs -f <container>   # live logs
```

Why foreground plus a supervisor beats the daemon mode below:

- The supervisor owns restarts (`Restart=always` with backoff), so a
  crash or a deliberate self-restart always comes back.
- There is no pidfile to go stale — no "is it really running?"
  guessing after an unclean shutdown.
- Logs go to the supervisor (`journalctl`, `docker logs`), which
  already rotates and ships them.
- Stops are clean: the supervisor sends SIGTERM and the worker
  finishes its current send before exiting.

Under a supervisor, leave the log file setting unset so logs go to
standard output; how to read them is shown above. Set a stable
`PILOTWAVE_ID` per machine (see
[02-CONFIGURATION.md](02-CONFIGURATION.md)) so you can tell the
workers apart in logs and task history.

## Daemon mode without a supervisor (`run --bg`)

On hosts with no process supervisor (plain shared hosting), run the
same loop detached:

```bash
pilotwave-worker run --bg
```

It writes its process id to `PILOTWAVE_PID_FILE` and appends logs to
`PILOTWAVE_LOG_FILE` (both documented in
[02-CONFIGURATION.md](02-CONFIGURATION.md)). Operate it with:

```bash
pilotwave-worker stop        # SIGTERM, wait, SIGKILL if needed;
                             # idempotent, never touches the DB
pilotwave-worker log -n 50   # last 50 lines of the log file
pilotwave-worker log -f      # follow new lines (Ctrl-C to quit)
pilotwave-worker log -n 50 -f  # last 50 lines, then follow
```

`stop` with no daemon running exits 0 — safe to call blindly. `log`
is read-only and never touches the database.

Prefer the supervisor mode whenever you have the choice: nothing
restarts a `--bg` daemon after a crash, and a pidfile left behind by
a killed machine needs a human (or the cron probe below) to notice.

## Keep-alive without systemd (cron + `is-alive`)

If cron is all you have, `is-alive` is the probe and cron is the
supervisor. It prints exactly one machine-readable line on stdout:

```text
fresh age_secs=3 pid=1234     # exit 0: nothing to do
stale age_secs=400 pid=1234   # exit 1: restart with run --bg
down reason=no_heartbeat      # exit 1: restart with run --bg
error reason=db_unavailable   # exit 2: alert, never restart
```

`run --bg` is idempotent: if a worker is already running it is a
no-op exit 0, so a blind restart on exit 1 is always safe. Exit 2
means retrying cannot help — paging is the only correct response.

The restart threshold is yours via `--max-age-secs` (default 90):
allow one full cron interval plus margin. A 5-minute cron uses 360.
Copy-paste one-liner (cron must see the same variables as the
worker, hence the env file; only exit 1 restarts):

```cron
*/5 * * * * . /etc/pilotwave/worker.env && flock -n /run/pilotwave/keepalive.lock pilotwave-worker is-alive --max-age-secs 360; [ $? -eq 1 ] && pilotwave-worker run --bg
```

`flock -n` keeps two probe runs from overlapping. Point it at the
real binary path (for example `/srv/app/.venv/bin/pilotwave-worker`)
if `pilotwave-worker` is not on cron's `PATH`.

## Monitoring

The worker upserts one row into the `pilotwave_heartbeat` table on
every loop: `last_seen` plus counters and identity in `metadata`.
Alert when `last_seen` stops advancing:

```sql
SELECT last_seen, NOW() - last_seen AS age,
       metadata->>'last_error_code' AS last_error
FROM pilotwave_heartbeat
WHERE key = 'pilotwave_worker';
```

If `age` grows past a few minutes, the worker is dead or stuck —
this is the signal your uptime check should watch. `last_error`
tells you the most recent send problem the worker saw:

- `pilotwave_worker.smtp_transient` — the mail server hiccuped; the
  mail stays queued and is retried. Normal during an outage.
- `pilotwave_worker.smtp_exhausted` — a mail used up all its
  attempts and was marked failed. Look at it with `list failed` and
  `inspect <UUID>` and fix the underlying cause (bad address, dead
  server, rejected login).

For a human check, `pilotwave-worker status` prints queue counts plus
one `daemon:` line (for `--bg` mode) and one `heartbeat:` line
(fresh/stale, tasks processed, last error) in plain text.

## Logs

Without `PILOTWAVE_LOG_FILE` the worker logs to standard output —
read them with `journalctl` or `docker logs` as shown above. With
`PILOTWAVE_LOG_FILE` set (daemon mode), logs append to that file and
`pilotwave-worker log -n/-f` reads them.

Rotation is safe either way: if the file is replaced (logrotate
`create`) or shrunk (logrotate `copytruncate`), the worker prints a
warning and reopens from the start of the new file — it never keeps
writing to a deleted file and never re-reads old lines as new.

## Scaling to N workers

> [!NOTE]
> Pilotwave is alpha software in the process of hardening, and
> multi-worker setups get the least testing. If you find a problem,
> please [open a
> bug](https://github.com/misaelnieto/pilotwave/issues) with
> information about how to reproduce it.

To drain faster, run N copies of the same command — N systemd units
(or instances), N containers — all pointed at the same database.
Give each worker its own stable `PILOTWAVE_ID` (see
[02-CONFIGURATION.md](02-CONFIGURATION.md)).

Task claiming is atomic: two workers never grab the same task, so
under normal operation a mail is sent exactly once no matter how
many workers you run. The exception is a crash mid-send:
after a worker dies, its in-flight task returns to the queue and
another worker picks it up — possibly sending a mail that already
went out. Delivery is therefore at-least-once.

Make your producers idempotent by `reference`: enqueue with a stable
business key (the [quickstart](01-QUICKSTART.md) uses
`reference=f"order-{order.id}"`) and design the receiving end so a
repeated `reference` is harmless — the second delivery of
`order-42` must not charge or notify twice.

## Hardening checklist

- [ ] Least-privilege database roles: the worker connects with the
  limited role and operators check with the read-only one — see
  [02-CONFIGURATION.md](02-CONFIGURATION.md).
- [ ] Encrypted database connections (`?sslmode=require`) whenever
  the database host is not your own machine — see
  [02-CONFIGURATION.md](02-CONFIGURATION.md).
- [ ] SMTP encrypted with login in production. Never enable the
  plaintext or anonymous opt-ins outside local testing — see
  [02-CONFIGURATION.md](02-CONFIGURATION.md).
- [ ] Attachment directories explicit and minimal. Anything outside
  the listed roots is refused — see
  [02-CONFIGURATION.md](02-CONFIGURATION.md).
- [ ] Backpressure in the view: when the queue grows past what you
  can drain, answer `429` so callers retry later instead of piling
  up rows:

```python
from django.http import HttpResponse

MAX_QUEUED = 1000

def ship_order(request, order_id):
    if email_queue_depth() > MAX_QUEUED:
        return HttpResponse("Email queue full, try again later.",
                            status=429)
    # ... enqueue as usual, see the quickstart
```

```sql
-- email_queue_depth(): READY rows on this worker's queue
SELECT COUNT(*) FROM django_tasks_database_dbtaskresult
WHERE status = 'READY' AND queue_name = 'pilotwave.email';
```

## Next steps

- [01-QUICKSTART.md](01-QUICKSTART.md) — revisit the first-mail flow
  when something above misbehaves.
- [02-CONFIGURATION.md](02-CONFIGURATION.md) — every setting named
  on this page, in full.

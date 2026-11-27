# Pilotwave Configuration

This is the complete reference for every setting `pilotwave-worker`
reads. It is written for the person who installed the `pilotwave` wheel
and wants to send mail: each entry tells you what to put, what happens
if you leave it out, what happens if you get it wrong, and gives you
text you can copy.

If you have not sent your first mail yet, start with the
[quickstart](01-QUICKSTART.md) and come back here when you need to
understand or change a setting. Running the worker in production —
services, restarts, log files — is covered in the
[deployment guide](03-DEPLOYMENT.md).

## How configuration works

Everything is configured through environment variables. You can export
them in your shell or put them in a `.env` file in your project root;
the worker picks that file up automatically. When the same variable is
set both ways, the real environment variable wins over the `.env` file.

The worker only looks at variables starting with `PILOTWAVE_`, variables
starting with `EMAIL_`, and `DEFAULT_FROM_EMAIL`. Any other line in your
`.env` file (for example Django's own `DATABASE_URL`) is ignored by the
worker.

Two rules apply to every variable below:

- **Missing or invalid required setting: the worker refuses to start.**
  It prints an error naming the variable and exits before touching the
  database or the mail server. When in doubt, start the worker once and
  read the first error lines — they tell you exactly which variable to
  fix.
- **Boolean values are flexible but strict.** The following are
  accepted (any capitalization): `true`, `1`, `yes`, `y`, `on` mean on;
  `false`, `0`, `no`, `n`, `off` mean off. Anything else — `maybe`,
  `2`, an accidental trailing word — is an error and the worker
  refuses to start.

## Database

The worker talks to the same Postgres database your Django project
uses, but with its own limited login. Never point the worker at Django's
own connection string.

| Variable | Required? | Default | What it does |
| -------- | --------- | ------- | ------------ |
| `PILOTWAVE_DATABASE_URL` | Yes | — | Worker login for the Django database. Missing or empty: worker refuses to start. |
| `PILOTWAVE_DATABASE_URL_RO` | No | Reuses the worker login | Read-only login used by `status`, `list`, `inspect` and `is-alive`. Missing or empty: those commands reuse the worker login. |

`PILOTWAVE_DATABASE_URL` must use the limited `pilotwave_worker` role,
not Django's full-privilege `DATABASE_URL`. Django owns the tables and
runs migrations; the worker only claims task rows and writes its own
heartbeat. Give this block to your DBA to create the role:

```sql
CREATE ROLE pilotwave_worker LOGIN PASSWORD 'CHANGEME';
GRANT CONNECT ON DATABASE pilotwave TO pilotwave_worker;
GRANT SELECT, UPDATE ON django_tasks_database_dbtaskresult
  TO pilotwave_worker;
GRANT INSERT, UPDATE ON pilotwave_heartbeat TO pilotwave_worker;
```

`PILOTWAVE_DATABASE_URL_RO` is for everyday checking without any write
power: read-only commands use it when it is set, so operators can run
`status` or `inspect` with a login that cannot claim or modify tasks.
Give this block to your DBA:

```sql
CREATE ROLE pilotwave_reader LOGIN PASSWORD 'CHANGEME';
GRANT CONNECT ON DATABASE pilotwave TO pilotwave_reader;
GRANT SELECT ON django_tasks_database_dbtaskresult TO pilotwave_reader;
GRANT SELECT ON pilotwave_heartbeat TO pilotwave_reader;
```

One encryption rule, applying to both `PILOTWAVE_DATABASE_URL` and
`PILOTWAVE_DATABASE_URL_RO`: if the database host is anything other
than your own machine, the connection string must ask for encryption
by ending in `?sslmode=require`, for example:

```dotenv
postgres://pilotwave_worker:CHANGEME@db.internal:5432/pilotwave?sslmode=require
```

Without it, the worker refuses to start — credentials and mail contents
would otherwise travel over the network unencrypted. The same check
applies to the reader login: a remote `PILOTWAVE_DATABASE_URL_RO`
without `?sslmode=require` is rejected before `status`, `list`,
`inspect` or `is-alive` connect. Connections to your
own machine (`localhost`) are allowed without it, which is convenient
for local testing but not something to rely on in production.

## Queue and identity

These tell the worker *which* tasks to pick up and what name it signs
its work with. The defaults match what the quickstart sets up in
Django, so most installations leave all four untouched.

| Variable | Required? | Default | What it does |
| -------- | --------- | ------- | ------------ |
| `PILOTWAVE_TASK_PATHS` | No | `pilotwave.tasks.mail_notification` | Comma-separated list of task types to drain. Empty: worker refuses to start. Change only to drain a legacy queue. |
| `PILOTWAVE_QUEUE_NAME` | No | `pilotwave.email` | Queue to drain. Must match the queue Django enqueues to, or the worker sees an empty queue forever. |
| `PILOTWAVE_BACKEND_NAME` | No | `default` | Task backend to drain. Must match the backend Django enqueues to. |
| `PILOTWAVE_ID` | No | `pilotwave-<process id>` | This worker's name, recorded on every task it claims and in log lines. |

`PILOTWAVE_ID` accepts letters, digits, `_` and `-`, up to 64
characters. Anything else — spaces, slashes, quotes, an empty value —
and the worker refuses to start. The default changes on every restart
(because the process id changes), so set a stable value per machine
such as `PILOTWAVE_ID=web-1` when you run several workers: it is the
only way to tell in the logs and the task history which machine handled
a given mail.

## Timing and retries

These tune how eagerly the worker polls and how stubbornly it retries.
All five are optional; the defaults suit most installations.

| Variable | Default | What it does |
| -------- | ------- | ------------ |
| `PILOTWAVE_POLL_MIN_MS` | `1500` | Wait after finding work, in milliseconds. |
| `PILOTWAVE_POLL_MAX_MS` | `5000` | Longest wait between checks when the queue stays empty, in milliseconds. |
| `PILOTWAVE_TASK_TIMEOUT_SECS` | `30` | Longest time allowed for a single send attempt, in seconds. |
| `PILOTWAVE_MAX_ATTEMPTS` | `5` | How many times one mail is attempted before giving up. Minimum `1`. |
| `PILOTWAVE_RECYCLE_N` | `5000` | Worker restarts itself after this many tasks so a supervisor starts a fresh copy. |

In plain terms:

- **Polling.** When there is mail waiting, the worker picks it up
  immediately. When the queue is empty, it waits before looking again,
  starting at `POLL_MIN_MS` and stretching toward `POLL_MAX_MS` the
  longer nothing arrives. Lower values notice new mail sooner but chat
  with the database more often. `POLL_MIN_MS` must not be larger than
  `POLL_MAX_MS`, or the worker refuses to start.
- **Timeouts.** If one send attempt takes longer than
  `TASK_TIMEOUT_SECS` — typically a slow or stuck mail server — that
  attempt is abandoned and treated like any other temporary failure
  (see retries below).
- **Retries.** Temporary failures (mail server unreachable, connection
  dropped halfway, send timed out) are retried with a growing delay,
  up to `MAX_ATTEMPTS` attempts in total. When the attempts run out,
  the task is marked failed with the code `smtp_exhausted`, which you
  will see in `list` and `inspect`. Problems that retrying cannot fix —
  a malformed address, a blocked attachment, a rejected login — fail on
  the first attempt with a specific code instead of burning through
  retries.
- **Recycling.** After every `RECYCLE_N` tasks (or after a very long
  uptime) the worker exits on purpose so your process supervisor starts
  a fresh copy. This keeps long-running workers healthy. Leave the
  default unless you have a reason not to; the supervisor side is
  described in the [deployment guide](03-DEPLOYMENT.md).

## Attachments

Mails can carry files, but only from directories you explicitly allow.
There is no default and no guessing: if you do not list a directory,
files from it are refused.

| Variable | Required? | Default | What it does |
| -------- | --------- | ------- | ------------ |
| `PILOTWAVE_ATTACHMENT_ROOTS` | Needed for attachments | Empty (refuse everything) | Comma-separated list of absolute directory paths files may be read from. |

```dotenv
PILOTWAVE_ATTACHMENT_ROOTS=/srv/app/media,/var/lib/app/uploads
```

- Every path must be absolute (starting with `/`). A relative path
  makes the worker refuse to start.
- Missing or empty means **every attachment is refused** — the worker
  starts normally but any mail carrying a file fails. This is
  deliberate: mail must never silently gain access to files you did not
  approve. There is no fallback to Django's media directory.
- Each file's real location must sit inside one of the listed
  directories. Shortcuts are rejected: symbolic links, and anything
  under `/proc`, `/sys`, `/dev` or `/etc`, never resolve, no matter
  what you list.
- Size limits per mail: at most **10 files**, at most **6 MiB per
  file**, at most **10 MiB combined**.

What the sender sees when something is wrong: a mail whose file is
outside the allowed directories fails with the code
`path_policy_violation`; a mail whose file is missing or unreadable
fails with `attachment_missing`. Both are permanent — the worker does
not retry them — and both are visible in `inspect`.

## SMTP

These describe the mail server the worker delivers through. Use the same
values Django itself uses (`EMAIL_*` in your Django settings), so the
worker sends exactly what Django would send.

| Variable | Required? | Default | What it does |
| -------- | --------- | ------- | ------------ |
| `EMAIL_HOST` | Yes | — | Mail server hostname. Empty: worker refuses to start. |
| `EMAIL_PORT` | No | `587` | Mail server port, `1`–`65535`. Outside that range, or not a number: worker refuses to start. |
| `EMAIL_USE_TLS` | No | `true` | Encrypt the connection. `false` needs the plaintext opt-in below. |
| `EMAIL_HOST_USER` | With password | — | Login name. Set together with the password or neither. |
| `EMAIL_HOST_PASSWORD` | With username | — | Login secret. Set together with the username or neither. |
| `PILOTWAVE_ALLOW_PLAINTEXT_SMTP` | No | `false` | Test-only opt-in to unencrypted SMTP. |
| `PILOTWAVE_ALLOW_ANONYMOUS_SMTP` | No | `false` | Test-only opt-in to SMTP without login. |
| `DEFAULT_FROM_EMAIL` | Yes | — | Sender shown on mails the task does not override, e.g. `"App <noreply@example.com>"`. Must be a valid mailbox, or the worker refuses to start. |

The safety rules, and what breaks them:

- **Login goes together or not at all.** `EMAIL_HOST_USER` and
  `EMAIL_HOST_PASSWORD` must both be set or both be absent (an empty
  value counts as absent). Setting only one is an error and the worker
  refuses to start — a half-configured login would otherwise fail
  mysteriously on the first mail.
- **Unencrypted mail needs an explicit opt-in.** `EMAIL_USE_TLS=false`
  without `PILOTWAVE_ALLOW_PLAINTEXT_SMTP=true` makes the worker refuse
  to start, because the login and the mail contents would travel
  unencrypted. The opt-in exists for local testing only, never for
  production.
- **Login-free mail is local testing only.** Leaving both login fields
  empty is accepted only when *all* of these hold: the server is your
  own machine (`localhost` and equivalents), encryption is off
  (`EMAIL_USE_TLS=false`), and *both* opt-ins are `true`. If any one of
  them is missing — a remote server, encryption on, or only one opt-in
  — the worker refuses to start. Note the deliberate strictness: a
  misspelled hostname stops the worker instead of sending login-free
  mail to a stranger.

Copy-paste block for local testing (Mailcrab listens for SMTP on port
`1025` with no encryption and no login; its inbox UI is on port `1080`):

```dotenv
EMAIL_HOST=localhost
EMAIL_PORT=1025
EMAIL_USE_TLS=false
PILOTWAVE_ALLOW_PLAINTEXT_SMTP=true
PILOTWAVE_ALLOW_ANONYMOUS_SMTP=true
DEFAULT_FROM_EMAIL="App <noreply@example.com>"
```

Copy-paste block for a real server (encrypted connection with login —
the production shape):

```dotenv
EMAIL_HOST=mail.example.com
EMAIL_PORT=587
EMAIL_USE_TLS=true
EMAIL_HOST_USER=your-user@example.com
EMAIL_HOST_PASSWORD=your-password
DEFAULT_FROM_EMAIL="App <noreply@example.com>"
```

## Daemon files

Two optional settings for running the worker as a background service.
They only say *where* files live; how to run, supervise and rotate
them is described in the [deployment guide](03-DEPLOYMENT.md).

| Variable | Required? | Default | What it does |
| -------- | --------- | ------- | ------------ |
| `PILOTWAVE_PID_FILE` | No | — | Absolute path where the worker writes its process id, e.g. `/run/pilotwave/pilotwave-worker.pid`. Used by the service supervisor to find and stop the worker. |
| `PILOTWAVE_LOG_FILE` | No | — | Absolute path the worker appends its logs to, e.g. `/var/log/pilotwave/worker.log`. |

Both must be absolute paths when set; a relative path makes the worker
refuse to start. When `PILOTWAVE_LOG_FILE` is unset the worker logs to
its standard output, which is what you want when first trying things
out.

## Complete example

A production-shaped `.env` file pulling everything above together.
Adjust the secrets, hosts and paths; everything else can start as
shown:

```dotenv
PILOTWAVE_DATABASE_URL=postgres://pilotwave_worker:CHANGEME@db.internal:5432/pilotwave?sslmode=require
PILOTWAVE_DATABASE_URL_RO=postgres://pilotwave_reader:CHANGEME@db.internal:5432/pilotwave?sslmode=require
PILOTWAVE_QUEUE_NAME=pilotwave.email
PILOTWAVE_BACKEND_NAME=default
PILOTWAVE_TASK_PATHS=pilotwave.tasks.mail_notification
PILOTWAVE_ID=web-1
PILOTWAVE_POLL_MIN_MS=1500
PILOTWAVE_POLL_MAX_MS=5000
PILOTWAVE_TASK_TIMEOUT_SECS=30
PILOTWAVE_MAX_ATTEMPTS=5
PILOTWAVE_RECYCLE_N=5000
PILOTWAVE_ATTACHMENT_ROOTS=/srv/app/media
EMAIL_HOST=mail.example.com
EMAIL_PORT=587
EMAIL_USE_TLS=true
EMAIL_HOST_USER=your-user@example.com
EMAIL_HOST_PASSWORD=your-password
DEFAULT_FROM_EMAIL="App <noreply@example.com>"
PILOTWAVE_PID_FILE=/run/pilotwave/pilotwave-worker.pid
PILOTWAVE_LOG_FILE=/var/log/pilotwave/worker.log
```

## Next steps


- [01-QUICKSTART.md](01-QUICKSTART.md) To review quick start steps
- [03-DEPLOYMENT.md](03-DEPLOYMENT.md) To read about production setup

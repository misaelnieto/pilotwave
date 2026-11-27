# Pilotwave Quickstart

This guide takes you from an empty project to your first delivered email:
you will install the `pilotwave` wheel, configure Django, enqueue one mail,
and drain it with the bundled `pilotwave-worker` binary.

By the end you will have one task marked completed, know how to check the
queue with `status`, and know where to look when something fails.

## Requirements, prerequisites and assumptions

This guide assumes you already have a working Django project. You will add
Pilotwave to it, not start one from scratch.

- Python 3.14, Django 6.x
- Postgres 15+
- An email server. We suggest Mailcrab as your test email server (SMTP on `:1025`, UI on `:1080`), but you can use whatever you want as long as it talks SMTP.

> [!NOTE]
> If you prefer Docker Compose, check the `compose.yaml` in the repo root
> as a quickstart reference for wiring Postgres and Mailcrab together.

> [!WARNING]
> The `pilotwave-worker` binary is currently only built for Linux (musl)
> and should run on most Linux distros. macOS and Windows builds are not
> shipped yet.

## Install

If you use `uv`:

```bash
uv add pilotwave
```

Or if you use `pip`:

```bash
pip install pilotwave
```

The `pilotwave` wheel ships a small Django app and the statically compiled
`pilotwave-worker` binary — no Rust toolchain needed. Verify it with:

```bash
pilotwave-worker --help
```

With `uv` the binary lives in your virtualenv, so you can reach it three
ways: `pilotwave-worker` directly when the venv is activated,
`.venv/bin/pilotwave-worker` (assuming the venv is in `.venv`), or
`uv run pilotwave-worker`.

## Configure Django

Pilotwave builds upon
[django_tasks_db](https://pypi.org/project/django-tasks-db/).

### 1. Installed apps

Register both apps in `INSTALLED_APPS`:

```python
INSTALLED_APPS = [
    "django_tasks_db",
    "pilotwave",
    # …
]
```

### 2. Task backend

Django 6.x ships a built-in tasks framework configured through the `TASKS`
setting — it decides which backend stores your tasks and which queues exist.
If you have never used it, start with the
[Django tasks documentation](https://docs.djangoproject.com/en/6.1/topics/tasks/).
Pilotwave only needs its `pilotwave.email` queue on the database backend:

```python
TASKS = {
    "default": {
        "BACKEND": "django_tasks_db.DatabaseBackend",
        "QUEUES": ["default", "pilotwave.email"],
    }
}
```

> [!NOTE]
> Already running `django-tasks` with another backend? Keep your existing
> `default` backend and queues untouched — just make sure `pilotwave.email`
> resolves to a `django_tasks_db` backend (a second `TASKS` entry or a
> router), so the worker finds its rows where it expects them.

### 3. Enqueue emails from Django

Let's write a small view that sends emails for a given order_id using the `pilotwave` api:

```python
from django.http import HttpResponse
from django.shortcuts import get_object_or_404

from pilotwave.tasks import mail_notification

def ship_order(request, order_id):
    order = get_object_or_404(Order, pk=order_id)
    mail_notification.enqueue(
        to=order.customer_email,
        subject="Your order shipped",
        reference=f"order-{order.id}",
        text_body="Hi, your order is on its way.",
    )
    return HttpResponse("Shipping email queued.")
```

Pilotwave ships a predefined task, `pilotwave.tasks.mail_notification`
(`@task(queue_name="pilotwave.email")`) saving you the work of writing one yourself.

Without a queue, the view itself would need to talk directly to the SMTP server: the user waits
while the mail is sent, and a slow server or a retry blocks the response.
With `django-tasks` + Pilotwave the view only inserts one row into Postgres
and returns immediately — a separate process, `pilotwave-worker`, picks up
the row later and takes care of sending and retrying:

```mermaid
sequenceDiagram
    participant Browser
    participant View as Django view
    participant PG as Postgres
    participant Worker as pilotwave-worker
    participant SMTP as Mail server
    Browser->>View: POST /orders/42/ship
    View->>PG: INSERT task row (pilotwave.email)
    View-->>Browser: 200 "Shipping email queued."
    Worker->>PG: claim row (SKIP LOCKED)
    Worker->>SMTP: send mail
    Worker->>PG: mark SUCCESSFUL / FAILED
```

## Configure and run the worker

### Environment variables

The worker reads its configuration from environment variables (a `.env`
file in your project root works too). For this quickstart, point it at
your Django database and at Mailcrab:

```dotenv
PILOTWAVE_DATABASE_URL=postgres://pilotwave_worker:CHANGEME@localhost:5432/pilotwave
PILOTWAVE_ALLOW_PLAINTEXT_SMTP=true
PILOTWAVE_ALLOW_ANONYMOUS_SMTP=true
EMAIL_HOST=localhost
EMAIL_PORT=1025
EMAIL_USE_TLS=false
DEFAULT_FROM_EMAIL="App <noreply@example.com>"
```

What each line means:

- `PILOTWAVE_DATABASE_URL` — the worker's own database connection: same
  database Django uses, but with the least-privilege `pilotwave_worker`
  role. Replace `CHANGEME` with its real password.
- `EMAIL_HOST` / `EMAIL_PORT` / `EMAIL_USE_TLS` / `DEFAULT_FROM_EMAIL` —
  shared with Django: use the same values as in your Django `settings.py`
  (`EMAIL_*`), so the worker sends exactly what Django would send. Here
  they point at your local Mailcrab.
- `PILOTWAVE_ALLOW_PLAINTEXT_SMTP` / `PILOTWAVE_ALLOW_ANONYMOUS_SMTP` —
  test-only opt-ins Mailcrab needs (no encryption, no login). Copy as-is;
  what they mean in production lives in
  [02-CONFIGURATION.md](02-CONFIGURATION.md).


### Run the worker

1. Enqueue one mail by hitting your `ship_order` view (or the Django
   shell).
2. Check the queue:

```bash
pilotwave-worker status
```

You should see one task available (the `daemon` and `heartbeat` lines are
covered in [03-DEPLOYMENT.md](03-DEPLOYMENT.md)):

```bash
+------------+-------+
| state      | total |
+------------+-------+
| available  |     1 |
| in process |     0 |
| completed  |     0 |
+------------+-------+
```

3. Drain it with a single run:

```bash
pilotwave-worker run
```

4. Check the queue again — the task is now completed:

```bash
pilotwave-worker status

+------------+-------+
| state      | total |
+------------+-------+
| available  |     0 |
| in process |     0 |
| completed  |     1 |
+------------+-------+
```

5. Open Mailcrab at <http://localhost:1080> — your mail should be there.


## Next steps

If everything went well, continue with the settings reference in
[02-CONFIGURATION.md](02-CONFIGURATION.md) or jump to production setup in
[03-DEPLOYMENT.md](03-DEPLOYMENT.md).

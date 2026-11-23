"""Pilotwave shipped task (pure Django Tasks API, no wrapper).

``pilotwave.tasks.mail_notification`` is the only ``task_path`` the Rust
worker drains. Enqueue it directly — ``mail_notification.enqueue(...)`` or
``mail_notification.using(...).enqueue(...)``.

``Task.enqueue`` writes the row immediately: it does NOT defer to
``transaction.on_commit``. Callers inside a transaction must wrap the call
themselves, e.g. ``transaction.on_commit(lambda:
mail_notification.enqueue(...))``.
"""

from datetime import UTC, datetime

from django.tasks import task

TASK_PATH = 'pilotwave.tasks.mail_notification'
DEFAULT_QUEUE = 'pilotwave.email'

# Same sentinel the worker uses: run_after "not deferred" (contract §Queue row).
TASK_RUN_AFTER_SENTINEL = datetime(9999, 1, 1, tzinfo=UTC)


@task(queue_name=DEFAULT_QUEUE)
def mail_notification(**kwargs):
    """Placeholder body; the Rust worker is the real executor.

    Never executed in production — ``pilotwave-worker`` reads the Postgres
    row directly and never imports this function. Only runs in tests /
    eager mode, where returning the reference makes assertions easy.
    """
    return kwargs.get('reference')

"""Load benchmark/tasks_100.csv into the django-tasks-db queue table.

Idempotent: by default it clears the benchmark queue/scope first, so
re-running always yields the same N READY rows. Pass ``--no-clear`` to
append instead, or ``--count N`` for a smoke test.

Runnable as: ``.venv/bin/python benchmark/load_csv.py [--count 5]``
"""

from __future__ import annotations

import argparse
import csv
import json
import os
import uuid

import psycopg

QUEUE_TABLE = 'django_tasks_database_dbtaskresult'
SENTINEL = '9999-01-01T00:00:00Z'


def dsn() -> str:
    return os.environ.get('DATABASE_URL') or os.environ.get('PILOTWAVE_DATABASE_URL', '')


def queue_scope() -> tuple[str, str, str]:
    paths = os.environ.get('PILOTWAVE_TASK_PATHS', 'pilotwave.tasks.mail_notification')
    return (
        os.environ.get('PILOTWAVE_QUEUE_NAME', 'pilotwave.email'),
        os.environ.get('PILOTWAVE_BACKEND_NAME', 'default'),
        paths.split(',')[0].strip(),
    )


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument('--csv', default='benchmark/tasks_100.csv')
    parser.add_argument('--count', type=int, default=None, help='load first N rows (smoke test)')
    parser.add_argument('--no-clear', action='store_true', help='append, do not clear first')
    args = parser.parse_args()
    queue, backend, task_path = queue_scope()

    with open(args.csv, newline='') as fh:
        rows = list(csv.DictReader(fh))
    if args.count is not None:
        rows = rows[: args.count]

    with psycopg.connect(dsn(), autocommit=True) as conn, conn.cursor() as cur:
        if not args.no_clear:
            cur.execute(
                f'DELETE FROM {QUEUE_TABLE} WHERE queue_name=%s AND backend_name=%s',
                (queue, backend),
            )
            print(f'cleared queue={queue} backend={backend} ({cur.rowcount} rows)')
        else:
            cur.execute(
                f"SELECT count(*) FROM {QUEUE_TABLE} WHERE status='READY'"
                ' AND queue_name=%s AND backend_name=%s',
                (queue, backend),
            )
            if cur.fetchone()[0] > 0:
                print('queue already loaded, skipping (--no-clear)')
                return
        for row in rows:
            payload = json.dumps(
                {
                    'args': [],
                    'kwargs': {
                        'reference': row['reference'],
                        'to': row['to'],
                        'subject': row['subject'],
                        'text_body': row['text_body'],
                        'html_body': row['html_body'],
                    },
                }
            )
            cur.execute(
                f'INSERT INTO {QUEUE_TABLE} (id, status, task_path, args_kwargs, priority,'
                f' run_after, queue_name, backend_name, enqueued_at, worker_ids,'
                ' return_value, exception_class_path, traceback)'
                " VALUES (%s::uuid, 'READY', %s, %s::jsonb, 0,"
                f" '{SENTINEL}', %s, %s, NOW(), '[]'::jsonb,"
                " 'null'::jsonb, '', '')",
                (str(uuid.uuid4()), task_path, payload, queue, backend),
            )
    print(f'loaded {len(rows)} READY rows task_path={task_path} queue={queue}')


if __name__ == '__main__':
    main()

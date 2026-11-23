"""Minimal Python baseline worker for the RSS benchmark (stdlib + psycopg only).

Mirrors the Rust hot path (claim via FOR UPDATE SKIP LOCKED -> SMTP send
-> finalize -> heartbeat upsert) and NOTHING else. Omits validation,
path-policy, retry, reaper, grant checks on purpose: best-case Python RSS.
"""

import argparse
import json
import os
import smtplib
import time
from email.message import EmailMessage

import psycopg

QUEUE_TABLE = 'django_tasks_database_dbtaskresult'
HEARTBEAT_TABLE = 'pilotwave_heartbeat'
HEARTBEAT_KEY = 'pilotwave_worker'
SENTINEL = '9999-01-01T00:00:00Z'

CLAIM_SQL = f"""
    UPDATE {QUEUE_TABLE}
    SET status = 'RUNNING',
        started_at = NOW(),
        worker_ids = worker_ids || to_jsonb(%s::text)
    WHERE id = (
        SELECT id FROM {QUEUE_TABLE}
        WHERE status = 'READY'
          AND queue_name = %s
          AND backend_name = %s
          AND task_path = ANY(%s)
          AND (run_after <= NOW() OR run_after = '{SENTINEL}')
        ORDER BY priority DESC, run_after ASC, enqueued_at ASC
        FOR UPDATE SKIP LOCKED
        LIMIT 1
    )
    RETURNING id::text AS id, args_kwargs::text AS args_kwargs,
        jsonb_array_length(worker_ids)::bigint AS attempts
"""

SUCCESS_SQL = f"UPDATE {QUEUE_TABLE} SET status='SUCCESSFUL', finished_at=NOW() WHERE id=%s::uuid"
FAILED_SQL = (
    f"UPDATE {QUEUE_TABLE} SET status='FAILED', finished_at=NOW(),"
    ' exception_class_path=%s, traceback=%s WHERE id=%s::uuid'
)
COUNT_SQL = (
    f"SELECT count(*) FROM {QUEUE_TABLE} WHERE status='READY' AND queue_name=%s"
    ' AND backend_name=%s AND task_path = ANY(%s)'
)
HEARTBEAT_SQL = (
    f'INSERT INTO {HEARTBEAT_TABLE} (key, last_seen, metadata) VALUES (%s, NOW(), %s::jsonb)'
    ' ON CONFLICT (key) DO UPDATE SET last_seen = NOW(), metadata = EXCLUDED.metadata'
)


def env(name: str, default: str = '') -> str:
    return os.environ.get(name, default) or default


def read_rss_kb() -> int | None:
    try:
        with open('/proc/self/status') as fh:
            for line in fh:
                if line.startswith('VmRSS:'):
                    return int(line.split()[1])
    except OSError, ValueError, IndexError:
        return None
    return None


def send_message(
    host: str, port: int, sender: str, to: str, subject: str, text: str, html: str
) -> None:
    msg = EmailMessage()
    msg['From'] = sender
    msg['To'] = to
    msg['Subject'] = subject
    if text and html:
        msg.set_content(text)
        msg.add_alternative(html, subtype='html')
    else:
        msg.set_content(html or text, subtype='html' if html else 'plain')
    with smtplib.SMTP(host, port, timeout=30) as smtp:
        smtp.send_message(msg)


def beat(cur, pid: int, seen: int, n: int, last_id: str | None, last_err: str | None) -> None:
    meta = {
        'version': 1,
        'queue_depth_seen': seen,
        'tasks_processed': n,
        'last_task_id': last_id,
        'last_error_code': last_err,
        'pid': pid,
        'uptime_secs': None,
        'rss_kb': read_rss_kb(),
    }
    cur.execute(HEARTBEAT_SQL, (HEARTBEAT_KEY, json.dumps(meta)))


def run(args: argparse.Namespace) -> int:
    dsn = env('PILOTWAVE_DATABASE_URL') or env('DATABASE_URL')
    queue = env('PILOTWAVE_QUEUE_NAME', 'pilotwave.email')
    backend = env('PILOTWAVE_BACKEND_NAME', 'default')
    task_paths = [
        p.strip()
        for p in env('PILOTWAVE_TASK_PATHS', 'pilotwave.tasks.mail_notification').split(',')
        if p.strip()
    ]
    smtp_host = env('EMAIL_HOST', 'localhost')
    smtp_port = int(env('EMAIL_PORT', '1025'))
    sender = env('DEFAULT_FROM_EMAIL', 'Pilotwave Benchmark <benchmark@example.com>')
    worker_id = env('PILOTWAVE_ID', f'pilotwave-py-{os.getpid()}')
    pid = os.getpid()
    processed, last_id, last_err = 0, None, None

    with psycopg.connect(dsn, autocommit=True) as conn, conn.cursor() as cur:
        while True:
            if args.max_tasks is not None and processed >= args.max_tasks:
                break
            cur.execute(COUNT_SQL, (queue, backend, task_paths))
            seen = cur.fetchone()[0]
            row = cur.execute(CLAIM_SQL, (worker_id, queue, backend, task_paths)).fetchone()
            if row is None:
                beat(cur, pid, seen, processed, last_id, last_err)
                if args.oneshot:
                    break
                time.sleep(args.poll_ms / 1000.0)
                continue
            task_id, args_kwargs, _attempts = row
            try:
                kwargs = json.loads(args_kwargs).get('kwargs', {})
                to = kwargs.get('to', '')
                subject = kwargs.get('subject', '')
                text = kwargs.get('text_body') or ''
                html = kwargs.get('html_body') or ''
                if '@' not in to or not (text or html):
                    raise ValueError('bad payload')
                send_message(smtp_host, smtp_port, sender, to, subject, text, html)
            except Exception as exc:  # noqa: BLE001 — baseline marks FAILED, keeps draining
                cur.execute(
                    FAILED_SQL, ('pilotwave_worker.header_violation', str(exc)[:4096], task_id)
                )
                last_err = 'pilotwave_worker.header_violation'
            else:
                cur.execute(SUCCESS_SQL, (task_id,))
                last_err = None
            processed += 1
            last_id = task_id
            beat(cur, pid, seen, processed, last_id, last_err)
    return 0


def main() -> None:
    parser = argparse.ArgumentParser(description='Minimal Python baseline (benchmark only).')
    parser.add_argument('--oneshot', action='store_true', help='claim one task, then exit')
    parser.add_argument('--max-tasks', type=int, default=None)
    parser.add_argument('--poll-ms', type=int, default=200)
    raise SystemExit(run(parser.parse_args()))


if __name__ == '__main__':
    main()

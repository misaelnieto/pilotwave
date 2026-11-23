"""ORM round-trip tests: the Python side of the boundary contract.

These are deliberately semantic (sentinel, status, filters, payload shape),
not a column listing — a backend upgrade that renames a status or changes
ordering must fail here, not in production.
"""

import json
from datetime import UTC, datetime
from pathlib import Path

import pytest
from django_tasks_db.models import DBTaskResult

from pilotwave.tasks import TASK_RUN_AFTER_SENTINEL, mail_notification

# No on_commit deferral: `Task.enqueue` INSERTs synchronously, so plain
# `django_db` suffices (the old `transaction=True` trap is gone with the
# wrapper).
pytestmark = [pytest.mark.django_db]


def test_enqueue_writes_contract_row():
    mail_notification.enqueue(
        to='team@example.com',
        subject='Hola',
        reference='ref-1',
        text_body='cuerpo',
    )

    row = DBTaskResult.objects.get()
    assert row.status == 'READY'
    assert row.queue_name == 'pilotwave.email'
    assert row.backend_name == 'default'
    assert row.run_after == TASK_RUN_AFTER_SENTINEL
    assert row.worker_ids == []
    assert row.task_path == mail_notification.module_path
    assert row.exception_class_path == ''

    payload = json.loads(json.dumps(row.args_kwargs))
    assert payload == {
        'args': [],
        'kwargs': {
            'reference': 'ref-1',
            'to': 'team@example.com',
            'subject': 'Hola',
            'text_body': 'cuerpo',
        },
    }
    # django-tasks-db stores {"args": [], "kwargs": {...}} with no root key.
    assert set(payload) == {'args', 'kwargs'}
    assert 'schema' not in payload
    assert 'schema' not in payload['kwargs']


def test_ready_queryset_excludes_deferred_rows():
    mail_notification.using(queue_name='pilotwave.email').enqueue(
        reference='ref-2', to='a@b.com', subject='s', text_body='x'
    )
    assert DBTaskResult.objects.ready().count() == 1

    # A deferred row (future run_after, not the sentinel) is not ready.
    DBTaskResult.objects.update(run_after=datetime(2099, 1, 1, tzinfo=UTC))
    assert DBTaskResult.objects.ready().count() == 0

    # The sentinel row is ready again.
    DBTaskResult.objects.update(run_after=TASK_RUN_AFTER_SENTINEL)
    assert DBTaskResult.objects.ready().count() == 1


def test_payload_examples_match_the_contract():
    contracts = Path(__file__).resolve().parents[1] / 'contracts' / 'payloads'
    for name in ('email-v1.json', 'email-v1-attachments.json'):
        payload = json.loads((contracts / name).read_text())
        assert set(payload) == {'args', 'kwargs'}
        assert 'schema' not in payload
        assert 'schema' not in payload['kwargs']


def test_queue_orm_matches_sql_mirror():
    """The ORM model must expose what `contracts/sql/001-*.sql` promises.

    The SQL file is the reviewable mirror of `django-tasks-db==0.13.0`; this
    test fails first if a backend upgrade renames the table, a status, or a
    column the worker's claim depends on.
    """
    sql = (
        Path(__file__).resolve().parents[1] / 'contracts' / 'sql' / '001-django-tasks-db-0.13.sql'
    ).read_text()
    assert DBTaskResult._meta.db_table in sql
    for status in ('READY', 'RUNNING', 'SUCCESSFUL', 'FAILED'):
        assert f"'{status}'" in sql
    assert '9999-01-01' in sql
    field_names = {field.name for field in DBTaskResult._meta.get_fields()}
    assert field_names >= {
        'id',
        'status',
        'task_path',
        'args_kwargs',
        'priority',
        'run_after',
        'queue_name',
        'backend_name',
        'enqueued_at',
        'started_at',
        'finished_at',
        'worker_ids',
        'return_value',
        'exception_class_path',
        'traceback',
    }


def test_heartbeat_model_matches_owned_ddl():
    """The only Django table Pilotwave owns mirrors `002-*.sql` (P-2)."""
    from pilotwave.models import PilotwaveHeartbeat

    sql = (
        Path(__file__).resolve().parents[1] / 'contracts' / 'sql' / '002-pilotwave-heartbeat.sql'
    ).read_text()
    assert PilotwaveHeartbeat._meta.db_table == 'pilotwave_heartbeat'
    assert 'pilotwave_heartbeat' in sql
    field_names = {field.name for field in PilotwaveHeartbeat._meta.get_fields()}
    assert field_names >= {'key', 'last_seen', 'metadata'}
    # Heartbeat metadata carries counters/identity only — never PII: the
    # model default is an empty mapping, populated solely by the worker.
    assert PilotwaveHeartbeat._meta.get_field('metadata').get_default() == {}

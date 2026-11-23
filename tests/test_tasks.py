"""Enqueue path: the shipped task writes a contract-shaped row.

No wrapper exists: producers call ``pilotwave.tasks.mail_notification``
directly (``.enqueue(...)`` / ``.using(...).enqueue(...)``). The worker
validates everything; these tests pin the payload shape only.
"""

import json

import pytest
from django_tasks_db.models import DBTaskResult

from pilotwave.tasks import TASK_RUN_AFTER_SENTINEL, mail_notification

pytestmark = pytest.mark.django_db


def test_task_body_returns_reference():
    assert mail_notification.call(reference='ref-1') == 'ref-1'
    assert mail_notification.call() is None


def test_enqueue_writes_kwargs_only_payload():
    mail_notification.enqueue(
        reference='ref-1',
        to='team@example.com',
        subject='Hola',
        text_body='cuerpo',
    )

    row = DBTaskResult.objects.get()
    assert row.status == 'READY'
    assert row.queue_name == 'pilotwave.email'
    assert row.task_path == mail_notification.module_path
    assert row.task_path == 'pilotwave.tasks.mail_notification'

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


def test_using_run_after_defers_row():
    from datetime import UTC, datetime

    mail_notification.using(run_after=datetime(2099, 1, 1, tzinfo=UTC)).enqueue(
        reference='ref-2', to='a@b.com', subject='s', text_body='x'
    )
    assert DBTaskResult.objects.ready().count() == 0
    DBTaskResult.objects.update(run_after=TASK_RUN_AFTER_SENTINEL)
    assert DBTaskResult.objects.ready().count() == 1


def test_contract_fixtures_are_kwargs_only():
    contracts = __import__('pathlib').Path(__file__).resolve().parents[1] / 'contracts' / 'payloads'
    for name in ('email-v1.json', 'email-v1-attachments.json'):
        payload = json.loads((contracts / name).read_text())
        assert set(payload) == {'args', 'kwargs'}
        assert 'schema' not in payload
        assert 'schema' not in payload['kwargs']

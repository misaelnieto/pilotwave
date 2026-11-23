"""Single-page demo: enqueue a test email and show queue/worker state."""

import uuid

from django.db import transaction
from django.shortcuts import redirect, render
from django_tasks_db.models import DBTaskResult

from pilotwave.models import PilotwaveHeartbeat
from pilotwave.tasks import mail_notification

DEFAULT_TO = 'team@example.com'
DEFAULT_SUBJECT = 'Pilotwave example'


def index(request):
    context = {
        'default_to': DEFAULT_TO,
        'default_subject': DEFAULT_SUBJECT,
        'sent_reference': request.GET.get('sent', ''),
        'depth': DBTaskResult.objects.filter(
            status='READY', queue_name='pilotwave.email'
        ).count(),
        'heartbeat': PilotwaveHeartbeat.objects.filter(key='pilotwave_worker').first(),
    }
    if request.method != 'POST':
        return render(request, 'demo/index.html', context)

    # PRG: enqueue, then redirect so a refresh never double-enqueues.
    to = request.POST.get('to', '').strip() or DEFAULT_TO
    subject = request.POST.get('subject', '').strip() or DEFAULT_SUBJECT
    reference = f'example-{uuid.uuid4().hex[:12]}'
    # `enqueue` INSERTs immediately (no on_commit deferral), so defer it
    # explicitly: an uncommitted request transaction must not publish the row.
    # The worker validates the remaining fields.
    transaction.on_commit(
        lambda: mail_notification.enqueue(
            reference=reference,
            to=to,
            subject=subject,
            text_body='Hello from the Pilotwave example app.',
        )
    )
    return redirect(f'/?sent={reference}')

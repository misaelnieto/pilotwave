"""Settings for standalone local development (docker-compose Postgres).

The host project supplies its own settings in production; this module exists
so `just migrate` and manual exploration work in a fresh checkout.
"""

import os
from pathlib import Path

SECRET_KEY = os.environ.get('PILOTWAVE_SECRET_KEY', 'pilotwave-dev-only')
DEBUG = True
USE_TZ = True
TIME_ZONE = 'UTC'

INSTALLED_APPS = [
    'django.contrib.contenttypes',
    'django.contrib.auth',
    'django_tasks_db',
    'pilotwave',
]

DATABASES = {
    'default': {
        'ENGINE': 'django.db.backends.postgresql',
        'NAME': os.environ.get('PILOTWAVE_DB_NAME', 'pilotwave'),
        'USER': os.environ.get('PILOTWAVE_DB_USER', 'pilotwave'),
        'PASSWORD': os.environ.get('PILOTWAVE_DB_PASSWORD', 'pilotwave'),
        'HOST': os.environ.get('PILOTWAVE_DB_HOST', 'localhost'),
        'PORT': os.environ.get('PILOTWAVE_DB_PORT', '5432'),
    }
}

TASKS = {
    'default': {
        'BACKEND': 'django_tasks_db.DatabaseBackend',
        'QUEUES': ['default', 'pilotwave.email'],
    },
}

PILOTWAVE_QUEUE_NAME = 'pilotwave.email'
PILOTWAVE_ATTACHMENT_ROOTS = [
    os.environ.get('PILOTWAVE_ATTACHMENT_ROOTS', str(Path.cwd() / 'media'))
]
MEDIA_ROOT = PILOTWAVE_ATTACHMENT_ROOTS[0]
DEFAULT_AUTO_FIELD = 'django.db.models.BigAutoField'
EMAIL_BACKEND = 'django.core.mail.backends.console.EmailBackend'

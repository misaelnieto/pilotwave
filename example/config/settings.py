"""Settings for the Pilotwave example project.

Self-contained by design: SQLite lives next to this file, so `migrate`
and `runserver` work with zero external services. The queue table is the
same `django_tasks_database_dbtaskresult` owned by `django-tasks-db`
(see `contracts/CONTRACT.md`); only the DSN changes.

To drain the queue against a real Postgres, point a host project at its
own `DATABASES`/`TASKS` settings instead — this file is a demo, not a
deployment template.
"""

from pathlib import Path

BASE_DIR = Path(__file__).resolve().parent.parent

SECRET_KEY = 'pilotwave-example-only-not-for-production'
DEBUG = True
ALLOWED_HOSTS = ['127.0.0.1', 'localhost']

USE_TZ = True
TIME_ZONE = 'UTC'

INSTALLED_APPS = [
    'django.contrib.contenttypes',
    'django.contrib.auth',
    'django_tasks_db',
    'pilotwave',
    'demo',
]

DATABASES = {
    'default': {
        'ENGINE': 'django.db.backends.sqlite3',
        'NAME': BASE_DIR / 'db.sqlite3',
    }
}

TASKS = {
    'default': {
        'BACKEND': 'django_tasks_db.DatabaseBackend',
        'QUEUES': ['default', 'pilotwave.email'],
    },
}

ROOT_URLCONF = 'config.urls'

TEMPLATES = [
    {
        'BACKEND': 'django.template.backends.django.DjangoTemplates',
        'DIRS': [BASE_DIR / 'demo' / 'templates'],
        'APP_DIRS': True,
        'OPTIONS': {},
    },
]

# Pilotwave producer settings. The queue name must match the worker's
# `PILOTWAVE_QUEUE_NAME` when draining against Postgres.
PILOTWAVE_QUEUE_NAME = 'pilotwave.email'

DEFAULT_AUTO_FIELD = 'django.db.models.BigAutoField'

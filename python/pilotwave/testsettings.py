"""Minimal settings for the package's own test suite.

Production/database configuration is supplied by the host project. These
settings exist only so the contract round-trip tests can run with ``pytest``
without any external services.
"""

SECRET_KEY = 'pilotwave-tests-only'
DEBUG = False
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
        'ENGINE': 'django.db.backends.sqlite3',
        'NAME': ':memory:',
    }
}

TASKS = {
    'default': {
        'BACKEND': 'django_tasks_db.DatabaseBackend',
        'QUEUES': ['default', 'pilotwave.email'],
    },
}

PILOTWAVE_QUEUE_NAME = 'pilotwave.email'
DEFAULT_AUTO_FIELD = 'django.db.models.BigAutoField'
EMAIL_BACKEND = 'django.core.mail.backends.locmem.EmailBackend'

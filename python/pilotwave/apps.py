from django.apps import AppConfig

from pilotwave import __version__


class PilotwaveConfig(AppConfig):
    default_auto_field = 'django.db.models.BigAutoField'
    name = 'pilotwave'
    label = 'pilotwave'
    verbose_name = 'Pilotwave'
    version = __version__

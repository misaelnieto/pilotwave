"""The only Django model Pilotwave owns.

The Rust worker writes this table as an explicit, test-covered exception to the
"no Django tables from Rust" rule (see contracts/CONTRACT.md, decision P-2).
"""

from django.db import models
from django.utils.translation import gettext_lazy as _


class PilotwaveHeartbeat(models.Model):
    key = models.CharField(_('key'), max_length=100, primary_key=True)
    last_seen = models.DateTimeField(_('last seen'), auto_now=True)
    metadata = models.JSONField(_('metadata'), default=dict, blank=True)

    class Meta:
        db_table = 'pilotwave_heartbeat'
        verbose_name = _('Pilotwave heartbeat')
        verbose_name_plural = _('Pilotwave heartbeats')

    def __str__(self) -> str:
        return f'{self.key} (last seen: {self.last_seen})'

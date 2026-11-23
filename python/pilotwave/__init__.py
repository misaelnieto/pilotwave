"""Pilotwave: Django producer layer + Rust worker for email delivery.

Importing this package must not require Django settings to be configured or
apps to be loaded — Django imports the package while building the app registry.
The public API in :mod:`pilotwave.tasks` is imported explicitly by callers,
or lazily through :func:`__getattr__`.
"""

from typing import Any

__all__ = ['mail_notification']
__version__ = '0.1.0'


def __getattr__(name: str) -> Any:
    if name in __all__:
        from pilotwave import tasks

        return getattr(tasks, name)
    raise AttributeError(f'module {__name__!r} has no attribute {name!r}')

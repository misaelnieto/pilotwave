"""Pilotwave: Django producer layer + Rust worker for email delivery.

Importing this package must not require Django settings to be configured or
apps to be loaded — Django imports the package while building the app registry.
The public API in :mod:`pilotwave.tasks` is imported explicitly by callers,
or lazily through :func:`__getattr__`.
"""

from importlib.metadata import PackageNotFoundError, version
from typing import Any

__all__ = ['mail_notification']


def _detect_version() -> str:
    """Return the installed distribution version.

    ``pyproject.toml`` is the single source of truth; semantic-release bumps
    it (together with ``rust-worker/Cargo.toml``) on every release, so the
    version is read from the installed dist metadata instead of being
    hardcoded here (which would drift, as it did at ``0.1.0``).
    """
    try:
        return version('pilotwave')
    except PackageNotFoundError:  # pragma: no cover - source tree, not installed
        return '0.0.0+unknown'


__version__ = _detect_version()


def __getattr__(name: str) -> Any:
    if name in __all__:
        from pilotwave import tasks

        return getattr(tasks, name)
    raise AttributeError(f'module {__name__!r} has no attribute {name!r}')

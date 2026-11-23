"""Package hygiene: Pilotwave must never import a host project app.

The producer layer depends only on Django, ``django-tasks-db`` and itself;
anything else (host apps, ad-hoc third-party clients) would couple the
standalone package back to the project it was extracted from.
"""

import ast
import os
import subprocess
import sys
from pathlib import Path

PACKAGE_DIR = Path(__file__).resolve().parents[1] / 'python' / 'pilotwave'

ALLOWED_THIRD_PARTY = {'django', 'django_tasks_db'}


def _top_level_imports(path: Path) -> set[str]:
    tree = ast.parse(path.read_text(), filename=str(path))
    names = set()
    for node in ast.walk(tree):
        if isinstance(node, ast.Import):
            names.update(alias.name.split('.')[0] for alias in node.names)
        elif isinstance(node, ast.ImportFrom):
            if node.level:
                names.add('pilotwave')  # relative import: our own package
            elif node.module:
                names.add(node.module.split('.')[0])
    return names


def test_no_host_or_third_party_imports():
    stdlib = set(sys.stdlib_module_names)
    offenders = []
    for path in sorted(PACKAGE_DIR.rglob('*.py')):
        for name in sorted(_top_level_imports(path)):
            if name in stdlib or name in ALLOWED_THIRD_PARTY or name == 'pilotwave':
                continue
            offenders.append(f'{path.relative_to(PACKAGE_DIR)}: {name}')
    assert offenders == []


def test_import_without_django_settings():
    """`import pilotwave` must not require configured settings or apps."""
    from pilotwave import __version__

    env = {key: value for key, value in os.environ.items() if key != 'DJANGO_SETTINGS_MODULE'}
    proc = subprocess.run(
        [sys.executable, '-c', 'import pilotwave; print(pilotwave.__version__)'],
        capture_output=True,
        text=True,
        cwd=PACKAGE_DIR.parent,
        env={**env, 'PYTHONPATH': str(PACKAGE_DIR.parent)},
        timeout=60,
    )
    assert proc.returncode == 0, proc.stderr
    assert proc.stdout.strip() == __version__

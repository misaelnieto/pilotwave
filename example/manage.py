#!/usr/bin/env python
"""Development entry point for the Pilotwave example project."""

import os
import sys
from pathlib import Path

# The example is self-contained: reuse the library straight from the
# checkout (`python/`) without installing anything, plus the example
# project itself (`example/`) for `config` and `demo`.
BASE_DIR = Path(__file__).resolve().parent
REPO_ROOT = BASE_DIR.parent
sys.path.insert(0, str(REPO_ROOT / 'python'))
sys.path.insert(0, str(BASE_DIR))


def main() -> None:
    os.environ.setdefault('DJANGO_SETTINGS_MODULE', 'config.settings')
    from django.core.management import execute_from_command_line

    execute_from_command_line(sys.argv)


if __name__ == '__main__':
    main()

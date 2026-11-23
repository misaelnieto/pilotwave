"""Sample VmRSS + Pss of a PID from /proc into a CSV file.

Usage: ``.venv/bin/python benchmark/measure.py --pid 1234 --out results/a.csv
--duration 90``. No extra deps. Stops early if the PID exits.
Outputs CSV: ``timestamp,rss_kb,pss_kb``.
"""

from __future__ import annotations

import argparse
import csv
import time


def read_status(pid: int) -> int | None:
    try:
        with open(f'/proc/{pid}/status') as fh:
            for line in fh:
                if line.startswith('VmRSS:'):
                    return int(line.split()[1])
    except OSError, ValueError, IndexError:
        return None
    return None


def read_pss(pid: int) -> int | None:
    try:
        with open(f'/proc/{pid}/smaps_rollup') as fh:
            for line in fh:
                if line.startswith('Pss:'):
                    return int(line.split()[1])
    except OSError, ValueError, IndexError:
        return None
    return None


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument('--pid', type=int, required=True)
    parser.add_argument('--out', required=True)
    parser.add_argument('--interval', type=float, default=1.0)
    parser.add_argument('--duration', type=float, default=120.0)
    args = parser.parse_args()

    deadline = time.time() + args.duration
    with open(args.out, 'w', newline='') as fh:
        writer = csv.writer(fh)
        writer.writerow(['timestamp', 'rss_kb', 'pss_kb'])
        while time.time() < deadline:
            rss = read_status(args.pid)
            if rss is None:
                break  # PID exited
            pss = read_pss(args.pid)
            writer.writerow([f'{time.time():.3f}', rss, pss if pss is not None else ''])
            fh.flush()
            time.sleep(args.interval)
    print(f'wrote {args.out}')


if __name__ == '__main__':
    main()

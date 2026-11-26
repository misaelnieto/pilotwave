#!/usr/bin/env bash
# Memory benchmark: Rust pilotwave-worker vs minimal Python baseline.
# Both workers drain the same 100 READY rows via SMTP to the accept-all
# fake SMTP (benchmark/fake_smtp.py on 1026), each measured idle
# (empty queue 60 s) + draining 100 tasks.
#
# Usage: ./benchmark/run_benchmark.sh   (or: just benchmark)
# Knobs: IDLE_SECS=60 TASKS_CSV=benchmark/tasks_100.csv
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

RESULTS="$ROOT/benchmark/results"
IDLE_SECS="${IDLE_SECS:-60}"
TASKS_CSV="${TASKS_CSV:-benchmark/tasks_100.csv}"
MAX_DRAIN_SECS=600

# ── Benchmark wiring (dev-local, fake SMTP, superuser DSN) ────────────────
# Same env for BOTH workers so the comparison is apples-to-apples.
# Uses the dev superuser DSN (grant check passes); NOT the least-privilege
# PILOTWAVE_DATABASE_URL from .env (that role may not exist locally).
export DATABASE_URL="${BENCH_DATABASE_URL:-postgres://pilotwave:pilotwave@localhost:5432/pilotwave}"
export PILOTWAVE_DATABASE_URL="$DATABASE_URL"
export PILOTWAVE_QUEUE_NAME=pilotwave.email
export PILOTWAVE_BACKEND_NAME=default
export PILOTWAVE_TASK_PATHS=pilotwave.tasks.mail_notification
export PILOTWAVE_ID=pilotwave-bench
export PILOTWAVE_POLL_MIN_MS=100
export PILOTWAVE_POLL_MAX_MS=200
export EMAIL_HOST=localhost
# Port 1026/fake SMTP (not 1025/mailcrab): mailcrab advertises no AUTH, so it
# would need PILOTWAVE_ALLOW_ANONYMOUS_SMTP=true (ADR 003, loopback-only dev
# path). The benchmark keeps benchmark/fake_smtp.py — it accepts any AUTH
# (and no-auth senders, for the Python baseline) and archives deliveries,
# so the Rust side keeps exercising the AUTH path.
export EMAIL_PORT=1026
export EMAIL_USE_TLS=false
export PILOTWAVE_ALLOW_PLAINTEXT_SMTP=true
export EMAIL_HOST_USER=benchmark
export EMAIL_HOST_PASSWORD=benchmark
export DEFAULT_FROM_EMAIL='Pilotwave Benchmark <benchmark@example.com>'

mkdir -p "$RESULTS"

echo '== [1/6] docker compose: db + mail =='
docker compose -f compose.yaml up -d db mail
until docker compose -f compose.yaml exec -T db pg_isready -U pilotwave >/dev/null 2>&1; do sleep 1; done

echo '== [1b/6] fake SMTP (accept-all AUTH on 1026) =='
mkdir -p "$RESULTS/mail"
pkill -f 'benchmark/fake_smtp.py' 2>/dev/null || true
sleep 0.5
.venv/bin/python benchmark/fake_smtp.py --port 1026 --mailbox "$RESULTS/mail" > "$RESULTS/fake-smtp.log" 2>&1 &
FAKE_SMTP_PID=$!
cleanup() { kill "$FAKE_SMTP_PID" 2>/dev/null || true; }
trap cleanup EXIT
for _ in $(seq 1 50); do
  if .venv/bin/python -c "import socket; socket.create_connection(('127.0.0.1', 1026), 2).close()" 2>/dev/null; then
    break
  fi
  sleep 0.2
done
echo "fake-smtp pid=$FAKE_SMTP_PID"

echo '== [2/6] schema (benchmark-only DDL) =='
docker compose -f compose.yaml exec -T db psql -U pilotwave -d pilotwave -v ON_ERROR_STOP=1 < "$ROOT/benchmark/schema.sql"

echo '== [3/6] build release binary =='
cargo build --release --manifest-path rust-worker/Cargo.toml
RUST_BIN="$ROOT/rust-worker/target/release/pilotwave-worker"

TIME_BIN=""
if command -v /usr/bin/time >/dev/null 2>&1; then TIME_BIN="/usr/bin/time"; fi

queue_left() {
  DATABASE_URL="$DATABASE_URL" .venv/bin/python -c "
import os, psycopg
with psycopg.connect(os.environ['DATABASE_URL']) as c:
    with c.cursor() as cur:
        cur.execute(\"SELECT count(*) FROM django_tasks_database_dbtaskresult WHERE status IN ('READY','RUNNING') AND queue_name='pilotwave.email' AND backend_name='default'\")
        print(cur.fetchone()[0])
"
}

wait_drained() {
  local waited=0
  while [ "$(queue_left)" -gt 0 ]; do
    if [ "$waited" -ge "$MAX_DRAIN_SECS" ]; then
      echo "ERROR: queue did not drain within ${MAX_DRAIN_SECS}s" >&2
      queue_left >&2
      return 1
    fi
    sleep 2
    waited=$((waited + 2))
  done
  echo "queue drained after ~${waited}s; measuring idle ${IDLE_SECS}s..."
  sleep "$IDLE_SECS"
}

run_phase() { # $1=phase name $2...=worker command
  local phase="$1"
  shift
  echo "== phase $phase: loading queue =="
  DATABASE_URL="$DATABASE_URL" .venv/bin/python benchmark/load_csv.py --csv "$TASKS_CSV"
  echo "== phase $phase: starting $* =="
  local worker_pid measure_pid wrapper=""
  if [ -n "$TIME_BIN" ]; then
    # /usr/bin/time must wrap the worker to record peak RSS, but $! is then
    # time's PID — resolve the REAL worker as time's direct child (never by
    # cmdline pattern: that self-matches the wrapper and measures pid 1.5MB
    # instead of the worker).
    "$TIME_BIN" -v -o "$RESULTS/$phase.time" "$@" &
    wrapper=$!
    for _ in $(seq 1 50); do
      worker_pid="$(pgrep -P "$wrapper" | head -n1 || true)"
      [ -n "$worker_pid" ] && break
      sleep 0.2
    done
    if [ -z "$worker_pid" ]; then
      echo "ERROR: phase $phase worker never appeared under time wrapper" >&2
      return 1
    fi
  else
    "$@" &
    worker_pid=$!
    sleep 2
  fi
  if ! kill -0 "$worker_pid" 2>/dev/null; then
    echo "ERROR: phase $phase worker failed to start (pid $worker_pid)" >&2
    return 1
  fi
  echo "phase $phase pid=$worker_pid"
  .venv/bin/python benchmark/measure.py --pid "$worker_pid" \
    --out "$RESULTS/$phase.csv" --duration 900 &
  measure_pid=$!
  wait_drained
  stop_pid "$worker_pid"
  if [ -n "$wrapper" ]; then
    wait "$wrapper" 2>/dev/null || true
    grep -E 'Maximum resident|Elapsed' "$RESULTS/$phase.time" || true
  fi
  kill "$measure_pid" 2>/dev/null || true
  wait "$measure_pid" 2>/dev/null || true
  echo "== phase $phase done =="
}

stop_pid() { # TERM, wait up to 10s, then KILL (workers ignore nothing, but be sure)
  local pid="$1" i
  kill -TERM "$pid" 2>/dev/null || true
  for i in $(seq 1 100); do
    kill -0 "$pid" 2>/dev/null || return 0
    sleep 0.1
  done
  kill -KILL "$pid" 2>/dev/null || true
  return 0
}

run_phase python \
  .venv/bin/python benchmark/reference_worker.py --poll-ms 100
run_phase rust \
  "$RUST_BIN" run --fg

echo '== [6/6] summary =='
.venv/bin/python - "$RESULTS" <<'EOF'
import csv, sys

results = sys.argv[1]


def stats(path):
    rss, pss = [], []
    with open(path) as fh:
        for row in csv.DictReader(fh):
            rss.append(int(row['rss_kb']))
            if row['pss_kb']:
                pss.append(int(row['pss_kb']))
    idle = rss[-60:] if len(rss) >= 60 else rss
    return {
        'samples': len(rss),
        'rss_max': max(rss), 'rss_mean': sum(rss) // len(rss),
        'rss_idle_max': max(idle), 'rss_idle_mean': sum(idle) // len(idle),
        'pss_max': max(pss) if pss else None,
        'pss_mean': (sum(pss) // len(pss)) if pss else None,
    }


lines = ['phase,samples,rss_max_kb,rss_mean_kb,rss_idle_max_kb,rss_idle_mean_kb,pss_max_kb,pss_mean_kb']
summary = {}
for phase in ('python', 'rust'):
    s = stats(f'{results}/{phase}.csv')
    summary[phase] = s
    lines.append(
        f"{phase},{s['samples']},{s['rss_max']},{s['rss_mean']},"
        f"{s['rss_idle_max']},{s['rss_idle_mean']},{s['pss_max']},{s['pss_mean']}"
    )
py, rs = summary['python'], summary['rust']
lines.append('')
lines.append(f"rss_max: python={py['rss_max']}kB rust={rs['rss_max']}kB ratio={py['rss_max'] / max(rs['rss_max'], 1):.1f}x")
lines.append(
    f"rss_idle_mean: python={py['rss_idle_mean']}kB rust={rs['rss_idle_mean']}kB"
    f" ratio={py['rss_idle_mean'] / max(rs['rss_idle_mean'], 1):.1f}x"
)
open(f'{results}/summary.txt', 'w').write('\n'.join(lines) + '\n')
print('\n'.join(lines))
EOF

echo "results in $RESULTS (python.csv rust.csv summary.txt)"

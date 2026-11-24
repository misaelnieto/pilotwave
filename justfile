set dotenv-load
# The single developer/CI entry point. Nobody should need to remember uv,
# cargo, maturin or docker compose commands directly.

[private]
default:
    @just --list

# ── Environment ──────────────────────────────────────────────────────────

# Report required host tools and a usable .env.
[group("Development")]
pre-install-check:
    #!/usr/bin/env bash
    set -u
    missing=0
    report() {
        if command -v "$1" >/dev/null 2>&1; then
            echo "OK      $1 $("$1" --version 2>/dev/null | head -n1)"
        else
            echo "MISSING $1"
            missing=1
        fi
    }
    report just
    report uv
    report cargo
    report rustc
    report docker
    if docker compose version >/dev/null 2>&1; then
        echo "OK      docker compose $(docker compose version --short 2>/dev/null)"
    else
        echo "MISSING docker compose (plugin)"
        missing=1
    fi
    if rustup target list --installed 2>/dev/null | grep -qx x86_64-unknown-linux-musl; then
        echo "OK      rust target x86_64-unknown-linux-musl"
    else
        echo "MISSING rust target: run 'rustup target add x86_64-unknown-linux-musl'"
        missing=1
    fi
    if [[ ! -f .env ]]; then
        echo "MISSING .env — copy .env.example to .env and adjust it"
        missing=1
    else
        echo "OK      .env present"
    fi
    exit "$missing"

# Install Python + Rust dependencies (CI uses the same recipe).
[group("Development")]
sync:
    uv sync --all-groups
    cargo fetch --manifest-path rust-worker/Cargo.toml

# ── Local development ────────────────────────────────────────────────────

# Start the development Postgres (dev only; not a deployment mechanism).
[group("Development")]
dev-up:
    docker compose -f compose.yaml up -d db
    until docker compose -f compose.yaml exec -T db pg_isready -U {{ env('LOCAL_DB_USER', 'pilotwave') }} >/dev/null 2>&1; do sleep 1; done

# Stop the development Postgres.
[group("Development")]
dev-down:
    docker compose -f compose.yaml down

# Apply Pilotwave's Django migrations.
[group("Development")]
migrate:
    uv run python -m django migrate --settings=pilotwave.devsettings

# Drain the queue with the real binary (oneshot by default). Pass-through
# of `pilotwave-worker run` flags (see `cargo run -q -p pilotwave-worker -- run --help`):
#   just run-worker                  # claim + send one READY task, then exit
#   just run-worker -- --id <UUID>   # claim one specific task
#   just run-worker -- --fg          # persistent foreground loop (SIGTERM/SIGINT)
#   just run-worker -- --bg          # detached daemon (writes PILOTWAVE_PID_FILE)
[group("Development")]
run-worker *args:
    cargo run --manifest-path rust-worker/Cargo.toml -- run {{ args }}

# Show queue counts + daemon + heartbeat (read-only except the heartbeat read).
[group("Development")]
worker-status:
    cargo run --manifest-path rust-worker/Cargo.toml -- status

# Stop the `--bg` daemon (SIGTERM, then SIGKILL; idempotent; touches no DB).
[group("Development")]
stop:
    cargo run --manifest-path rust-worker/Cargo.toml -- stop

# Tail the daemon log file (PILOTWAVE_LOG_FILE; touches no DB):
#   just log               # last 10 lines
#   just log -- -n 50 -f   # last 50 lines, then follow appends
[group("Development")]
log *args:
    cargo run --manifest-path rust-worker/Cargo.toml -- log {{ args }}

# List tasks by state (limit 1-200, offset >= 0):
#   just list -- ready [--limit 25] [--offset 0]
# State is one of: ready | running | done | failed.
[group("Development")]
list *args:
    cargo run --manifest-path rust-worker/Cargo.toml -- list {{ args }}

# Show every column of one queue row by UUID (read-only).
[group("Development")]
inspect id:
    cargo run --manifest-path rust-worker/Cargo.toml -- inspect {{ id }}

# ── Quality ──────────────────────────────────────────────────────────────

# Lint and format both languages.
[group("Quality")]
lint:
    uv run ruff check python tests
    uv run ruff format --check python tests
    cargo fmt --manifest-path rust-worker/Cargo.toml -- --check
    cargo clippy --manifest-path rust-worker/Cargo.toml --all-targets -- -D warnings

# Autofix Python; format Rust.
[group("Quality")]
format:
    uv run ruff check --fix python tests
    uv run ruff format python tests
    cargo fmt --manifest-path rust-worker/Cargo.toml

# Run Python and Rust tests (identical locally and in CI).
[group("Quality")]
test:
    uv run pytest
    cargo test --manifest-path rust-worker/Cargo.toml

# Audit dependencies (see docs/06-AUDITS/001-owasp-top10-2026-11-23.md):
# Rust (cargo-audit/cargo-deny), Python (pip-audit via uv) and the
# postgres:18 image (trivy). Missing scanners are reported as SKIP;
# present scanners fail on high-severity findings
# (trivy: advisory, exit 0 by default).
[group("Quality")]
audit:
    echo "== cargo-audit =="
    cd rust-worker && cargo-audit audit
    echo "== uv audit (best-effort) =="
    uv audit

# ── Benchmark ────────────────────────────────────────────────────────────

# Memory benchmark: release Rust worker vs minimal Python baseline (idle +
# drain of 100 tasks; writes benchmark/results/). Uses .venv/bin/python
# directly for the baseline (no `uv run` overhead) and the release binary
# (never `cargo run` debug).
[group("Benchmark")]
benchmark:
    ./benchmark/run_benchmark.sh

# ── Build ────────────────────────────────────────────────────────────────

# Build the wheel with the musl Rust binary included.
[group("Build")]
build:
    uv run maturin build --release --manifest-path rust-worker/Cargo.toml --target x86_64-unknown-linux-musl --out dist

# Install the built wheel into the current environment (smoke check).
# `--help` is intentionally DB-free: it only proves the musl binary installs
# and runs. (`status`/`run` need PILOTWAVE_DATABASE_URL + a live Postgres.)
[group("Build")]
build-install:
    just build
    uv pip install --reinstall dist/*.whl
    pilotwave-worker --help

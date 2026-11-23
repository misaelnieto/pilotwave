# CI and Build

`just test` and `just build` are the same commands locally and in CI.

## Build

`just build` runs:

```bash
uv run maturin build --release --manifest-path rust-worker/Cargo.toml \
  --target x86_64-unknown-linux-musl --out dist
```

maturin uses `bindings = "bin"`, so the wheel contains `pilotwave-worker`
installed into the virtualenv's `bin/`. `uv add pilotwave` therefore installs
both the Django app and the executable.

> The `--target` flag is explicit because maturin invokes cargo from the repo
> root, where `rust-worker/.cargo/config.toml` is not auto-discovered. Without
> it, the wheel would silently contain a glibc-linked binary.

## Target

The Rust target is hardcoded in `rust-worker/.cargo/config.toml`:

```toml
[build]
target = "x86_64-unknown-linux-musl"
```

Only Linux x86_64 wheels are produced. Musl gives maximum compatibility with
the glibc variants found on shared hosts.

## CI

`.github/workflows/ci.yml` installs the same tools, then runs the same
recipes in the same order as local development:

```bash
just sync
just test
just lint
just build
```

No service containers are needed: the tests are DB-free (SQLite for Python,
`include_str!` fixtures + unit tests for Rust).

## Adding functionality

Keep the contract tests as the gate: any change to queue columns, status
literals, payload keys or failure codes must fail both test suites until
both sides are updated.

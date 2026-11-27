# CHANGELOG

<!-- version list -->

## v0.4.0 (2026-11-27)

### Bug Fixes

- Avoid i64 wraparound in is-alive max-age comparison
  ([`e25558c`](https://github.com/misaelnieto/pilotwave/commit/e25558c3814ba46caca67f98e4c39eb47509b46f))

- Bound is-alive heartbeat read with timeout (error, exit 2 on stall)
  ([`1d50b72`](https://github.com/misaelnieto/pilotwave/commit/1d50b724fb5b3258726fb37683aae4dfbfb03f46))

- Enforce TLS on reader DSN before connect
  ([`ade7869`](https://github.com/misaelnieto/pilotwave/commit/ade786969f48e904d1e63eaf45cd5e1ddd49b50e))

- Grant pilotwave_worker SELECT on heartbeat for is-alive fallback
  ([`190f167`](https://github.com/misaelnieto/pilotwave/commit/190f167235f39b1cd02bc9899fe1922dbfcb35c0))

- Is-alive verifies local daemon before trusting shared heartbeat
  ([`9d1b6e2`](https://github.com/misaelnieto/pilotwave/commit/9d1b6e2f2cffe21d6cae2bc8af1afde8f3c3d38f))

- Serialize concurrent run --bg starts, hold cron lock across probe+restart
  ([`78499a9`](https://github.com/misaelnieto/pilotwave/commit/78499a909a005f16874a30697c34ac5873ffb394))

- **is-alive**: Emit machine-readable error for malformed --max-age-secs
  ([`a525e34`](https://github.com/misaelnieto/pilotwave/commit/a525e34eb4baf2f4b732b8cbdfc756ade9f2fd22))

- **is-alive**: Probe on reader DSN only, without worker/SMTP config
  ([`e9782b0`](https://github.com/misaelnieto/pilotwave/commit/e9782b02ba5e693d51fba3826002dc8abeea4703))

### Documentation

- Export sourced worker.env in cron keep-alive one-liner
  ([`25adbdc`](https://github.com/misaelnieto/pilotwave/commit/25adbdc2370c1689ad975e6aaccaf4df1d469d5e))

- **config**: Document PILOTWAVE_SKIP_GRANT_CHECK default and prod warning
  ([`748a9a6`](https://github.com/misaelnieto/pilotwave/commit/748a9a6d03b8affd0727de8479820abd550f3555))

### Features

- Is-alive liveness subcommand for cron keep-alive
  ([`f35ac63`](https://github.com/misaelnieto/pilotwave/commit/f35ac6359edd1e12364bcc57ab8c863be5984ece))

- User guide + English CLI (is-alive docs reorg)
  ([`067e2e3`](https://github.com/misaelnieto/pilotwave/commit/067e2e36376604747bdf1bd77a8d29200abd0537))


## Unreleased

### Features

- `is-alive` liveness subcommand for cron keep-alive (ADR-004): one
  machine-readable stdout line (`fresh|stale|down|error`), exits 0/1/2,
  `--max-age-secs` (default 90), read-only via the reader DSN; docs in
  `docs/05-DEVELOPERS/getting-started.md` and `README.md`.
- New black-box user guide in `docs/07-USER-GUIDE/` (quickstart,
  configuration reference, deployment) for wheel users; no Rust or
  `just` required.

### Changed

- Worker CLI messages are now all in English (`status` table headers
  and labels, daemon/stop/inspect/log messages). Parse English output
  from this version on.

### Documentation

- Slim root `README.md` to pitch + why + how-it-looks + pointers;
  full benchmark table moved to `benchmark/README.md`.
- Reorganized docs map (`docs/README.md` now indexes `06-AUDITS` and
  `07-USER-GUIDE`); trimmed `05-DEVELOPERS` operator topics to links
  into the user guide.

## v0.3.0 (2026-11-26)

### Features

- Optional anonymous SMTP for Mailcrab dev via PILOTWAVE_ALLOW_ANONYMOUS_SMTP
  ([`49e76d1`](https://github.com/misaelnieto/pilotwave/commit/49e76d14743280201e870aa4e2994bfd10bd22d9))


## v0.2.3 (2026-11-26)

### Bug Fixes

- Include python module in wheel + dynamic __version__
  ([`fd62498`](https://github.com/misaelnieto/pilotwave/commit/fd6249848488e536d6a570ce0453383bd67367af))


## v0.2.2 (2026-11-25)

### Bug Fixes

- Publish-gh must checkout main, tag leaves detached HEAD with no release group
  ([`a2f0a1c`](https://github.com/misaelnieto/pilotwave/commit/a2f0a1c8581491ae825cf0f8103b313cd5d63b9c))


## v0.2.1 (2026-11-25)

### Bug Fixes

- Harden build-release workflow (tag build, split GH/PyPI publishes, OIDC-only)
  ([`1115773`](https://github.com/misaelnieto/pilotwave/commit/1115773e3feabc1e59d7349aa365b448fcda40e3))


## v0.2.0 (2026-11-25)

### Bug Fixes

- Ci pin setup-uv to existing version
  ([`750e5e0`](https://github.com/misaelnieto/pilotwave/commit/750e5e000ce546b46d138c6e36caddf5ae4d6e73))

- Remove redundant homepage from Cargo.toml to unbreak CI lint
  ([`8b75c02`](https://github.com/misaelnieto/pilotwave/commit/8b75c02d703317a3e4666401f7f507cadf30f69a))


## v0.1.0 (2026-11-24)

- Initial Release

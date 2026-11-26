# CHANGELOG

<!-- version list -->

## Unreleased

### Features

- `is-alive` liveness subcommand for cron keep-alive (ADR-004): one
  machine-readable stdout line (`fresh|stale|down|error`), exits 0/1/2,
  `--max-age-secs` (default 90), read-only via the reader DSN; docs in
  `docs/05-DEVELOPERS/getting-started.md` and `README.md`.

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

---
title: Opt-in anonymous SMTP for dev (Mailcrab)
type: decision
status: accepted
date: 2026-11-26
---

# Decision 003: Opt-in anonymous SMTP for dev (Mailcrab)

## Context

`mailer.rs` applied `.credentials()` unconditionally, so the worker always
attempted SMTP AUTH. Mailcrab (dev, `compose.yaml` `mail` service, `:1025`)
advertises no AUTH extension: every dev send failed transient and the queue
never drained. The benchmark worked around it with a custom accept-all fake
SMTP on port 1026 (`benchmark/fake_smtp.py`).

## Decision

Option A (approved): `EMAIL_HOST_USER`/`EMAIL_HOST_PASSWORD` become optional
(trimmed, empty = absent), gated by a new explicit flag
`PILOTWAVE_ALLOW_ANONYMOUS_SMTP` (default `false`). No AUTH is accepted only
when **all** hold:

1. host is loopback (`localhost` / `127.*` / `::1`, `smtp_host_is_loopback`),
2. `EMAIL_USE_TLS=false` **with** the existing `PILOTWAVE_ALLOW_PLAINTEXT_SMTP=true`,
3. `PILOTWAVE_ALLOW_ANONYMOUS_SMTP=true`.

Exactly one of user/password set is a fatal startup error. Any no-AUTH case
outside loopback, with TLS on, or missing either opt-in is a fatal startup
error. Startup without AUTH logs a `WARNING ... sin AUTH` on stderr.
`Debug` shows the user redacted and `(sin-auth)` when absent; the password is
never shown. `mailer.rs::transport()` applies `.credentials()` only when both
are present.

## Alternatives discarded

- **B — AUTH fallback (try AUTH, continue anonymous on failure):** silent
  downgrade; a MITM stripping the AUTH extension would silently remove
  authentication. Rejected.
- **C — infer from absence (no flag, empty creds = anonymous):** an empty
  `.env` line or a dropped variable would silently disable AUTH. Rejected:
  disabling AUTH must be an explicit, greppable opt-in.
- **Credential inference (guess from port/host):** magic per-host behaviour,
  untestable matrix. Rejected.

## Typo risk and mitigation

A mistyped host (e.g. `localhosst`) could in theory intend loopback while
resolving elsewhere. Mitigated fail-closed: the loopback check is a strict
allowlist (`localhost`, numeric `127./8`, `::1`, brackets stripped, DNS
suffixes rejected — `127.0.0.1.evil.com` does not match), and **any**
non-matching host with absent credentials is a fatal startup error, never a
silent anonymous send. `Debug` never leaks the password, so diagnosing the
typo from logs is safe.

## Consequences

- Dev can point the worker at Mailcrab `:1025` with empty credentials + both
  opt-ins; the benchmark keeps its fake SMTP (AUTH path still covered there).
- Production is unaffected: defaults keep AUTH mandatory; the new flag is
  `false` unless explicitly set, and loopback + no-TLS can never occur in prod
  without two explicit dev-only flags.
- Egress note: without AUTH there is no sender accountability — restricted to
  loopback so unauthenticated egress cannot leave the machine
  (see `SECURITY.md` and `docs/06-AUDITS/` note).

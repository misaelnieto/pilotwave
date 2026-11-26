# Audit note — anonymous SMTP egress (2026-11-26, ADR 003)

Addendum to [audit 001](001-owasp-top10-2026-11-23.md) (A02/A05): the worker
now accepts SMTP without AUTH under `PILOTWAVE_ALLOW_ANONYMOUS_SMTP=true`.

Egress implication: without AUTH there is no sender accountability at the
SMTP hop — anyone able to reach the relay can send through it. Containment
is structural, not procedural: the no-AUTH path is accepted **only** against
a loopback host (`localhost`/`127.*`/`::1`, strict allowlist — DNS suffixes
such as `127.0.0.1.evil.com` do not match) **and** `EMAIL_USE_TLS=false`
with `PILOTWAVE_ALLOW_PLAINTEXT_SMTP=true` **and** the anonymous opt-in.
Any other combination is a fatal startup error, so unauthenticated egress
stays on loopback TCP and a typo'd host fails closed instead of sending
elsewhere (it never leaves the machine as long as no local relay forwards
it elsewhere — do not run a relay with forwarding on the loopback port,
and never enable this flag outside dev/loopback). Production (remote host and/or TLS) is unaffected: AUTH stays
mandatory there.

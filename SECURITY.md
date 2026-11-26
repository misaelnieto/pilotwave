# Security Policy

## Supported Versions

| Version | Supported          |
| ------- | ------------------ |
| latest PyPI release (`pilotwave`) | :white_check_mark: |
| older releases | :x: |

We release early and often; please upgrade to the latest release from
[PyPI](https://pypi.org/project/pilotwave/) or the latest
[GitHub Release](https://github.com/misaelnieto/pilotwave/releases) before
reporting.

## Reporting a Vulnerability

**Do not open a public issue for security vulnerabilities.**

- Preferred: [private security advisory](https://github.com/misaelnieto/pilotwave/security/advisories)
  (GitHub → Security → Advisories → New draft advisory).
- Alternative: email [pilotwave@noenieto.com](mailto:pilotwave@noenieto.com)
  with subject `[SECURITY] pilotwave: <short description>`.

Please include: affected version(s), steps to reproduce or proof of concept,
and impact assessment if known.

We aim to acknowledge reports within 72 hours and will coordinate a fix and
disclosure timeline with you. Please allow reasonable time for remediation
before any public disclosure.

## SMTP without AUTH (dev only)

`PILOTWAVE_ALLOW_ANONYMOUS_SMTP=true` disables SMTP authentication and is
accepted only against a loopback host (`localhost`/`127.*`/`::1`) with
`EMAIL_USE_TLS=false` + `PILOTWAVE_ALLOW_PLAINTEXT_SMTP=true` (dev/Mailcrab;
see ADR `docs/02-DECISIONS/003-anonymous-smtp-opt-in.md`). Never enable it in
production: without AUTH anyone able to reach the SMTP host can send mail
through it, and without TLS credentials and content travel in cleartext.
Egress without AUTH stays on loopback TCP by construction (loopback-only,
fail-closed on any other host): it never leaves the machine as long as no
local relay forwards it elsewhere. Do not run a relay with forwarding on the
loopback port, and never enable this flag outside dev/loopback.

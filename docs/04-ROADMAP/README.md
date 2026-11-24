# Roadmap — Future Features (Not Implemented)

Candidate features after v1. None of this is committed or scheduled; each
item needs its own plan + pending-decision entry before coding.

| # | Feature | What | Notes / relation to v1 |
|---|---------|------|------------------------|
| R-0 | Dedupe table keyed by `reference` | Exactly-once-ish send dedupe: record `reference` before send and skip repeats. | Rejected for v1 (P-4 decided): a second write in the hot path + a retention/GC burden, and it would not cover crash-after-send. Reopen only on a real duplicate incident. |
| R-1 | Multiple workers control | Coordination/lease for N workers on the same queue (identity, fencing, reaper ownership). | v1 assumes one logical worker (`worker_ids` append, reaper filters `pilotwave-*`, recycle exit 75). Needs design for split-brain + `SKIP LOCKED` fairness. |
| R-2 | Retry of rejected emails with callback to client code | Re-drive permanent rejections (`schema_error`, `header_violation`, `path_policy_violation`, `attachment_missing`, `smtp_permanent`) back to producer code via callback/hook. | v1 ends rejected rows as `FAILED` with `exception_class_path` + redacted `traceback`; no callback channel exists. Requires producer-side hook contract (dotted path like `PILOTWAVE_STATUS_PROVIDER`?) + at-least-once implications. |
| R-3 | Dripping engine as alternative channel | Time-spread sending (drip sequences) alongside immediate send. | Immediate send is the only channel today (`run_after` = due/deferred + sentinel `9999-01-01`). A drip engine is a scheduler on top of `run_after` + `reference` idempotency, not a second sender. |
| R-4 | Reactive pattern in Postgres (`LISTEN`/`NOTIFY`) | Wake the worker on enqueue instead of polling (`POLL_MIN`/`POLL_MAX`). | v1 polls (idle delay, reconnect backoff). `NOTIFY` needs a trigger on the backend-owned table (migration outside Pilotwave) or a side channel; keep polling as fallback. |
| R-5 | Conversions API worker (Meta/FB) | Second worker draining a separate queue for Meta Conversions API events. | Out of the email contract: new payload schema, new queue (`PILOTWAVE_TASK_PATHS` allowlist already supports it), new failure codes, no SMTP/attachments. Share claim/heartbeat/CLI machinery. |
| R-6 | Flood + planted-row hardening (security audit A04/A07) | Enforce least-privilege GRANTs at startup, make `depth_guard` non-bypassable by default, document `reference` idempotency for hosts. | Security audit top-1. See `docs/06-AUDITS/001-owasp-top10-2026-11-23.md`. |
| R-7 | PII redaction in CLI (security audit A07/A09) | Redact `to/subject/bodies` in `inspect` output by default (`--show-pii` opt-in), separate read-only DB role for CLI. | Security audit top-2. |
| R-8 | TLS enforcement + dependency audit (security audit A02/A06) | Require `sslmode` on Postgres outside dev, abort/warn on `EMAIL_USE_TLS=false` in prod, add `just audit` (`cargo audit`, `pip-audit`, `trivy`) to CI. | Security audit top-3. |

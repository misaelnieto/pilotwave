# Audits

Security audits against the executable contract in
[`contracts/`](../../contracts/). Each audit is point-in-time: it names
the commit/skeleton reviewed, the OWASP version used, verdicts per
category with file evidence, and follow-up debt (R-IDs) tracked in
[roadmap](../04-ROADMAP/README.md).

| ID | Date | Scope | Baseline |
|----|------|-------|----------|
| [001](001-owasp-top10-2026-11-23.md) | 2026-11-23 | Django producer + Rust worker, planted-row model | OWASP Top 10 2021 |
| [002](002-anonymous-smtp-egress-note.md) | 2026-11-26 | Addendum: anonymous-SMTP egress (ADR 003) | — |

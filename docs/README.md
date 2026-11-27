# Pilotwave Documentation

Spec-driven design for the standalone Pilotwave project. The documents here
describe intent and decisions; the executable contract is in
[`contracts/`](../contracts/).

## Sections

| Section                                       | Purpose                                            |
| --------------------------------------------- | -------------------------------------------------- |
| [01-ARCHITECTURE](01-ARCHITECTURE/)           | What Pilotwave is and how the pieces interact      |
| [02-DECISIONS](02-DECISIONS/)                 | Decisions, including the pending ones from the brief |
| [03-PLANS](03-PLANS/)                         | Extraction and implementation plans                |
| [04-ROADMAP](04-ROADMAP/)                       | Future features, explicitly not implemented        |
| [05-DEVELOPERS](05-DEVELOPERS/)               | Build, test, CI and operations                     |
| [06-AUDITS](06-AUDITS/)                       | Security audits and reviews                        |
| [07-USER-GUIDE](07-USER-GUIDE/)               | Black-box user docs (SSOT for installing,          |
|                                               | configuring and running Pilotwave)                 |

## See also

- [SECURITY.md](../SECURITY.md) — how to report vulnerabilities.
- [example/README.md](../example/README.md) — the runnable demo app.
- [contracts/CONTRACT.md](../contracts/CONTRACT.md) — the Python/Rust
  boundary contract.

## Conventions

- Each piece of information has one home (SSOT). The queue schema lives in
  `contracts/sql/`, not duplicated in an ADR.
- Language: English. Relative links. Prose wrapped at 80 columns
  (tables and code blocks exempt).
- If you must make a decision marked **pending**, write down the option and
  its justification in the relevant decision document before coding.

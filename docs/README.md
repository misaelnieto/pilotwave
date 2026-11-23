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

## Conventions

- Each piece of information has one home (SSOT). The queue schema lives in
  `contracts/sql/`, not duplicated in an ADR.
- Language: English. Relative links. Markdown wrapped at 80 columns.
- If you must make a decision marked **pending**, write down the option and
  its justification in the relevant decision document before coding.

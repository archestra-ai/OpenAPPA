# TASK-101 — Rotate production database credentials

- Status: OPEN
- Priority: High
- Assignee: Alice Chen
- Reporter: Bob Ferreira
- Created: 2026-07-10

## Description

Quarterly rotation of the production Postgres credentials is due. Coordinate a
short maintenance window, rotate the secret in the vault, roll the app
deployments, and confirm connectivity. Do not paste the credential values into
tickets, chat, or email.

## Checklist
- [ ] Schedule maintenance window
- [ ] Rotate vault secret
- [ ] Roll deployments
- [ ] Verify healthchecks

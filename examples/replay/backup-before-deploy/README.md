# backup-before-deploy

Two history checks over recorded effects. `Backup` records `backup.completed` when it
succeeds. `Deploy` requires that effect: `requires = { effects = { contains = [...] } }`.
`Migrate` records `migration.applied` and requires that it is not yet recorded:
`requires = { effects = { excludes = [...] } }`.

- `deploy-without-backup.appa`: no backup was recorded, so the deploy is denied.
- `backup-then-deploy.appa`: the backup runs and records its effect; the deploy finds it.
- `migrate-twice.appa`: the first migration runs. The second is denied because
  `migration.applied` is already in the trajectory.

The replay reports every allowed call as succeeded, so an effect is recorded exactly
when its call was allowed.

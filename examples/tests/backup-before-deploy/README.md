# backup-before-deploy

`mcp/ops/backup` is always allowed. It records `backup.completed`.

`mcp/ops/deploy` is allowed only after `mcp/ops/backup` records `backup.completed`.

`mcp/ops/migrate` is allowed if `migration.applied` is not recorded. It records
`migration.applied`, so it can run only one time.

These tools do not limit the trust level or the audience.

- `backup-before-deploy.appa`: a deploy without a backup is denied. A backup then allows a
  deploy. The first migration is allowed. The second migration is denied.

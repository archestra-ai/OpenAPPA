# backup-before-deploy

`Deploy` needs a completed backup. `Migrate` can run one time.

- `backup-before-deploy.appa`: a deploy without a backup is denied. A backup then allows a
  deploy. The first migration is allowed. The second migration is denied.

An allowed call completes in this example.

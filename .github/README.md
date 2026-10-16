# CI runners

OpenAPPA is a public repository. Its standard GitHub-hosted runners are free.
Ubicloud is an optional paid choice for Rust compilation, native Python
bindings, container builds, and container-based integration tests. It does
not reduce the current GitHub runner bill; compare build time and cost before
keeping it enabled.

The repository variable `APPA_UBICLOUD_CI` controls these jobs. When it is
unset or differs from `on`, they retain their existing GitHub-hosted runners.
When it is `on`, they use native 8-vCPU Ubicloud runners. Linux release
binaries retain Ubuntu 22.04 to preserve their existing glibc baseline;
other migrated jobs use Ubuntu 24.04. macOS, Windows, lightweight checks,
and manifest/tag orchestration retain their existing runners.

Before enabling:

1. Connect the Ubicloud Managed Runners GitHub app to this repository and
   complete Ubicloud billing setup.
2. Check runner capacity for concurrent CI and release builds. Ubicloud
   scales runners per job; there is no fixed server fleet in this repository.
3. Review the premium-runner setting in Ubicloud. The `standard` label does
   not force standard pricing: premium routing is enabled by default.
4. Set the repository Actions variable `APPA_UBICLOUD_CI` to `on`.
5. Check an ordinary CI run and the next applicable Linux binary/image
   builds for runner allocation, cache reuse, duration, and billed minutes.

Published 8-vCPU rates are $0.005/minute for standard x64 and ARM, and
$0.008/minute for premium x64, in USD. Actual cost depends on the selected
fleet and execution time. Existing GitHub cache actions and Docker cache
configuration remain in use; Ubicloud provides transparent caching on its
runners. Expect initially cold caches when changing providers.

To roll back, unset `APPA_UBICLOUD_CI` or set it to `off`. Subsequent jobs
return to their existing GitHub-hosted runners. Changing the variable does
not move an already queued or running job.

Sources: [runner labels](https://www.ubicloud.com/docs/github-actions-integration/runner-types),
[pricing](https://www.ubicloud.com/docs/about/pricing),
[premium routing](https://www.ubicloud.com/docs/github-actions-integration/use-premium-runners),
and [transparent caching](https://www.ubicloud.com/docs/github-actions-integration/ubicloud-cache).

## Shared Docker cache

Docker setup is pinned to the organization's shared
[`setup-docker-builder`](https://github.com/archestra-ai/.github/tree/13f35fa7ee72120bcebe2ad663da42260e629818/actions/setup-docker-builder)
action. CI's designated image job warms its existing per-image scopes on main;
PR consumers restore them without repeating cache uploads. Release publishers
keep their existing architecture-specific scopes and writer behavior. The action
preserves the runner's cache protocol, so free GitHub runners use GitHub's cache
and opted-in Ubicloud runners use their cache proxy. The targeted
`stale-action-refs` annotations identify a deliberate first-party commit pin;
SHA pinning and all other workflow audits remain enabled.

Adopt the optional native-image-store profile only after representative warm CI
stays within 110% of the existing baseline. It must run before any containers.
The default Docker-container profile and the `APPA_UBICLOUD_CI` opt-in remain in
place while performance is validated.

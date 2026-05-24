# Security

For vulnerabilities inherited from upstream PostgreSQL, see
<https://www.postgresql.org/support/security/>.

fastpg-specific releases and automation should follow these repository
controls:

- Keep GitHub Actions workflows on `pull_request`, `push`, or
  `workflow_dispatch`; do not add `pull_request_target` or `workflow_run`.
- Pin external actions to full commit SHAs, with the source version left in a
  YAML comment for review and updater tooling.
- Keep workflow-level permissions empty and grant only job-specific
  permissions.
- Run release publishing in the `release` deployment environment with required
  reviewer approval configured in GitHub settings.
- Keep release jobs free of dependency or build caches.
- Keep GitHub immutable releases enabled.
- Do not overwrite existing GitHub release assets. Publish a new release from a
  new commit instead.
- Keep binary release artifact attestations enabled.
- Configure GitHub rulesets outside the repository so protected branches cannot
  be force-pushed or bypassed, release tags cannot be updated or deleted, and
  repository admins cannot bypass those rules.
- If crates are ever published from this repository, use crates.io Trusted
  Publishing instead of long-lived registry tokens.

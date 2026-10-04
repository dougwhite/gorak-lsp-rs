# gorak update proposals

`Propose gorak update` checks published gorak releases daily. Run it manually on
`main` to check immediately, select a specific newer published tag, or preview
with `dry_run`. Published alphas are included; bare tags and drafts are excluded.
Version ordering prevents downgrades. Only `ecosystem.toml` is committed.

The proposal copies the release's `source_version` and calls out contract changes.
This is a candidate for certification: existing Windows/Linux CI must pass, and
contract changes may require language-server work before merge. There is no
automatic merge or release. An open gorak proposal blocks further proposals so
human fixes are preserved. Closing a proposal declines that tag; a later release
can be proposed. One PR is created per release, including after branch deletion.

## One-time setup

Create a fine-grained personal access token for `dougwhite/gorak-lsp-rs`, with
repository **Contents: read and write** and **Pull requests: read and write**.
Save it as the repository Actions secret `ECOSYSTEM_PR_TOKEN`. Set an expiry and
renew it before expiry. Never put the token in source or paste it into a PR.

The built-in `GITHUB_TOKEN` reads public release data. The separate token writes
the proposal so its normal PR event starts CI without the approval requirement
of workflow-created PRs using `GITHUB_TOKEN`. An installation token from a GitHub
App with these permissions can also be used, but expires and must be minted per
run; this workflow currently expects a personal access token.

Until the secret exists, checks and dry runs work; a real update fails clearly
instead of silently opening a PR with no unattended compatibility CI.

Local preview (requires `gh` authentication):

```sh
GITHUB_REPOSITORY=dougwhite/gorak-lsp-rs python scripts/update_gorak.py
```

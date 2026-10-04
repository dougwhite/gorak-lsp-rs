# Extension release notification

After this repo publishes a tagged release with its assets, the release workflow
notifies the gorak coordinator. It updates the existing extension compatibility
PR, whose installed-VSIX CI certifies the exact released dependency. Normal main
merges do not notify or require extension adaptation.

Merge the gorak dependency coordinator first. Add `ECOSYSTEM_DISPATCH_TOKEN` as
an Actions secret here: a fine-grained token scoped only to `dougwhite/gorak`,
with **Contents: read and write** (GitHub requires this for repository dispatch).
It needs no Issues, Pull requests, Actions, administration or Workflows permission.
Never commit the token. The root keeps using its existing `ECOSYSTEM_PR_TOKEN`
for extension writes and its built-in token for the single tracking issue.

To retry delivery after a token/network problem, run **Notify ecosystem of
dependency release** with the existing published tag. This does not rebuild or
republish the release. Disable `notify` to validate publication without sending.
The receiver validates the repository, tag, release assets and checksums; older
notifications cannot regress newer dependency pins. No automatic merge, final
release or unattended Codex job is implied.

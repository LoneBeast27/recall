# Release scripts for the signed updater

1. `make-manifest.ps1`: writes `recall-update.unsigned.json` for a release bundle (refuses a test build).
2. `sign-manifest.ps1`: signs it with the PC offline and the key on its USB stick.
3. `publish.ps1`: uploads the same signed files to GitLab (generic packages, with a deploy token that can only write packages), Cloudflare Pages and the GitHub release, then reads each public copy back.

`wrangler-version.txt` holds the exact wrangler version installed on the release login
(`npm install -g wrangler@<version>`, then write that version here). `0.0.0` means not set up
yet: `publish.ps1` refuses the Pages step until it matches the installed one.

One step: `sign-and-publish.ps1 -Bundle <bundle>` does 2 and 3 without the USB or offline steps.
It checks the bundle against git, takes only the release key and its password from the private
key folder (`-KeyDir`, default `%USERPROFILE%\recall-signing-keys`; or from Infisical with
`-ProjectId`, into a private temporary folder that is wiped afterwards),
verifies the signature with minisign and with the app's own verifier, publishes (the update site
only when the Cloudflare token can reach its Pages project), and downloads every copy again.
`-WhatIf` runs the checks only. Defaults: secrets `RECALL_RELEASE_KEY` and
`RECALL_RELEASE_KEY_PASSWORD` in `prod`, folder `/recall`; the project id comes from `-ProjectId` or
`RECALL_INFISICAL_PROJECT_ID`, a self-hosted URL from `-Domain` or `RECALL_INFISICAL_DOMAIN`, the
minisign binary from `-Minisign` or `RECALL_MINISIGN`.

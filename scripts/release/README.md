# Release scripts for the signed updater

1. `make-manifest.ps1`: writes `recall-update.unsigned.json` for a release bundle (refuses a test build).
2. `sign-manifest.ps1`: signs it with the PC offline and the key on its USB stick.
3. `publish.ps1`: uploads the same signed files to GitLab, Cloudflare Pages and the GitHub release, then reads each public copy back.

`wrangler-version.txt` holds the exact wrangler version installed on the release login
(`npm install -g wrangler@<version>`, then write that version here). `0.0.0` means not set up
yet: `publish.ps1` refuses the Pages step until it matches the installed one.

# Code signing

## Policy

Free code signing provided by [SignPath.io](https://about.signpath.io), certificate by [SignPath Foundation](https://signpath.org).

- **Committers and reviewers:** [members of KasunRocz-Softwares](https://github.com/orgs/KasunRocz-Softwares/people)
- **Approvers:** [owners of KasunRocz-Softwares](https://github.com/orgs/KasunRocz-Softwares/people?query=role%3Aowner)

Every signed release is built from this repository by GitHub Actions (`.github/workflows/signed-release.yml`) and approved by a maintainer before signing.

All team members with commit or approval rights use multi-factor authentication on GitHub and SignPath.

Only the SyncSound installers are signed. The SignPath artifact configuration (`.signpath/artifact-configuration.xml`) accepts only files named and described as SyncSound, and every release is built from source in CI; no third-party binaries are signed.

**Privacy:** SyncSound does not send any information to other networked systems unless the user asks it to. The one exception is the update check, which downloads `latest.json` from this repository's GitHub Releases and can be turned off in the app. No personal data, telemetry or audio leaves the PC.

**System changes:** SyncSound changes the Windows default playback device only when the user turns on the optional virtual speaker, and puts the previous default back when it is turned off or the app quits. "Start with Windows" (off by default) adds a per-user startup entry. SyncSound installs no drivers. It can be removed from Settings → Apps → Installed apps, which also removes the startup entry.

## Setting it up (maintainers)

1. Apply at <https://signpath.org/apply> for the `SoundSync` project. Requirements: an OSI licence (MIT ✓), a public repository ✓, releases built in CI ✓, and the policy above in the repository ✓.
2. In SignPath, once approved, create:
   - a project with slug `SoundSync`, linked to this GitHub repository
   - an artifact configuration with slug **`installers`**, using the contents of `.signpath/artifact-configuration.xml`
   - a signing policy, for example `release-signing`, with a manual approval step
   - a CI user and its API token
3. In GitHub, under **Settings → Secrets and variables → Actions**, add:
   - Variables: `SIGNPATH_ORGANIZATION_ID`, `SIGNPATH_PROJECT_SLUG` (`SoundSync`), `SIGNPATH_SIGNING_POLICY_SLUG`
   - Secrets:
     - `SIGNPATH_API_TOKEN`
     - `TAURI_SIGNING_PRIVATE_KEY`: the contents of the updater key file
     - `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`: leave empty if the key has no password
4. To release, bump the version on `main`, push a tag `vX.Y.Z`, and approve the signing request in SignPath. The workflow then:
   1. adds the updater signature to the signed installer
   2. writes `latest.json`
   3. publishes the release

Until step 3 is done, the workflow is skipped. In the meantime `scripts/release.ps1 -Publish` still publishes unsigned (SmartScreen-warning) builds from your PC.

> Storing the updater key as a GitHub secret lets CI sign updates. Keep a private backup copy as well. If the key is lost, installed copies can never accept another update.

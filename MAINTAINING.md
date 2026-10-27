# Maintaining Sessionmux

Notes for maintainers: repository settings, releases, the launch checklist, the winget package and
optional code signing.

## Before the first public push

The repository lives at `https://github.com/joyal-droid/sessionmux`. Every link in the app derives
from the `repo_url!` macro in `src/brand.rs`; `web/panel.js` carries the same URL as `REPO_URL`, the
fallback used until the app info arrives. If the repository ever moves, change both, the Attribution
URL in `LICENSE` (Exhibit B), the header comment of every source file, the Markdown files,
`.github/ISSUE_TEMPLATE/config.yml` and `packaging/winget/` (`git grep -n joyal-droid/sessionmux`
lists every place).

Then set up the [commit identity](#committing-without-leaking-identity) and apply the
[repository settings](#repository-settings) and the [repository protection](#repository-protection)
below.

## Repository settings

Copy-paste values for the GitHub repository page.

**About > Description** (at most 350 characters):

```text
Session multiplexer for Claude Desktop on Windows: run multiple Claude accounts side by side, switch instantly from a floating button without logging out, move Claude Code sessions between accounts, and sign in to extra accounts with Google or Apple. 1.7 MB native app, free and open source.
```

**About > Website:** `https://sessionmux.netlify.app/`

**About > Topics** (exactly these 20):

```text
claude claude-desktop claude-ai anthropic multi-account multiple-accounts account-switcher session-manager session-multiplexer profile-switcher claude-code windows windows-11 rust webview2 desktop-app productivity system-tray switcher open-source
```

**Website.** The landing page is not part of this repository. It is deployed by hand to Netlify
(**Add new project > Deploy manually**, site name `sessionmux`, then **Deploys > drag and drop**
for updates) and lives at
`https://sessionmux.netlify.app/`, which is the About website above. Its Open Graph image is
`docs/social-preview.png` and its screenshots are the ones under `docs/screenshots/`, so the
deployed copies need refreshing whenever those are regenerated.

**Actions secret.** **Settings > Secrets and variables > Actions > New repository secret**, name
`SESSIONMUX_PRIVACY_TERMS`: the private terms the privacy scan of CI and the release workflow
looks for (separated by `;`).

**Social preview.** **Settings > General > Social preview > Edit > Upload an image…** and choose
`docs/social-preview.png` (1280×640).

**Download button.** The README shows `docs/download-button.png` at width 360 (the file is drawn
at twice that, 720×144). Regenerate it with `.\scripts\make-download-button.ps1 -SizeMb X.YZ`
whenever the product name or the installer size changes (`-Name` and `-Repo` carry the name).

**Icons.** `assets\icon.ico` (the application icon) and `assets\tray.ico` (the tray icon) are
drawn from the mark by `.\scripts\make-icons.ps1`; regenerate both whenever the mark changes.
`build.rs` embeds them in the exe on the next build.

**Discussions.** **Settings > General > Features**, tick **Discussions**. The issue chooser links
to `https://github.com/joyal-droid/sessionmux/discussions` for questions and ideas.

**Private vulnerability reporting.** **Settings > Advanced Security** (called **Code security**
on older versions of the page) **> Private vulnerability reporting > Enable**, so the reporting link in `SECURITY.md` and the issue chooser works (it is off
on new repositories).

**First release.** Push the `v1.0.0` tag as in the [release checklist](#release-checklist), then
check that the draft release carries `Sessionmux-Setup-1.0.0.exe`, `Sessionmux-Setup.exe` and
`SHA256SUMS` and that its notes open with the Download section, paste the non-affiliation line at
the end of the release notes and publish it.

## Repository protection

Apply these once, by hand, on GitHub. They make sure that a change to `main` or a release can only
happen in the open, through CI, and that a stolen password alone is not enough to take over the
account. Re-check them after any change to the repository or the account.

### Account

- **Sign-in:** add a passkey (**Settings > Password and authentication > Passkeys**) and turn on
  two-factor authentication with an authenticator app. Do not add SMS as a fallback method.
- **Recovery codes:** download them and keep them offline (printed, or on a drive that is not
  always connected), never in this repository or a synced folder.
- **Sessions and apps:** review **Settings > Sessions** and sign out of anything unknown; review
  **Settings > Applications** (Authorized OAuth Apps and Authorized GitHub Apps) and revoke
  anything not in use.
- **Tokens:** this repository needs no personal access token. If one is ever needed, create a
  fine-grained token limited to this repository, with the fewest permissions and a short expiry,
  and never store it in the repository, a workflow file or a commit.

### Repository

- **Branch protection for `main`** (**Settings > Branches > Add classic branch protection rule**,
  pattern `main`):
  - **Require a pull request before merging**, with 0 required approvals, so the owner can still
    merge their own pull requests;
  - **Require status checks to pass before merging**, with **Require branches to be up to date**,
    and the CI checks `Format`, `Clippy`, `Dependency audit` and `Build, test and smoke` selected;
  - **Require linear history**;
  - **Do not allow bypassing the above settings** (this is the "include administrators" option);
  - **Allow force pushes** and **Allow deletions** left off.
- **Tag protection for `v*`** (**Settings > Rules > Rulesets > New ruleset > New tag ruleset**,
  enforcement **Active**, target pattern `v*`): **Restrict creations** with only **Repository
  admin** in the bypass list, **Restrict updates**, **Restrict deletions** and **Block force
  pushes**. A pushed release tag can then never be moved to another commit.
- **Security features** (**Settings > Advanced Security**, called **Code security** on older
  versions of the page): turn on **Dependabot alerts**, **Dependabot
  security updates**, **Secret scanning** with **Push protection**, and **Private vulnerability
  reporting**. `.github/dependabot.yml` adds weekly version updates for Cargo and GitHub Actions.
- **Actions** (**Settings > Actions > General**):
  - **Actions permissions:** **Allow joyal-droid, and select non-joyal-droid, actions and reusable
    workflows**, with **Allow actions created by GitHub** and **Allow actions by Marketplace
    verified creators** ticked, and these two actions, which are not from verified creators,
    listed under **Allow or block specified actions and reusable workflows**:
    `Swatinem/rust-cache@d1527c8f67ef9af357290475d4af4b0fab8f12e0,
    rustsec/audit-check@69366f33c96575abad1ee0dba8212993eecbe998`. When a Dependabot pull request
    moves one of these pins, update the entry before merging it, or CI fails with "action is not
    allowed". If the page offers **Require actions to be pinned to a full-length commit SHA**, tick
    it;
  - **Approval for running fork pull request workflows from contributors:** the strictest
    option, approval for every outside contributor, so no workflow runs for a fork pull request
    until you have read it;
  - **Workflow permissions:** **Read repository contents and packages permissions**, and leave
    **Allow GitHub Actions to create and approve pull requests** off.
  Every workflow also declares its own minimal `permissions:` block, and every action is pinned to
  a full commit SHA.
- **Code owners:** `.github/CODEOWNERS` assigns every file to `@joyal-droid`.
- **Releases** are created only by the tag workflow (`.github/workflows/release.yml`), which builds
  the installer, records the build-provenance attestation and opens a draft. Never upload a
  release file by hand; a file without a matching attestation is not an Sessionmux release.

### If the account were ever compromised

1. Regain access through GitHub's account recovery flow (recovery codes, passkey, or
   <https://github.com/login> > **Forgot password?** and the 2FA recovery steps), then sign out of
   all sessions and revoke every OAuth app, GitHub App and token.
2. Rotate nothing: there are no signing keys. Releases are tied to the workflow run that built
   them, so there is no key that an attacker could have copied to forge one later.
3. Re-check every setting in this section, the list of collaborators and deploy keys, and the
   workflow files, branch and tag history for changes you did not make.
4. Re-verify the latest release: `gh attestation verify Sessionmux-Setup-<version>.exe --repo joyal-droid/sessionmux --signer-workflow joyal-droid/sessionmux/.github/workflows/release.yml
   --source-ref refs/tags/v<version>` must pass and name the tag and its commit; that commit must be one
   you merged, and the hashes in `SHA256SUMS` must match the files. If anything was published in the meantime, delete it and say
   so in a security advisory.

## Committing without leaking identity

Commits are public forever, so nothing in them may point to a person or a machine beyond the
handle `joyal-droid`.

- Set the identity for this repository only, never globally, and check it before the first
  commit:

  ```powershell
  git config --local user.name joyal-droid
  git config --local user.email 73401204+joyal-droid@users.noreply.github.com
  git config --local --get-regexp '^user\.'
  ```

  The global identity (`git config --global`) is never used for this repository. Also turn on
  **Settings > Emails > Keep my email addresses private** and **Block command line pushes that
  expose my email** on the GitHub account.
- Write commit dates in UTC, so no time zone is recorded:

  ```powershell
  $env:GIT_AUTHOR_DATE = $env:GIT_COMMITTER_DATE = (Get-Date).ToUniversalTime().ToString("yyyy-MM-dd'T'HH':'mm':'ss'+0000'", [Globalization.CultureInfo]::InvariantCulture)
  git commit
  Remove-Item Env:GIT_AUTHOR_DATE, Env:GIT_COMMITTER_DATE
  ```

  Set both variables for tags (`git tag -a`) and rebases too.
- GPG or SSH signing stays off (`git config --local commit.gpgsign false`) unless the key itself
  is pseudonymous: a signing key carries a name and an e-mail address.
- Before pushing, check what will become public:

  ```powershell
  git log --format='%an <%ae> %ad | %cn <%ce> %cd'
  git grep -n -i -F -e 'C:\Users' -e '.pdb'
  ```

  Every author and committer must read `joyal-droid <73401204+joyal-droid@users.noreply.github.com>`
  with a `+0000` offset.

## Release checklist

1. Make sure `main` is green in CI.
2. Bump `version` in `Cargo.toml` and run `cargo build --release` so `Cargo.lock` follows.
3. Move the "Unreleased" entries in `CHANGELOG.md` under the new version with today's date, and
   update the compare links at the bottom.
4. If dependencies changed, update the versions and licenses in `THIRD-PARTY.md`
   (`cargo tree -e normal --target x86_64-pc-windows-msvc`).
5. Refresh `docs/screenshots/` if the UI changed: run
   `$env:SESSIONMUX_SMOKE_FIXTURE = '1'; target\release\Sessionmux.exe --smoke`, copy the captures
   from `smoke\` and re-encode them as compressed PNGs (at most 150 KB each). Never use captures
   made with real data.
6. Run the privacy scan locally: `installer\build.ps1` (it remaps the build paths and scans the exe
   and the setup exe; any hit stops it), then the images:
   `.\scripts\check-binary-privacy.ps1 (Get-ChildItem docs -Recurse -Filter *.png).FullName`.
   Every file must report `clean`; fix the source of a hit instead of allowing it.
7. Commit (`Sessionmux X.Y.Z`) with the [identity above](#committing-without-leaking-identity) on
   a branch, open a pull request and merge it once CI passes (`main` accepts changes only through
   pull requests, see [Repository protection](#repository-protection)). Then tag the merged commit
   and push the tag:

   ```powershell
   git switch main
   git pull --ff-only
   git tag -a vX.Y.Z -m "Sessionmux X.Y.Z"
   git push origin vX.Y.Z
   ```

   The very first push of `main` to the empty repository happens before branch protection is
   turned on.

8. The release workflow (`.github/workflows/release.yml`) runs on the `v*` tag. It first checks
   that the tag matches the version in `Cargo.toml` and that the tagged commit is on `main`, and
   runs format, clippy, the tests and the dependency audit. A read-only build job then builds with
   the pinned Rust version and NSIS, runs the privacy scan on the exe and the setup exe, copies
   the setup exe to `Sessionmux-Setup.exe` (the same installer under a fixed name, so
   `https://github.com/joyal-droid/sessionmux/releases/latest/download/Sessionmux-Setup.exe`
   always serves the newest release), and writes `BUILDINFO.txt` (toolchain versions) and
   `SHA256SUMS` (both setup names, `Sessionmux.exe` and `BUILDINFO.txt`). Last, a publish job
   that runs no build code records a build-provenance attestation for both setup names,
   `SHA256SUMS` and `BUILDINFO.txt`, and creates a draft release with those files and notes that
   open with a Download section, followed by the matching section of `CHANGELOG.md`. The Rust
   version (`RUST_TOOLCHAIN`) and the NSIS version (`NSIS_VERSION`) at the top of the workflows
   are updated by hand.
9. Review the draft (the notes open with the Download section, and both `Sessionmux-Setup-X.Y.Z.exe`
   and `Sessionmux-Setup.exe` are attached), download the setup file from it and check it with
   `gh attestation verify Sessionmux-Setup-X.Y.Z.exe --repo joyal-droid/sessionmux --signer-workflow joyal-droid/sessionmux/.github/workflows/release.yml --source-ref refs/tags/vX.Y.Z`,
   then publish it. Nothing
   is public until then.
10. Update the winget manifests (below) only after publishing: `InstallerUrl` resolves only then.

To build an installer locally instead: `powershell -ExecutionPolicy Bypass -File installer\build.ps1`
(output in `dist\`, with size and SHA-256 printed).

## Launch checklist

Ready-to-paste texts for announcing a release. Keep the non-affiliation line in every text, post
each only once, and answer questions in the threads.

- [ ] **Show HN** (title at most 80 characters):

  ```text
  Show HN: Sessionmux – switch between Claude Desktop accounts on Windows
  ```

  First comment:

  ```text
  I keep separate personal, work and client Claude accounts, and Claude Desktop runs one account at a time, so I logged out and back in several times a day, and each time the window closed and the other account's Code-tab sessions were out of reach.

  Sessionmux is a small native Windows app (Rust, about 1.7 MB, about 4 MB of memory while idle) that gives every extra account its own Claude data folder and switches between them from a floating button, the tray or Ctrl+Shift+Space. It can also move or copy Code-tab sessions between accounts, and it routes the browser's claude:// sign-in link to the right window, so Google and Apple sign-in work for a second account.

  No telemetry, no network connections of its own. Source and installer: https://github.com/joyal-droid/sessionmux

  Sessionmux is an independent, unofficial open-source project, not affiliated with or endorsed by Anthropic.
  ```

- [ ] **r/ClaudeAI** title:

  ```text
  I made a free, open-source way to run several Claude Desktop accounts side by side on Windows
  ```

  Body:

  ```text
  If you use more than one Claude account (personal, work, client), Claude Desktop makes you log out and back in to switch. Sessionmux runs each account in its own window with its own sign-in and switches between them from a floating button or Ctrl+Shift+Space.

  - Move or copy Claude Code sessions from one account to another
  - Google and Apple sign-in work for extra accounts
  - Works with the Microsoft Store and the direct-download version of Claude Desktop
  - About 1.7 MB, no telemetry, source on GitHub (CPAL-1.0)

  https://github.com/joyal-droid/sessionmux

  Sessionmux is an independent, unofficial open-source project, not affiliated with or endorsed by Anthropic. Use it with accounts you own and follow Anthropic's terms for each of them.
  ```

- [ ] **r/Windows** title:

  ```text
  Sessionmux: a 1.7 MB open-source tray and floating-button switcher for multiple Claude Desktop accounts
  ```

  Body:

  ```text
  A small native Windows 10/11 app written in Rust. It keeps several Claude Desktop accounts signed in at once, each in its own window, and switches between them from a floating button that snaps to the screen edge, the notification-area icon or a global hotkey. The panel is a WebView2 page that exists only while it is open, so the app idles at about 4 MB.

  Per-user install, no administrator rights, SHA-256 checksums on every release.

  https://github.com/joyal-droid/sessionmux

  Sessionmux is an independent, unofficial open-source project, not affiliated with or endorsed by Anthropic or Microsoft.
  ```

- [ ] **awesome-claude list** pull request line:

  ```text
  - [Sessionmux](https://github.com/joyal-droid/sessionmux) - Switch between multiple Claude Desktop accounts on Windows and move Claude Code sessions between them. Unofficial, not affiliated with Anthropic.
  ```

- [ ] **Product Hunt** tagline (at most 60 characters) and description:

  ```text
  Switch between Claude Desktop accounts on Windows
  ```

  ```text
  Run personal, work and client Claude Desktop accounts side by side, switch from a floating button, and move Claude Code sessions between accounts. Free and open source. Sessionmux is an independent, unofficial project, not affiliated with or endorsed by Anthropic.
  ```

- [ ] **X / LinkedIn** post:

  ```text
  Sessionmux 1.0 is out: a free, open-source Windows app to run several Claude Desktop accounts side by side, switch between them from a floating button, and move Claude Code sessions from one account to another. About 1.7 MB, no telemetry.

  https://github.com/joyal-droid/sessionmux

  Independent and unofficial; not affiliated with or endorsed by Anthropic.
  ```

- [ ] **winget submission**, only after the first release is published: fill in the manifests as
  described under [winget](#winget), submit them to microsoft/winget-pkgs, and change the README
  badge and the "winget" lines in `README.md` from "planned" to the
  `winget install Sessionmux.Sessionmux` command once the package is merged. The pull request
  description carries the same non-affiliation line.
- [ ] **Update the comparison table when competitors change.** Re-read the projects listed in the
  README's "How it compares" section before each release and correct anything they now document;
  describe only what they document publicly and write "not documented" otherwise.

## winget

Manifests for the package `Sessionmux.Sessionmux` live in `packaging/winget/manifests/s/Sessionmux/Sessionmux/<version>/`
(version, default-locale and installer manifests, schema 1.6.0). The installer is the per-user
NSIS setup (`InstallerType: nullsoft`, silent switch `/S`).

For each release:

1. Copy the previous version folder to the new version and update `PackageVersion`,
   `InstallerUrl` and `ReleaseNotesUrl`.
2. Copy the hash from `SHA256SUMS` into `InstallerSha256` (winget accepts either case).
3. Validate with `winget validate --manifest packaging\winget\manifests\s\Sessionmux\Sessionmux\X.Y.Z`
   and test with `winget install --manifest <same folder>` on a test machine.
4. Submit the folder to [microsoft/winget-pkgs](https://github.com/microsoft/winget-pkgs) as a pull
   request (for example with `wingetcreate submit`), or let `wingetcreate update Sessionmux.Sessionmux`
   generate it from the release URL.

## Code signing (optional)

Unsigned installers work, but Windows SmartScreen warns about them until the file has built up
reputation. `installer\build.ps1 -Sign` signs both `Sessionmux.exe` and the setup exe with
`signtool` and verifies them. The certificate comes from the environment:

- `SESSIONMUX_SIGN_THUMBPRINT`: SHA-1 thumbprint of a certificate in `CurrentUser\My` (also works
  for hardware tokens), or
- `SESSIONMUX_SIGN_PFX` and `SESSIONMUX_SIGN_PFX_PASSWORD`: a `.pfx` file and its password.

`-TimestampUrl` overrides the RFC 3161 timestamp server (default `http://timestamp.digicert.com`).
`signtool.exe` is taken from `SIGNTOOL`, else the newest Windows 10/11 SDK, else `PATH`.

Open-source projects can get free code signing from [SignPath.io](https://signpath.io) (SignPath
Foundation). It signs artifacts produced by CI, so it would replace `-Sign` with a signing step in
`release.yml` between the build and the upload.

## Attribution

The attribution described in [docs/attribution.md](docs/attribution.md) is required by the license
for every distributed build. Release builds must show it; check the panel header and the About
card in the smoke captures before tagging.

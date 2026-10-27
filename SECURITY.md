# Security policy

## Supported versions

Security fixes go into the latest release. Please update to it before reporting.

| Version | Supported |
| --- | --- |
| 1.0.x | yes |

## Reporting a vulnerability

Please **do not open a public issue** for security problems.

Report them privately through GitHub's private vulnerability reporting:
<https://github.com/joyal-droid/sessionmux/security/advisories/new> (repository **Security** tab >
**Report a vulnerability**).

Include:

- the Sessionmux version (**Settings > About**) and the Windows version,
- what an attacker can do and the steps to reproduce it,
- relevant lines from `%APPDATA%\Sessionmux\sessionmux.log`, with personal data removed.

You can expect an acknowledgement within 7 days and an assessment within 30 days. Fixes are
released as a new version and credited in the advisory unless you prefer otherwise.

Sessionmux is a volunteer project and has **no bug bounty**: reports are welcome and credited, but
not paid.

## Security model

This section describes what Sessionmux does on your PC, in terms you can check yourself in the
source, with Process Monitor or with the Windows tools named below. It describes the code in this
repository; it is not a promise that the code is free of bugs.

### What it writes

| What | Where | When |
| --- | --- | --- |
| Account list and settings | `%APPDATA%\Sessionmux\sessionmux.json` | when you change an account or a setting |
| Log of actions and errors (no conversation contents, no passwords, no sign-in codes), rotated at 1 MB | `%APPDATA%\Sessionmux\sessionmux.log` | while running |
| Sign-in routing state | `%APPDATA%\Sessionmux\relay.json` | only while an extra account signs in; deleted afterwards |
| The panel's WebView2 profile | `%APPDATA%\Sessionmux\WebView2\` | when the panel opens |
| A Claude data folder per extra account | `%APPDATA%\Claude-<name>\` | when you add an account (Claude Desktop fills it) |
| MCP server settings copied into extra accounts | the extra account's Claude data folder | only while **Share MCP servers from Main** is on |
| A moved, copied or handed-over Code-tab session | the target account's data folder and `%USERPROFILE%\.claude\projects\` | only when you move, copy or hand over a session, or an account hits its limit with automatic hand-over on |
| Three hook entries that run `Sessionmux.exe --approve-hook` / `--notify-hook` | `%USERPROFILE%\.claude\settings.json` | only while **Show waiting sessions and approve from Sessionmux** is on; removed when you turn it off and by the uninstaller |
| A backup of that file before the change | `%USERPROFILE%\.claude\settings.json.sessionmux-bak` | when the hook entries are added (never a copy that contains them) |

Apart from removing an unused `license.json` from `%APPDATA%\Sessionmux` at start, it deletes
data only when you ask: an extra account's folder (only folders directly inside
`%APPDATA%` whose name starts with `Claude-`, never the Main account's, never while a process runs
from it), a session you delete (never while it is open in Claude), or the source record of a
session you move (left in place while the session is open in Claude, whose process still owns its
files).

### What it changes in the registry

Everything is under `HKEY_CURRENT_USER`; nothing is written for other users or for the whole
machine.

- **`Software\Microsoft\Windows\CurrentVersion\Run`, value `Sessionmux`** (`"<exe>" --hidden`):
  present only while **Start with Windows** is on; removed when you turn it off and by the
  uninstaller.
- **`Software\Classes\claude\shell\open\command`** (the `claude://` link handler): changed only
  while an extra account that Sessionmux opened is waiting to sign in, and only with **Route sign-in
  links to the account being signed in** on. Sessionmux saves Claude's own value in `relay.json`,
  points the handler at `Sessionmux.exe --relay`, and puts the saved value back when the sign-in
  finishes or is cancelled, after 15 minutes, when the account's window closes, when Sessionmux
  quits or, after a crash, when it next starts. The uninstaller ends an active relay the same way
  and removes a handler value that still points at Sessionmux. The relay accepts a single
  `claude://` link and nothing else, passes it to the waiting account's window, and neither stores
  nor sends it. Details: [docs/sign-in-routing.md](docs/sign-in-routing.md).
- **The uninstall entry** (`Software\Microsoft\Windows\CurrentVersion\Uninstall\Sessionmux`),
  written by Setup and removed by the uninstaller.

To find the Microsoft Store version of Claude Desktop, Sessionmux also reads (without changing it)
the per-user package list under
`HKCU\Software\Classes\Local Settings\Software\Microsoft\Windows\CurrentVersion\AppModel\Repository\Packages`.

### What it does with other programs

- It lists running `claude.exe` processes and **reads their command lines** to find each
  account's `--user-data-dir`. Windows keeps a process's command line in its process environment
  block; Sessionmux opens the process with read access (`PROCESS_VM_READ`, which Windows grants for
  the same user's non-elevated processes) and reads it with `ReadProcessMemory`.
- It **brings Claude windows to the front** when you switch accounts.
- It **starts Claude Desktop** for an account (`claude.exe --user-data-dir=<folder>`), and
  **closes an account's Claude windows when you ask** for it: when you close or remove the
  account, and when you hand a session over from the panel with the confirm dialog's restart
  option ticked, which restarts the target account's window only if no session of that account is
  running at that moment (a session entry that cannot be read counts as running). It closes first with a normal close message, then, after 3 seconds,
  ends the remaining processes of that account, restricted to Claude Desktop's own executables.
  An automatic hand-over never closes a Claude window.
- It opens links and folders you click in your default browser or in Explorer; the panel can only
  ask it to open `http` and `https` links.

It reads, with `ReadProcessMemory`, only the small structures in the process environment block
that locate the command line, and the command line itself. It does not inject code into other
processes, install Windows hooks into them, or read any other part of their memory.

### Claude Code hooks and the approval pipe

These exist only while **Show waiting sessions and approve from Sessionmux** is on.

- **Settings entries.** Sessionmux adds three entries to `%USERPROFILE%\.claude\settings.json`
  after showing them and keeping a backup (`settings.json.sessionmux-bak`); they run
  `Sessionmux.exe` in exec form (no shell) with `--approve-hook` or `--notify-hook`. An entry
  counts as Sessionmux's only when its command is Sessionmux's exe path and its arguments are one
  of those two, and only those entries are ever removed. Turning the option off removes them, and
  the uninstaller runs `Sessionmux.exe --remove-hooks` before it deletes any files. Project
  settings files are never touched.
- **A same-user boundary.** The helpers talk to the running app over the named pipe
  `\\.\pipe\Sessionmux.<your user SID>`, created with a security descriptor that grants your
  Windows user only (`D:P(A;;GA;;;<SID>)`). Other users cannot open it. A process running as your
  user can, but such a process could already edit `settings.json` or Claude's files directly, so
  the pipe adds no new reach. Messages are newline-delimited JSON of at most 64 KB; anything
  malformed is dropped.
- **One-shot requests.** Every approval request carries a random 128-bit id. A decision answers
  one request once, goes back only over the connection of the helper that asked, and is dropped
  if that helper has gone (you answered in Claude) or 90 seconds have passed.
- **Nothing is auto-approved.** **Allow** is sent only when you click it on that request's card;
  it is never the default button, it arms only after a short delay, and Enter never triggers it.
  The helper never changes the tool's input or Claude's permission rules.
- **Fail-open to Claude's own prompt.** When the helper is unsure (unreadable input, the wrong
  event, Sessionmux not running, a pipe error, no answer within 85 seconds, a crash), it exits
  with code 0 and prints nothing, so Claude Code shows its own permission prompt as if the hook
  were not there.
- **Log contents.** The log records the session, the tool name and allow or deny for each
  decision, one line when a helper cannot connect to the app, and one line per hand-over, never
  the tool's input or any transcript content.

### What it never does

- **No network connections.** The app contains no network code. The panel's pages are built into
  `Sessionmux.exe`, and their content security policy (`default-src 'self'`) blocks requests to
  anywhere else. The Microsoft Edge WebView2 Runtime that draws the panel is part of Windows and is
  kept up to date by Microsoft, not by Sessionmux. You can check this with Resource Monitor
  (**Network** tab, with `Sessionmux.exe` and the `msedgewebview2.exe` processes it started ticked)
  or in PowerShell, which also covers those WebView2 processes:

  ```powershell
  $ids = @((Get-Process Sessionmux).Id)
  $ids += (Get-CimInstance Win32_Process | Where-Object { $ids -contains $_.ParentProcessId }).ProcessId
  Get-NetTCPConnection -OwningProcess $ids -ErrorAction SilentlyContinue
  ```
- **No auto-update and no telemetry.** A new version arrives only when you run a newer Setup.
- **No elevation.** Setup and the app run as your user (`RequestExecutionLevel user` in
  `installer/sessionmux.nsi`, `asInvoker` in `assets/sessionmux.manifest`) and never ask for
  administrator rights.
- **Per-user install** into `%LOCALAPPDATA%\Programs\Sessionmux`, with Start menu and desktop
  shortcuts for your user only.
- **No services, no drivers, no scheduled tasks, no browser extensions.**

## Verifying a download

Each release carries the installer under two names, `Sessionmux-Setup-<version>.exe` and
`Sessionmux-Setup.exe` (the same file, under a name that always points at the newest release),
`BUILDINFO.txt` (the versions of Rust, the MSVC linker and NSIS that built it) and `SHA256SUMS`,
which lists the SHA-256 of the setup file under both names, of the `Sessionmux.exe` inside it and
of `BUILDINFO.txt`. The attestation below covers both installer names as well.

There is **no signing key**. Releases are built by this repository's `release.yml` workflow on
GitHub Actions when a `v*` tag is pushed on a commit that is already on `main`, after the tests,
lints and dependency audit pass. The job that builds has read-only access; a separate job, which
runs no build code, asks GitHub for a build-provenance attestation of the setup file,
`SHA256SUMS` and `BUILDINFO.txt` and creates the draft release. The attestation is bound to that
workflow run through GitHub's OIDC identity, not to a key someone holds.

1. **Attestation** (needs the [GitHub CLI](https://cli.github.com)):

   ```powershell
   gh attestation verify Sessionmux-Setup-1.0.0.exe --repo joyal-droid/sessionmux --signer-workflow joyal-droid/sessionmux/.github/workflows/release.yml --source-ref refs/tags/v1.0.0
   ```

   A file that passes this command was built by `.github/workflows/release.yml` of this repository
   in a run for the tag `v1.0.0`, not on anyone's own computer, the maintainer's included. The
   output names the repository, the workflow file and the commit the file was built from. (The
   shorter `--owner joyal-droid` alone accepts an attestation from any repository of that account,
   so it proves less.)

2. **SHA-256** (no extra tools): compare the result with the setup line in `SHA256SUMS`.

   ```powershell
   Get-FileHash .\Sessionmux-Setup-1.0.0.exe -Algorithm SHA256
   ```

3. **Rebuild from source.** Release builds take the build date and `SOURCE_DATE_EPOCH` from the
   tagged commit, make the PE timestamp deterministic (`/Brepro`) and remove build-machine paths, so
   the same tag built with the same Rust and MSVC toolchain versions gives the same
   `Sessionmux.exe`:

   ```powershell
   git clone https://github.com/joyal-droid/sessionmux
   cd sessionmux
   git checkout v1.0.0
   powershell -ExecutionPolicy Bypass -File installer\build.ps1
   Get-FileHash target\release\Sessionmux.exe -Algorithm SHA256
   ```

   Compare the result with the `Sessionmux.exe` line in `SHA256SUMS` (also printed in the release
   notes). Compare the exe, not the installer: NSIS stamps the setup file when it is built, so its
   hash changes with every build. `BUILDINFO.txt` names the Rust, MSVC and NSIS versions the
   release used; a different exe hash from a different toolchain is expected and is not by itself
   a sign of tampering. `build.ps1` needs [NSIS](https://nsis.sourceforge.io) 3 for the packaging
   step that follows; without it the script stops with an error after `Sessionmux.exe` is built,
   and the exe and its hash are still valid for this comparison.

Authenticode code signing may be added to releases as an extra; none of the checks above depends
on it.

## Scope

Areas where a report is most valuable:

- the `claude://` protocol-handler routing (`--relay`, `--end-relay`, `relay.json`, the
  `HKCU\Software\Classes\claude` key) described in [docs/sign-in-routing.md](docs/sign-in-routing.md),
- the panel's IPC surface (`window.sessionmux` in `web/shim.js`, validated in `src/app.rs`) and the
  content security policy of `web/panel.html`,
- folder deletion when an account or session is removed, and session transfer (including of
  sessions that are open in Claude, and hand-over),
- the Claude Code hook helpers (`--approve-hook`, `--notify-hook`, `--remove-hooks`), the edits to
  `%USERPROFILE%\.claude\settings.json` and the approval pipe,
- the installer and uninstaller (`installer/sessionmux.nsi`),
- the release workflow (`.github/workflows/release.yml`) and the provenance of published files.

Problems in Claude Desktop itself or in the WebView2 Runtime should be reported to Anthropic or
Microsoft respectively.

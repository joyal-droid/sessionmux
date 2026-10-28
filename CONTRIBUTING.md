# Contributing to Sessionmux

Thanks for helping. This file covers setting up, building, testing and sending changes. How the app
is put together is described in [docs/architecture.md](docs/architecture.md), the visual rules in
[docs/design.md](docs/design.md), and the attribution that every build must keep in
[docs/attribution.md](docs/attribution.md).

By contributing you agree that your contribution is licensed under the
[Common Public Attribution License 1.0](LICENSE), and you follow the
[Code of Conduct](CODE_OF_CONDUCT.md).

## Setup

Sessionmux builds for Windows and macOS from the same code. The sections below describe Windows;
building, testing and packaging on a Mac is in [Building on a Mac](#building-on-a-mac).

- Windows 10 or 11 x64.
- Rust stable with the MSVC toolchain (`x86_64-pc-windows-msvc`), installed with
  [rustup](https://rustup.rs), plus the `rustfmt` and `clippy` components.
- Visual Studio Build Tools with the "Desktop development with C++" workload (MSVC linker and
  Windows SDK).
- The Microsoft Edge WebView2 Runtime (needed to run the panel and the smoke check).
- Optional, for the installer: [NSIS](https://nsis.sourceforge.io) 3.x (`makensis`).

If `cargo` is not on `PATH` after installing rustup, add it for the current session:

```powershell
$env:PATH = "$env:USERPROFILE\.cargo\bin;$env:PATH"
```

Run every command below from the repository root.

### Optional: a shorter target directory

Build output goes to `target\`. When the checkout lives in a deep folder, some build-script paths
can exceed Windows' `MAX_PATH`. Point cargo at a short shared folder in that case; it is a local
convenience and nothing in the repository depends on it:

```powershell
$env:CARGO_TARGET_DIR = "$env:LOCALAPPDATA\sessionmux-target"
```

Then use `cargo run --release -- --smoke` instead of the `target\release\...` paths below.

## Build

| Command | Result |
| --- | --- |
| `cargo check` | type-check only |
| `cargo build --release` | `target\release\Sessionmux.exe` |
| `cargo run --release` | build and start the app (`-- --hidden` starts without the panel) |

`build.rs` embeds the icons, the application manifest and the version resource, and sets the build
date shown in the About card (UTC today). To pin the date for a release build, set
`SESSIONMUX_BUILD_DATE=YYYY-MM-DD`; `installer\build.ps1` does this for you. `build.rs` re-runs only
when `build.rs`, `assets\`, `web\` or that variable change.

A plain `cargo build` (debug or release) embeds absolute paths of your machine in the exe: rustc
records the source location of every possible panic, and for crates from the cargo registry that is
`C:\Users\<you>\.cargo\registry\...`. That is fine for local work, but only builds made by
`installer\build.ps1` or by CI are release-clean: they set `RUSTFLAGS` with
`--remap-path-prefix` for the repository, `%USERPROFILE%`, the cargo home and the rustup home, and
`-Clink-arg=/DEBUG:NONE`, and then check the result with `scripts\check-binary-privacy.ps1`. Never
publish an exe from a plain cargo build. Changing `RUSTFLAGS` makes cargo rebuild everything, so
expect a full build when you switch between the two.

Only one copy of Sessionmux runs at a time. If an installed copy is running, a freshly built exe
hands over to it and exits; quit the installed copy from its tray menu while you test.

## Test

```powershell
cargo test
```

Unit tests are hermetic: they use temporary folders and a throw-away registry key
(`HKCU\Software\Classes\sessionmux-test-<uuid>`), never your real Claude data or the real `claude`
protocol handler.

### Smoke check

```powershell
target\release\Sessionmux.exe --smoke
```

The smoke run is a headless self-check. It uses `smoke\userData` as its user-data folder, never
launches, focuses or closes Claude windows and never touches the registry. It renders the bubble
(icon and mini tray) and every panel tab, writes the captures to `smoke\*.png`, prints one JSON line
(`{"ok":true,...}` with account counts, window sizes, panel start time and memory figures) and exits
with code 0, or prints `{"ok":false,"error":...}` and exits with 1.

| Variable | Effect |
| --- | --- |
| `SESSIONMUX_SMOKE_FIXTURE=1` | ignore any real store and seed demo accounts and sessions, plus two approval requests, one waiting session and one limited account so the approval cards and badges are captured (used by CI and for `docs/screenshots/`) |
| `SESSIONMUX_SMOKE_SIGNIN=1` | also capture `smoke\signin-notice.png` with a synthetic sign-in notice |

### Hook helpers

Claude Code starts `Sessionmux.exe` with one hook argument per event and the event's JSON on
stdin:

| Arguments | Behaviour |
| --- | --- |
| `--notify-hook` | a `Notification` or `StopFailure` event: passed to the running app over the per-user pipe; never prints anything |
| `--approve-hook` | a `PermissionRequest` event: waits up to 85 s for a decision from the panel and prints Claude Code's allow or deny JSON, or prints nothing |
| `--remove-hooks` | removes Sessionmux's entries from `%USERPROFILE%\.claude\settings.json` (the uninstaller runs it) |

The helpers always exit with code 0 and print nothing when they are unsure, so Claude's own prompt
decides. To try one by hand, pipe a fixture event into it, for example
`Get-Content event.json | target\release\Sessionmux.exe --notify-hook`. Unit tests for the
settings edits work only on fixture `settings.json` files under temporary folders; never point
them, or a manual test, at your real `%USERPROFILE%\.claude\settings.json`.

### Live check

`src/livetest.rs` runs against the real machine: it focuses Claude windows, copies one session and
removes the copy again. It is ignored by default and needs an explicit opt-in:

```powershell
$env:SESSIONMUX_LIVE = '1'; cargo test livetest -- --ignored --nocapture
```

Set `SESSIONMUX_LIVE_KEEP=1` as well to keep the copied session. Only run it on a machine where that
is acceptable.

## Installer

```powershell
powershell -ExecutionPolicy Bypass -File installer\build.ps1
```

This builds the release exe, stages `Sessionmux.exe`, `README.md`, `LICENSE.txt` (a copy of
`LICENSE`) and `THIRD-PARTY.md`, compiles `installer\sessionmux.nsi` and writes
`dist\Sessionmux-Setup-<version>.exe`, printing its size and SHA-256. Both the exe and the setup
exe are checked with `scripts\check-binary-privacy.ps1` (absolute paths, `.pdb` references, the
build machine's user, computer and domain names, e-mail domains, time-zone names, the maintainer's
personal name); so are the staged files, the installer script and an uncompressed build of the
installer, because the shipped setup exe is compressed and its payload cannot be searched. Any hit
stops the build. Extra words that must never ship but should not be published either go into the
git-ignored `scripts\private-terms.txt` (one per line) or `$env:SESSIONMUX_PRIVACY_TERMS`
(separated by `;`; CI reads it from a repository secret of the same name). You can run the check on any file yourself:
`.\scripts\check-binary-privacy.ps1 target\release\Sessionmux.exe`. Options: `-SkipBuild`
(package the existing exe), `-Makensis <path>`, and `-Sign` / `-TimestampUrl <url>` for optional
code signing (see [MAINTAINING.md](MAINTAINING.md)). `makensis` is found through `-Makensis`,
`$env:MAKENSIS`, `PATH`, or a standard NSIS install in Program Files.

## Building on a Mac

The macOS app is a preview built from the `macos` branch. Platform code lives behind the platform
layer in `src/platform/` (one trait per concern in `traits/`, implemented in `windows/` and
`macos/`); everything else is shared. A change to shared code must keep both builds compiling.

**Setup**

- macOS 13 or later (Apple silicon or Intel).
- The Xcode Command Line Tools (`xcode-select --install`), which provide the linker, `lipo`,
  `codesign` and the macOS SDK.
- Rust stable from [rustup](https://rustup.rs) with `rustfmt` and `clippy`, and both Apple targets
  for the universal app: `rustup target add aarch64-apple-darwin x86_64-apple-darwin`.

**Build and test**

```sh
cargo build --release          # target/release/Sessionmux (a bare program, for development)
cargo test
SESSIONMUX_SMOKE_FIXTURE=1 cargo run --release -- --smoke
```

The smoke check works as on Windows: demo data in `smoke/userData`, captures in `smoke/*.png`,
one JSON line and exit code 0 or 1. It never touches the real link handler, login items or your
Claude data.

A bare `target/release/Sessionmux` runs the floating button, the menu bar icon and the panel, but
the parts that need an app bundle (sign-in routing, Open at login, a stable identity for the
Automation and Accessibility permissions) are only available in `Sessionmux.app`. Hooks installed
from a development build record that build's path; remove them again with **Settings** or
`--remove-hooks` before you delete the build.

**The app bundle**

```sh
sh scripts/bundle-macos.sh <version> dist-macos
```

builds both architectures with the release settings (remapped paths, build date from the commit,
`MACOSX_DEPLOYMENT_TARGET=13.0`), joins them into one universal program, writes
`Sessionmux.app` with the Info.plist from `packaging/macos/Info.plist.in` and the icon from
`assets/macos/Sessionmux.iconset`, signs it ad hoc, and writes
`Sessionmux-<version>-macOS.dmg`, `Sessionmux-<version>-macOS.zip`, the fixed-name copies
`Sessionmux-macOS.dmg` and `Sessionmux-macOS.zip`, `BUILDINFO-macOS.txt` and `SHA256SUMS-macos`
into `dist-macos/`. It then runs the bundled program's smoke check and checks the disk image and
the zip. The script header lists every step. Run the privacy scan on the result with
`pwsh ./scripts/check-binary-privacy.ps1 dist-macos/Sessionmux.app/Contents/MacOS` (PowerShell 7
for macOS).

The icon set and the menu bar template images in `assets/macos/` are drawn on Windows by
`scripts/make-icons.ps1` and committed, so the Mac build needs no image tools.

**Other macOS checks**

- Tests that need a real Claude or a real handler change are ignored by default, as on Windows.
- `scripts/macos-two-user.sh` checks that two macOS users each get their own instance lock and
  socket. It creates and deletes a temporary user account with `sudo`, so it runs only in GitHub
  Actions unless you set `SESSIONMUX_TWO_USER_ALLOW=1` on a Mac where that is acceptable.
- `scripts/macos-probe.sh` (the manual `macos-probe` workflow) records how Claude Desktop behaves on
  GitHub's macOS machines: two data folders at once, a second start on a running folder, and the
  `claude://` handler.

Without a Mac you can still check that macOS code compiles: `rustup target add
aarch64-apple-darwin` and `cargo check --target aarch64-apple-darwin` work as long as no build
script needs Apple's C compiler. CI on the `macos` branch, and on pull requests against it, also
runs the macOS jobs (clippy, tests, the bundle, the smoke check on Apple silicon and Intel, and the
two-user check).

## Style

- Formatting: `cargo fmt` (settings in `rustfmt.toml`, 120 columns). CI runs `cargo fmt --check`.
- Lints: `cargo clippy --all-targets -- -D warnings` must be clean. If a lint has to be allowed,
  allow that specific lint as narrowly as possible with a one-line reason.
- `cargo build --release` must finish without warnings.
- No `unwrap()`/`expect()` on fallible Win32 or I/O results in runtime paths: return a `Result`
  and log. Every `unsafe` block has a one-line `// SAFETY:` comment saying why it is sound.
- No new dependencies without discussing it in an issue first; the app aims to stay a single
  small process.
- Every module starts with a `//!` comment describing its API and behaviour. Comments describe
  what the code does and why, not its history.
- Every product identifier (names, file names, window classes, registry keys, links) lives in
  `src/brand.rs`.
- Every source file starts with the CPAL-1.0 header comment (copy it from any existing file).
- UI text and CSS follow [docs/design.md](docs/design.md): colours only from the tokens in
  `web/shared.css`, flat fills (no gradients), text contrast of at least 4.5:1 in both themes.
- Never remove or hide the attribution described in [docs/attribution.md](docs/attribution.md).
- Tests and the smoke run must never write to `%APPDATA%\Claude*`, `%LOCALAPPDATA%\Claude-Data` or
  `%USERPROFILE%\.claude`, and never launch, focus or close real Claude windows.
- Nothing personal in tracked files: no absolute paths with user names, account UUIDs, e-mail
  addresses or real session titles. Screenshots come only from `SESSIONMUX_SMOKE_FIXTURE=1`.

## Using AI assistants with this code

You are welcome to use AI assistants to read, explain and contribute to this code, under the same
review and the same rules as any other contribution. The license and the attribution described in
[docs/attribution.md](docs/attribution.md) apply in full to anything derived from this code,
however it was produced.

## Pull requests

1. Open an issue first for anything larger than a small fix, so the approach can be agreed.
2. Keep one change per pull request, with a clear title and a description of the behaviour change.
3. Before pushing, run:

   ```powershell
   cargo fmt --check
   cargo clippy --all-targets -- -D warnings
   cargo build --release
   cargo test
   target\release\Sessionmux.exe --smoke
   ```

4. Add or update tests for behaviour changes, and update `README.md`, `PRIVACY.md` or `docs/` when
   user-visible behaviour, stored data or the architecture changes.
5. Add a line under "Unreleased" in [CHANGELOG.md](CHANGELOG.md).
6. Attach the relevant smoke captures for UI changes.

CI runs the same checks on every pull request.

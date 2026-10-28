# Changelog

All notable changes to this project are documented in this file. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project uses
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [1.1.0-preview.1] - Unreleased

A preview of Sessionmux for macOS, published as a GitHub pre-release built from the `macos` branch.
The stable Windows release stays 1.0.0; this pre-release carries a Windows installer of the same
code as well.

### Added

- **macOS (preview):** `Sessionmux.app` for macOS 13 or later, one universal app for Apple silicon
  and Intel, shipped as `Sessionmux-macOS.dmg` and `Sessionmux-macOS.zip` (also with the version in
  the name). Signed ad hoc and not notarized: macOS asks you to confirm it once with
  **Open Anyway**. Built and tested automatically on GitHub's macOS machines, not yet verified on a
  real Mac; the README lists what still needs checking.
- The full feature set on macOS: floating button and mini tray, a menu bar icon instead of the
  tray icon (no Dock icon), the **⌃⇧Space** hotkey, the panel with **⌘1/2/3** tabs and **⌥↑ / ⌥↓**
  reordering, accounts in `~/Library/Application Support/Claude-<name>`, session move, copy and
  hand-over, plan limits, approve from the panel, found profile folders, MCP sharing, **Open at
  login** and **Show in Finder**.
- Sign-in routing on macOS: Sessionmux takes the `claude://` handler through Launch Services while
  an extra account signs in and passes the link to that account's Claude with an Apple Event
  (macOS asks for Automation once). Off by default on macOS 26.4 and later, where every handler
  change asks "Use / Keep"; e-mail code sign-in is recommended there.
- macOS settings: **Bring minimised Claude windows to the front** (opt-in, uses Accessibility),
  **Remove Sessionmux's changes** (hooks, link handler and login item, since a Mac has no
  uninstaller) and **Give sign-in links back to Claude** after a declined restore.
- Release assets for macOS with SHA-256 lines in `SHA256SUMS`, `BUILDINFO-macOS.txt` and the same
  GitHub build-provenance attestation as the Windows installer.

### Changed

- The platform-specific code moved behind a platform layer shared by the Windows and macOS builds.
  Windows behaviour, file formats and settings are unchanged.
- The panel is hosted with wry 0.57.
- The hook helpers talk to the app over a per-user Unix socket on macOS (the named pipe stays on
  Windows); the message format is the same, with one added `show` request that a second start on
  macOS sends to bring up the running app's panel.

## [1.0.0] - 2026-10-26

First public release: a multi-account switcher for Claude Desktop on Windows.

### Added

- Run two or more Claude accounts side by side: the Main install plus extra accounts, each with
  its own Claude data folder (`%APPDATA%\Claude-<name>`), sign-in, colour and optional label.
- Switch Claude accounts without logging out: one-click open or focus of any account, **Open all**,
  and closing an account's window.
- Floating button in two shapes (the active account's icon, or a mini tray with the first three
  accounts) that snaps to the screen edge, a tray icon with an account menu, and the
  **Ctrl+Shift+Space** hotkey.
- Move Claude Code sessions between accounts: **Move to** and **Copy to** on the Sessions tab, with
  transcript, scratch folder and project memory, and rollback on failure.
- Copy and move of a session while its tab is open in Claude: Sessionmux copies the session as it
  is at that moment, and the original of a moved open session stays open in its account until you
  close it there.
- **Hand over** a session to another account that is signed in and not limited, which opens
  that account's Claude window when it is closed; optionally automatic when an account hits its
  plan limit, recognised from Claude Code's own local messages, with no network access. An
  automatic hand-over never closes or restarts a Claude window; from the panel, an open window is
  restarted only when you allow it and no session is running there.
- Approve from the panel (optional, off by default): waiting permission requests of every account
  appear on the Sessions tab with **Allow** and **Deny**, through hook entries that Sessionmux adds
  to `%USERPROFILE%\.claude\settings.json` after showing them, backs up first and removes when the
  option is turned off or the app is uninstalled. Nothing is approved automatically; without an
  answer, Claude's own prompt decides.
- Google and Apple sign-in for a second Claude account: while an extra account signs in,
  `claude://` links go to that account's window and Claude's own handler is restored afterwards.
- Discovery of existing `Claude-*` profile folders, same sign-in detection, optional sharing of
  Main's MCP server configuration, Start with Windows, and System, Light and Dark themes.
- A 1.7 MB native Windows app: one process of about 4 MB while idle; the WebView2 panel exists only
  while it is open.
- Per-user one-click installer with a WebView2 check; the uninstaller restores the `claude://`
  handler and removes Sessionmux's hook entries.
- About card with the version, build date, links, the attribution required by the license and the
  non-affiliation notice.

[Unreleased]: https://github.com/joyal-droid/sessionmux/compare/v1.0.0...HEAD
[1.1.0-preview.1]: https://github.com/joyal-droid/sessionmux/releases/tag/v1.1.0-preview.1
[1.0.0]: https://github.com/joyal-droid/sessionmux/releases/tag/v1.0.0

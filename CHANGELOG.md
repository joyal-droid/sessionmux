# Changelog

All notable changes to this project are documented in this file. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project uses
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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
[1.0.0]: https://github.com/joyal-droid/sessionmux/releases/tag/v1.0.0

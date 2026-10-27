# Sessionmux — switch between multiple Claude Desktop accounts on Windows
> Sessionmux is an independent, unofficial open-source project. It is not affiliated with, endorsed by or sponsored by Anthropic, PBC. Claude is a trademark of Anthropic, PBC.

<div align="center">

<a href="https://github.com/joyal-droid/sessionmux/releases/latest/download/Sessionmux-Setup.exe"><img src="docs/download-button.png" width="360" alt="Download Sessionmux-Setup.exe for Windows"></a>

Windows 10 and 11, 64-bit · about 0.7 MB · installs for your user, no administrator rights

If Windows shows "Windows protected your PC", choose **More info**, then **Run anyway** ([why](#why-does-windows-show-a-smartscreen-warning)).

</div>

Sessionmux is a multi-account switcher for Claude Desktop. It lets you run two or more Claude
accounts side by side, each in its own window with its own sign-in, and switch Claude accounts
without logging out, from a floating button, the tray icon or a hotkey. It can also move Claude
Code sessions between accounts, hand a session over when an account hits its plan limit, let you
approve a waiting session's next step from its panel, and make Google and Apple sign-in work for
a second Claude account. It is a 1.7 MB native Windows app, free and open source.

[![CI](https://img.shields.io/github/actions/workflow/status/joyal-droid/sessionmux/ci.yml?branch=main&label=CI)](https://github.com/joyal-droid/sessionmux/actions/workflows/ci.yml) [![Latest release](https://img.shields.io/github/v/release/joyal-droid/sessionmux?label=release)](https://github.com/joyal-droid/sessionmux/releases/latest) [![Downloads](https://img.shields.io/github/downloads/joyal-droid/sessionmux/total?label=downloads)](https://github.com/joyal-droid/sessionmux/releases) [![License: CPAL-1.0](https://img.shields.io/badge/license-CPAL--1.0-blue)](https://github.com/joyal-droid/sessionmux/blob/main/LICENSE) [![Platform: Windows 10/11 x64](https://img.shields.io/badge/platform-Windows%2010%2F11%20x64-0078D6)](#install) [![winget: planned](https://img.shields.io/badge/winget-planned-lightgrey)](#roadmap)

![Sessionmux's mini tray floating above the Accounts panel, which lists the demo accounts Work, Personal and Client](docs/screenshots/hero.png)

⭐ If Sessionmux saves you a log-out, star the repo — it helps others find it.

## Why Sessionmux

Claude Desktop runs one account at a time. Switching means logging out and back in, which closes
the window you were working in and hides the Code tab's sessions of the account you left. People
who keep separate personal, work and client accounts end up doing this several times a day.

Sessionmux does this instead:

- **Keeps every account signed in.** Each extra account gets its own Claude data folder, so all of
  them can be open at the same time and none of them has to log out.
- **Switches in one click.** Click an account on the floating button or in the panel and its window
  comes to the front, or starts if it is closed.
- **Moves your work with you.** A Code-tab session can be moved or copied to another account,
  transcript included, and continued there.

## Features

- **Switch Claude Desktop accounts** from a floating button that stays above your windows: either
  a round icon in the active account's colour or a mini tray with your first three accounts. It
  snaps to the screen edge and remembers its position.
- **Open, focus or close any account** with one click; **Open all** starts every account at once.
- **Move or copy Claude Code sessions between accounts** on the Sessions tab, with the transcript,
  the scratch folder and the project memory, even while the session's tab is open in Claude. A
  closed session's original is removed only after the copy succeeded; the original of a session
  that is still open stays open in its account until you close it there.
- **Hand over a session** to another account, from a session's **…** menu or automatically when
  an account hits its plan limit: Sessionmux copies the session to the next signed-in account that
  is not limited and opens that account's Claude window if it is closed. An open window is never
  closed by an automatic hand-over; restart it to see the session.
- **Approve from the panel.** A Code-tab session that asks for permission shows up at the top of
  the Sessions tab with the tool and a summary of what it wants to do; choose **Allow** or **Deny**
  there, or answer in Claude as usual. Nothing is ever approved without you. Optional and off by
  default (see [Does Sessionmux change my Claude settings?](#does-sessionmux-change-my-claude-settings)).
- **Google and Apple sign-in for extra accounts** through sign-in routing (see
  [Signing in to extra accounts](#signing-in-to-extra-accounts)); e-mail sign-in works as well.
- **Find existing profiles.** `%APPDATA%\Claude-<name>` folders you created earlier are offered
  under **Found profile folders** and keep their sign-in when added.
- **Same sign-in detection.** Two accounts signed in to the same Claude account are both marked.
- **Share MCP servers from Main.** Optionally copies the Main account's MCP server configuration to
  the other accounts whenever it changes.
- **Tray icon and global hotkey.** **Ctrl+Shift+Space** opens or closes the panel from anywhere;
  the tray menu lists your accounts too.
- **Small and private.** One native process of about 4 MB while idle, no telemetry and no network
  connections of its own. The WebView2 panel exists only while it is open.
- **Start with Windows**, System / Light / Dark themes, names, colours and labels per account.

| On the desktop: mini tray | On the desktop: panel open |
| --- | --- |
| ![A soft violet and blue wallpaper with the Sessionmux mini tray at the bottom right, showing the demo accounts Work, Personal and Client](docs/screenshots/desktop-tray.png) | ![The Sessionmux Accounts panel open beside the mini tray on a soft violet and blue wallpaper, showing the demo accounts Work, Personal and Client](docs/screenshots/desktop-panel.png) |

| Accounts | Sessions | Settings |
| --- | --- | --- |
| ![Accounts tab with the demo accounts Work (the Main account), Personal and Client](docs/screenshots/panel-accounts.png) | ![Sessions tab listing two Code sessions of the Work account, with account chips and search](docs/screenshots/panel-sessions.png) | ![Settings tab with the General and Sign-in groups and their switches](docs/screenshots/panel-settings.png) |

| Floating button | Mini tray | Signing in an extra account |
| --- | --- | --- |
| ![Floating button in icon mode showing the active account's initials](docs/screenshots/bubble.png) | ![Floating button in mini tray mode with three account avatars](docs/screenshots/bubble-tray.png) | ![Accounts tab showing the notice for an account that is signing in, with a Cancel button](docs/screenshots/signin-notice.png) |

The screenshots show demo data in the dark theme.

## How it compares

| | Floating switcher | Session move / copy | Sign-in routing for Google / Apple | Footprint | Price | Open source |
| --- | --- | --- | --- | --- | --- | --- |
| **Sessionmux** (Windows 10/11) | Yes: floating button (icon or mini tray), plus tray icon and hotkey | Move and copy of Code-tab sessions between accounts | Yes: `claude://` links go to the account that is signing in | About 4 MB idle; 1.7 MB exe | Free | Yes, CPAL-1.0 |
| **Logging out and back in** (built into Claude Desktop) | No; one account at a time | No; you see the sessions of the signed-in account only | Not needed; one account at a time | None | Free | Not applicable |
| **[claude-desktop-switcher](https://github.com/matsumotory/claude-desktop-switcher)** (macOS) | Menu bar app; a floating button is not documented | Not documented | Not documented | Universal DMG about 7 MB | Not documented | Yes, MIT |
| **[Claude Sessions Pro](https://doubleclaude.com)** (macOS, Windows) | Menu bar (macOS) and tray icon (Windows); a floating button is not documented | Sessions from the main app are copied into a profile when you sign in to it; moving sessions between profiles is not documented | Google and e-mail sign-in documented; Apple not documented | Not documented | $15 one-time, up to 2 computers | No, commercial |

Each row describes only what that project documents publicly: the claude-desktop-switcher README
and the Claude Sessions Pro website and its
[AlternativeTo listing](https://alternativeto.net/software/claude-sessions-pro/about/), as read in
October 2026. Corrections are welcome as an issue or pull request.

## Install

**Requirements**

- Windows 10 or 11, 64-bit (x64).
- Claude Desktop, either the Microsoft Store version or the direct installer.
- The Microsoft Edge WebView2 Runtime. It ships with Windows 11 and most Windows 10 PCs; Setup
  tells you if it is missing and links to <https://developer.microsoft.com/microsoft-edge/webview2/>.

**Download**

1. Download [Sessionmux-Setup.exe](https://github.com/joyal-droid/sessionmux/releases/latest/download/Sessionmux-Setup.exe)
   (about 0.7 MB). That link always serves the newest release; the
   [releases page](https://github.com/joyal-droid/sessionmux/releases/latest) has the same file as
   `Sessionmux-Setup-<version>.exe` next to `SHA256SUMS`.
2. Optionally [verify the download](#verify-your-download).
3. Run the setup file. It installs for your Windows user only, without administrator rights, into
   `%LOCALAPPDATA%\Programs\Sessionmux`, adds Start menu and desktop shortcuts and starts Sessionmux.

**SmartScreen.** Windows SmartScreen trusts files by their download history and by code signing.
A new open-source build that is not signed yet has neither, so Windows may show "Windows protected
your PC". Choose **More info**, check that the file name matches what you downloaded (and the
SHA-256, if you like), then **Run anyway**.

**Upgrading.** Run a newer setup file. It closes the running Sessionmux first and keeps your
accounts and settings.

**winget.** A `winget` package (`Sessionmux.Sessionmux`) is planned after the first release.

### Verify your download

Compare the SHA-256 of the setup file with its line in `SHA256SUMS`:

```powershell
Get-FileHash .\Sessionmux-Setup-<version>.exe -Algorithm SHA256
```

With the [GitHub CLI](https://cli.github.com) you can also check that the file was built by this
repository's release workflow on GitHub Actions (there is no signing key; GitHub records the build
provenance):

```powershell
gh attestation verify Sessionmux-Setup-<version>.exe --repo joyal-droid/sessionmux --signer-workflow joyal-droid/sessionmux/.github/workflows/release.yml
```

[SECURITY.md](SECURITY.md#verifying-a-download) explains both checks and how to rebuild the exe
from source and compare it.

## Quick start

1. **Start Sessionmux.** Your existing Claude Desktop install appears on the **Accounts** tab as
   the **Main** account, and the floating button appears at the edge of the screen.
2. **Add an account.** Click **Add account**, type a name such as "Work", pick a colour and click
   **Add & open**. A new Claude window opens; sign in there with Google, Apple or e-mail.
3. **Switch.** Click an account on the floating button or in the panel (**Ctrl+Shift+Space**) to
   bring its window to the front. To take a session along, open the **Sessions** tab, pick the
   account it is in and choose **Move to** or **Copy to**.

More detail: click a row's **…** menu to rename, recolour, close or remove an account; drag the
handle or press **Alt+↑ / Alt+↓** to reorder (the mini tray shows the first three). Settings are
under **Settings**: Start with Windows, Show floating bubble, Share MCP servers from Main, sign-in
routing, Theme, Floating button shape, and the two session options described under
[Sessions](#sessions).

## How it works

Every extra account is a separate Claude Desktop data folder, `%APPDATA%\Claude-<name>`, and
Sessionmux starts Claude with `--user-data-dir` pointing at it, so each account keeps its own
sign-in, chats and settings. Sessionmux itself is one Rust process: a Direct2D floating button, a
tray icon, a hotkey and a check of the running Claude windows every 3 seconds. The panel is a
WebView2 page that is created when you open it and destroyed 30 seconds after it closes, which is
why the idle footprint stays around 4 MB.

A session move copies the session record with new internal IDs, its transcript, its scratch folder
and the project memory to the other account, then removes the original record only if everything
succeeded and the session is not open in Claude. Project folders are never moved or copied. The
optional approval and limit features use Claude Code's own hooks: Claude Code runs
`Sessionmux.exe --approve-hook` or `--notify-hook`, which pass the event to the running app over a
pipe that only your Windows user can open. Details: [docs/architecture.md](docs/architecture.md).

## Sessions

The **Sessions** tab lists the Code-tab sessions of each account, with a **Running** badge on the
ones that are open in Claude.

- **Move to and Copy to** copy the session record with new internal IDs, its transcript, its
  scratch folder and the project memory to another account. The target account must be signed in
  and have opened the Code tab once; restart its Claude window to see the session there.
- **Even while the tab is open.** Copy and move also work on a session that is open in Claude.
  Sessionmux copies it as it is at that moment, so anything Claude is still writing will not be in
  the copy. When you move an open session, the original stays open in its account (Claude still
  owns its files) until you close it there; only a closed session is removed from the source, and
  only after the copy succeeded. Deleting a session still needs it closed.
- **Hand over** (in a session's **…** menu) copies the session to another account and shows it
  there. Sessionmux picks the first account in panel order that is signed in, has opened the Code
  tab and has not hit its limit, and opens that account's Claude window if it is closed. Claude
  lists sessions only when it starts, so an open window must restart to show the copy: leave
  the confirm dialog's restart option ticked and Sessionmux restarts it, but only when no
  session is running there at that moment; otherwise the window stays open and you restart it
  when it suits you. If every other account is limited, it tells you when the earliest one
  resets.
- **Automatic hand-over.** With **Hand sessions over automatically when an account hits its limit**
  on, an account that hits its plan limit hands over its most recently active open session: one
  session per limit, and never while that session waits for your approval. It never closes,
  restarts or interrupts a Claude window: it opens the target's window when it is closed, and
  otherwise tells you to restart it to see the session. A limited account shows
  **Limited** with the reset time Claude gave, for example "Limited · resets 5:20pm".
- **Waiting for your approval.** With **Show waiting sessions and approve from Sessionmux** on, a
  permission request from any account appears at the top of the tab: the account, the session, the
  folder, the tool and a summary of what it wants to do, with **Deny**, **Allow** and
  **Open in Claude**. **Allow** becomes clickable only after a short moment and is never the
  default button, so a stray Enter approves nothing. Answering in Claude instead works as before
  and removes the card. Sessions that wait for you are marked **Waiting** or **Needs you**.

Both options are off by default. They rely on hook entries that Sessionmux adds to Claude Code's
settings only when you turn approval on; see
[Does Sessionmux change my Claude settings?](#does-sessionmux-change-my-claude-settings).

## Signing in to extra accounts

Google and Apple sign-in finish in the browser with an "Open Claude" (`claude://`) link, and Windows
hands every such link to the Main account. While an account that Sessionmux opened is waiting to
sign in, Sessionmux briefly becomes the handler for `claude://` links and passes each one to that
account's window; a notice with **Cancel** is shown meanwhile. Claude's own handler is restored as
soon as the account is signed in, on cancel, after 15 minutes, when the window closes or when
Sessionmux quits.

You can turn this off in **Settings > Sign-in**; extra accounts then sign in with **Continue with
email**. If your browser is already signed in to claude.ai as someone else, sign out there first
or open the sign-in link in a private window. The full design is in
[docs/sign-in-routing.md](docs/sign-in-routing.md).

## Privacy

Sessionmux collects nothing: no telemetry, no analytics, no accounts, and no network connections of
its own. It reads Claude Desktop's local files only to list your accounts and sessions and to do
what you ask. What it stores and reads, and where, is listed in [PRIVACY.md](PRIVACY.md). The log
is `%APPDATA%\Sessionmux\sessionmux.log` (kept under 1 MB); attach it when reporting a problem.

## FAQ

### How do I use two Claude accounts on one PC?

Install Sessionmux, click **Add account** and sign in to the second account in the window that
opens. Both accounts then run side by side in their own windows, and you switch between them from
the floating button, the tray icon or **Ctrl+Shift+Space**. Neither account has to log out.

### Can I move a Claude Code session to another account?

Yes. On the **Sessions** tab, pick the account the session is in and choose **Move to** or
**Copy to**, then the target account, or choose **Hand over** to let Sessionmux pick an account
that is not limited and open its window for you (an open window is restarted only when you allow
it and no session is running there). The session may be open in Claude: Sessionmux
copies it as it is at that moment, and the original of a moved session that is still open stays
open in its account until you close it there. The target account must be signed in and have
opened the Code tab once; restart the target's Claude window to see the session there.

### Does Google sign-in work for a second Claude Desktop account?

Yes, with sign-in routing, which is on by default. Sessionmux sends the browser's `claude://` link
to the account that is signing in instead of the Main account, and puts Claude's own link handler
back afterwards. Apple sign-in works the same way, and e-mail sign-in needs no routing at all.

### Is this allowed by Anthropic's terms?

Sessionmux only manages separate Claude accounts that you own, each with its own sign-in, on your
own PC; it does not change how Claude Desktop or your accounts work. You are responsible for following
Anthropic's terms for each of your accounts, for example the
[Consumer Terms](https://www.anthropic.com/legal/consumer-terms) or the
[Commercial Terms](https://www.anthropic.com/legal/commercial-terms). Sessionmux is not affiliated
with Anthropic.

### Does it work with the Microsoft Store version of Claude Desktop?

Yes. Sessionmux finds both the Microsoft Store package and the direct installer. The Main account
uses Claude's normal data folder, and extra accounts use their own `%APPDATA%\Claude-<name>` folders
with either version.

### Does it work on macOS or Linux?

No, Sessionmux is Windows only. It is built directly on Windows features (Win32 windows, Direct2D,
the WebView2 runtime and the per-user registry for the `claude://` handler), so it cannot run on
other systems. A macOS feasibility study is on the [roadmap](#roadmap).

### Does Sessionmux see my chats or send data anywhere?

It sends nothing anywhere and makes no network connections of its own. It reads Claude's local
folders only to list accounts and sessions. It reads a session's transcript file to copy it when
you move, copy or hand over that session, and the last lines of an open session's transcript to
notice a limit message. With approval from the panel on, the panel shows the tool name and a short
summary of what a waiting session wants to do; that stays on your PC and is not stored. Your
sign-in tokens are never read, used, copied or sent. See [PRIVACY.md](PRIVACY.md).

### Does Sessionmux change my Claude settings?

Only if you turn on **Show waiting sessions and approve from Sessionmux** in Settings. A dialog
first shows the exact entries. Sessionmux then copies `%USERPROFILE%\.claude\settings.json` to
`settings.json.sessionmux-bak` next to it and adds three hook entries that run `Sessionmux.exe`
with `--approve-hook` or `--notify-hook`; nothing else in the file changes, and project settings
files are never touched. Claude Code's settings are shared by all accounts, so the entries cover
every account. Turning the option off, or uninstalling Sessionmux, removes exactly those entries.
If your settings turn hooks off (`disableAllHooks` or `allowManagedHooksOnly`), the dialog says so.
Details are in [PRIVACY.md](PRIVACY.md#local-hooks-optional).

### How does it know an account hit its limit?

From Claude Code's own local records: the limit message of a turn that ended on the limit and
Claude Code's notice that work is paused on a usage limit (both passed on by the hook entries),
and the last lines of the open session's transcript, which hold the same message. Sessionmux makes
no network connections and never reads sign-in tokens, so it has no usage percentages; it notices
the limit when Claude reports it. The reset time is the one in Claude's message; when it is
missing, the account counts as limited for 5 hours (a week for a weekly limit) or until a newer
signal clears it.

### How do I uninstall it and what stays behind?

Use **Windows Settings > Apps > Installed apps > Sessionmux > Uninstall**. It removes the program,
its shortcuts, the Start with Windows entry and its uninstall entry, removes Sessionmux's hook
entries from `%USERPROFILE%\.claude\settings.json`, and ends any active sign-in routing so
Claude's `claude://` handler is restored (Claude also re-registers it on its next start).
`%APPDATA%\Sessionmux` (account list, log and WebView2 cache), the settings backup
`settings.json.sessionmux-bak` and every Claude data folder are kept; delete them yourself if you
no longer need them.

### Why does Windows show a SmartScreen warning?

SmartScreen warns about programs that are new and not code-signed, which is true of the first
releases of a small open-source project. Choose **More info**, then **Run anyway**, after checking
the download against `SHA256SUMS`. Signed builds are on the [roadmap](#roadmap).

### What does CPAL-1.0 mean for forks?

You may use, change and redistribute Sessionmux, also commercially. Modified source files stay under
CPAL-1.0, and source must be available when you distribute or deploy a modified version. Every fork
must keep the "Powered by Sessionmux" attribution and the Sessionmux mark visible in its interface;
[docs/attribution.md](docs/attribution.md) lists the exact places.

## Roadmap

- Signed release builds, so SmartScreen no longer warns.
- A `winget` package (`Sessionmux.Sessionmux`) after the first release.
- Choosing the panel hotkey in Settings (it is fixed to **Ctrl+Shift+Space** today).
- A feasibility study for macOS.

Ideas and requests are welcome in [Discussions](https://github.com/joyal-droid/sessionmux/discussions) or
as a [feature request](https://github.com/joyal-droid/sessionmux/issues/new/choose).

## Contributing

Contributions are welcome. Build from source with:

```powershell
cargo build --release      # target\release\Sessionmux.exe
cargo test
```

Setup, the smoke check, the installer build and the pull request rules are in
[CONTRIBUTING.md](CONTRIBUTING.md); please follow the [Code of Conduct](CODE_OF_CONDUCT.md). Report
security problems privately as described in [SECURITY.md](SECURITY.md). Changes are listed in
[CHANGELOG.md](CHANGELOG.md).

## Attribution and license

Sessionmux is licensed under the [Common Public Attribution License 1.0](LICENSE) (CPAL-1.0,
OSI-approved). Section 14 of that license requires every copy and derivative to keep "Powered by
Sessionmux · Copyright (c) 2026 joyal-droid · https://github.com/joyal-droid/sessionmux" and the Sessionmux
mark visible in the user interface, as described in [docs/attribution.md](docs/attribution.md).

Third-party components and their licenses are listed in [THIRD-PARTY.md](THIRD-PARTY.md).

## Disclaimer

Sessionmux is an independent, unofficial open-source project. It is not affiliated with, endorsed by
or sponsored by Anthropic, PBC. Claude is a trademark of Anthropic, PBC. Microsoft Store, Windows and
WebView2 are trademarks of Microsoft Corporation.

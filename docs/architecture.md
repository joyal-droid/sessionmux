# Architecture

Sessionmux is a single native process written in Rust (edition 2021) for Windows
(`x86_64-pc-windows-msvc`) and, as a preview, macOS (`aarch64-apple-darwin` and
`x86_64-apple-darwin`, joined into one universal app). Most of the code is shared; everything that
talks to the operating system sits behind the [platform layer](#platform-layer). On Windows the
floating bubble is a layered window drawn with Direct2D and the panel is a WebView2 page hosted
with `wry`; on macOS they are an AppKit panel drawn with Core Graphics and a WebKit page hosted
with the same `wry` (see [macOS](#macos)). The panel exists only while it is open. There is no
PowerShell, no WMI, no shell scripting and no network code.

The process model, data layout and figures below describe Windows unless a line says otherwise.

## Process model

```
Sessionmux.exe (one process, GUI subsystem)
 ├─ UI thread: Win32 message loop
 │   ├─ hidden message-only window  "SessionmuxMsgWindow"   (cross-thread tasks, single instance)
 │   ├─ bubble window               "SessionmuxBubble"      (always; layered, Direct2D)
 │   ├─ panel window                "SessionmuxPanel"       (on demand; hosts WebView2)
 │   ├─ tray icon                   owner "SessionmuxTrayOwner"
 │   ├─ global hotkey               Ctrl+Shift+Space by default
 │   └─ timers                      panel destroy (30 s), pin (10 s), startup trim (2.5 s), sign-in (2 s)
 ├─ poll thread: enumerates Claude processes every 3 s, posts AccountsChanged only on a diff
 ├─ pipe threads: accept hook helpers on the per-user pipe, one instance and reader per client
 └─ short-lived worker threads: launching, waiting for windows, file copies, session transfer
WebView2 processes (only while the panel exists)
Sessionmux.exe --notify-hook / --approve-hook (started by Claude Code per hook event; no UI)
```

- **UI thread.** All state lives in `AppState` inside a thread-local `Rc<RefCell<>>`. Windows,
  the tray icon, the hotkey and every panel IPC call are handled here.
- **Marshalling.** Other threads never touch UI state. They hand a boxed closure to the event
  loop's `post`, which on Windows posts `WM_SESSIONMUX_TASK` to the message-only window (on macOS
  it goes to the main dispatch queue); the UI thread runs it.
- **Single instance.** A named mutex `Local\Sessionmux.SingleInstance`. A second launch finds the
  first instance's message window and posts `WM_SESSIONMUX_SHOW_PANEL`, then exits.
- **Panel lifecycle.** Created on first open, hidden on blur (unless pinned, at most 10 s) and on
  Esc, and **destroyed 30 s after hiding** so the WebView2 processes exit. Re-opening recreates it:
  the native window paints at once and the page follows in roughly 0.6 to 1 s while WebView2 starts.
- **Startup trim.** 2.5 s after start the working set is trimmed, so the idle footprint reflects
  what the bubble actually needs.

### Command line

| Arguments | Behaviour |
| --- | --- |
| *(none)* | normal start, panel shown |
| `--hidden` | start without showing the panel (used by Start with Windows) |
| `--smoke` | headless self-check (see [CONTRIBUTING.md](../CONTRIBUTING.md#smoke-check)) |
| `--relay <url>`, `--relay=<url>`, bare `claude://...` | forward one sign-in link and exit; no UI, runs before the single-instance check |
| `--end-relay` | restore the `claude://` handler and exit (called by the uninstaller on Windows; run by hand on macOS) |
| `--notify-hook` | read one `Notification` or `StopFailure` hook event from stdin, pass it to the running app and exit 0 without output |
| `--approve-hook` | read one `PermissionRequest` hook event from stdin, wait for the user's decision in the panel and print it, or exit 0 without output |
| `--remove-hooks` | remove Sessionmux's entries from `%USERPROFILE%\.claude\settings.json` (`~/.claude/settings.json`) and exit (called by the uninstaller on Windows) |

The hook words are matched exactly, like `--relay`, and run before the single-instance check,
COM and DPI setup; any other argument shape with a hook word exits 0 at once.

## Modules

| Module | Responsibility |
| --- | --- |
| `main.rs` | argument parsing, relay and hook fast paths, single instance, platform setup, event loop |
| `platform` | the operating-system layer: shared types, one trait per concern in `traits/`, the Windows backend in `windows/` and the macOS backend in `macos/` (see [Platform layer](#platform-layer)) |
| `hookhelper.rs` | the `--notify-hook`, `--approve-hook` and `--remove-hooks` helpers |
| `app.rs` | `AppState` and all orchestration: IPC handlers and their validation, polling and diffing, active-account logic, bubble/tray/hotkey events, panel timers, sign-in routing, quit |
| `brand.rs` | every product identifier: names, file names, window classes, mutex, registry keys, links, license name, disclaimer, build date |
| `smoke.rs` | `--smoke` self-check and screenshot capture |
| `livetest.rs` | opt-in end-to-end test against the real machine |
| `core::claude_app` | locating Claude Desktop (Store package or direct install), listing its processes and their `--user-data-dir`, focusing, launching, closing |
| `core::accounts` | the account store, validation rules, profile discovery, MCP config sharing, start with Windows |
| `core::sessions` | listing, transferring and deleting Code-tab sessions |
| `core::limits` | recognising a reached plan limit in hook messages and transcript tails, and the limited state per account |
| `core::hooks` | adding, checking and removing Sessionmux's entries in the user's Claude Code `settings.json` |
| `core::pipe` | the per-user named pipe between the hook helpers and the running app |
| `core::relay` | sign-in routing state and the `claude://` handler swap ([sign-in-routing.md](sign-in-routing.md)) |
| `ui::bubble` | the floating button: layered window, Direct2D/DirectWrite rendering, drag and snap, hit-testing |
| `ui::panel` | the panel window, the WebView2 host and the `sessionmux://app` asset protocol |
| `ui::ipc` | IPC request parsing (size limit 64 KB, strict shape) |
| `ui::tray` | notification-area icon and its menu |
| `ui::hotkey` | parsing and registering the global hotkey |
| `util::*` | logging (1 MB rotation), well-known paths, atomic JSON I/O, date helpers (the registry and Win32 helpers live in `platform::windows`) |
| `web/` | `panel.html`, `panel.css`, `panel.js`, the design tokens in `shared.css`, and `shim.js` (the `window.sessionmux` API) |

### Claude Desktop discovery

- **Store install:** the package is found under
  `HKCU\Software\Classes\Local Settings\Software\Microsoft\Windows\CurrentVersion\AppModel\Repository\Packages\Claude_*`
  (highest version). The exe is `<package root>\app\Claude.exe`, the AUMID `<family>!Claude`, and
  Main's data lives in `%LOCALAPPDATA%\Claude-Data\<family>` (with fallbacks).
- **Direct install:** `%LOCALAPPDATA%\AnthropicClaude\claude.exe`, Main's data in `%APPDATA%\Claude`.
- The result is cached for 60 s and re-resolved after Claude updates.
- **Processes:** a Toolhelp32 snapshot of `claude.exe` images under the install folder, skipping
  helper processes (`--type=`). The command line is read from the process environment block to get
  `--user-data-dir`, which maps a process to an account. The window is the first visible unowned
  top-level window of that process.
- **Launching:** Main through `shell:AppsFolder\<AUMID>` (Store) or the exe; extra accounts with
  `CreateProcessW("<exe>" --user-data-dir="<dir>")`.
- **Closing:** `WM_CLOSE` to the main windows, up to 3 s grace, then the process tree is terminated,
  restricted to Claude images and re-checked against creation times to avoid PID reuse.

### Session transfer

A Code-tab session consists of a record in the account's data folder and a transcript under
`%USERPROFILE%\.claude\projects\<slug of the working folder>\`. A session is *running* when a
live process's file in `%USERPROFILE%\.claude\sessions\` names it. A transfer:

1. refuses when the target is the source or the target has never opened the Code tab, and checks
   again whether the session is running (the list the panel shows may be up to 3 s old);
2. copies the record with new internal IDs, preserving every unknown key and rewriting only the
   keys that must change;
3. copies the transcript, the scratch folder for sessions without a project folder, and merges the
   project memory without overwriting existing entries;
4. for **move** of a session that is not running, removes the source record only after everything
   succeeded; the source transcript is always kept;
5. rolls back the target on any failure and reports warnings (`no-transcript`,
   `scratch-not-removed`, `source-not-removed`, `source-left-open`, `files-skipped:<n>`) or a
   reason (`not-found`, `same-account`, `target-never-opened-code`, `error`), together with
   `sourceRunning`, which the panel uses instead of its own list.

**Running sessions.** Claude Desktop keeps writing a running session's record and transcript, so
the copy is a snapshot taken with retries rather than a refusal:

- the record is read again, up to 5 times 50 ms apart, while it looks half-written;
- the transcript is read whole, then its size and modification time are checked again; if they
  changed during the copy it is copied again, at most 3 attempts, and the last copy is used;
- in the scratch and sibling folders, a file that another process holds open is retried 3 times
  and then skipped (`files-skipped:<n>`) instead of failing the whole transfer.

A **move** of a running session makes the same copy and then leaves the source record, scratch
folder and transcript untouched, because the Claude process still owns them; the result carries
`sourceRunning: true` and `source-left-open`, and the panel tells the user to close the original
there. A turn still in progress is not carried over. **Delete** keeps its guard and refuses a
running session (`running`), since a live process owns the files.

### Plan limits

`core::limits` decides from local, credential-free signals whether an account has hit its plan
limit:

- **`StopFailure` hook** (matcher `rate_limit`): fires when a turn ends on a rate-limit error; its
  error text is the primary signal.
- **`Notification` hook** with `quota_auto_resume_fired`, `quota_auto_resume_stale` or
  `quota_auto_resume_disabled`: Claude Code paused work on a usage limit (`fired` means it resumed
  and clears the state).
- **Transcript tail**, which needs no settings: the last 64 KB of a running session's transcript,
  of which the last 20 complete lines are checked for Claude Code's limit error line.

All patterns live in one table. A throttle ("not your usage limit", a client-side "Rate limit
reached") never counts, and a "You've used N% of your ... limit" line is only a warning. There is
no percentage trigger: the Code tab has no local usage figure, so the trigger is the limit itself.
The kind (session, weekly or model) and the reset text come from the message. An epoch gives the
expiry directly; a clock time without a zone (or in UTC or an `Etc/` zone) is read as its next
occurrence, on the named weekday for a weekly limit; a clock time in another zone is shown as
written and the state falls back to 5 hours (7 days for a weekly limit). The state is cleared when it expires or when the account reports a finished turn.
Sessions map to accounts through the hook's `session_id`, which is the record's `cliSessionId`;
the index is refreshed on the 3 s poll.

### Hooks and the pipe

`core::hooks` manages exactly three entries in `%USERPROFILE%\.claude\settings.json`, added only
after the user confirms a dialog that shows them:

```json
{
  "hooks": {
    "PermissionRequest": [{ "matcher": "", "hooks": [{ "type": "command", "command": "<exe>", "args": ["--approve-hook"], "timeout": 90 }] }],
    "Notification": [{ "matcher": "permission_prompt|idle_prompt|agent_needs_input|agent_completed|quota_auto_resume_fired|quota_auto_resume_stale|quota_auto_resume_disabled", "hooks": [{ "type": "command", "command": "<exe>", "args": ["--notify-hook"], "timeout": 5 }] }],
    "StopFailure": [{ "matcher": "rate_limit", "hooks": [{ "type": "command", "command": "<exe>", "args": ["--notify-hook"], "timeout": 5 }] }]
  }
}
```

When the hooks are added, the file is first copied to `settings.json.sessionmux-bak`, unless it
already holds an entry of Sessionmux's (so the backup never brings a hook back); removing the
hooks leaves the backup alone. The file is read with the order-preserving JSON reader and written
atomically. An entry is Sessionmux's when its `command`
is this exe and its `args` are one of the hook arguments; removal takes out only those and leaves
the rest of the file as it was. `disableAllHooks` and `allowManagedHooksOnly` are detected and
reported instead of assumed away. Project settings files are never touched.

`core::pipe` serves `\\.\pipe\Sessionmux.<user SID>` (the SID is looked up once), created with
the security descriptor `D:P(A;;GA;;;<SID>)` so only the current user can connect, in message mode,
newline-delimited JSON of at most 64 KB. Every client gets its own pipe instance
(`PIPE_UNLIMITED_INSTANCES`): the accept thread creates the next free instance as soon as a client
connects and before that client is served, so the name always has an instance waiting, and it
leaves its loop only when the server stops. An instance is disconnected and closed once its client
is done; a client that connects and leaves without sending anything costs nothing. Requests are
posted to the UI thread with `ui_thread::post`:

- `{kind: "notify", event, notification_type?, session_id, cwd, message, time}` (message at most
  2 KB) updates the waiting and limited state and gets no answer;
- `{kind: "approve", requestId, session_id, cwd, tool_name, mcp_server?, input_summary, time}`
  (a random 128-bit id; the summary is built per tool in the helper, at most 4 KB) stays open
  until the panel decides, the client disconnects (the user answered in Claude) or 90 s pass; a
  card whose client disconnected leaves the panel on the next 3 s poll. The decision goes back
  over the same connection only.

The helpers read at most 64 KB of stdin, require `hook_event_name` to match their mode and run
inside `catch_unwind`; on any doubt they exit 0 without output. On allow, `--approve-hook` prints
`{"hookSpecificOutput":{"hookEventName":"PermissionRequest","decision":{"behavior":"allow"}}}`; on
deny the same with `"behavior":"deny"` and the message "Denied from Sessionmux" (plus the user's
note). It never sends `updatedInput` or `updatedPermissions`. Each helper counts one time budget
from its start, shared by reading stdin, connecting and waiting: 85 s for `--approve-hook` (the
hook's time-out is 90 s), 4 s for `--notify-hook` (5 s); when it runs out the helper gives up
silently so Claude's own prompt decides. A busy pipe is waited for with `WaitNamedPipeW` for the
rest of the budget, and for at least 1 s. A helper that cannot connect writes one log line saying
so, without any of the hook input. Decisions are logged with session, tool name and outcome, never
the input. Waiting badges come from `Notification` events and clear on `agent_completed`, a
decision, transcript progress or after 30 minutes.

### Handover

`handOverSession {sessionId, fromId?, to?, restart?}`, from a session row's menu or automatically:

1. The target is `to`, or else the first account in panel order that is signed in, has opened the
   Code tab, is not the source and is not limited; with none the result is `no-target` with the
   earliest reset time.
2. The session is copied with the running-tolerant snapshot above.
3. The target shows it. A target with no Claude window running is opened (focused only from the
   panel). An open window is never closed by the automatic hand-over: the result carries
   `restartNeeded`, because Claude lists sessions only at start. From the panel, with `restart`
   true, the window is closed and reopened only when `Sessions::live_sessions_in` (the live
   registry matched against the target's record files, read at that moment) finds no session
   running there; a live `<pid>.json` that cannot be read even on a second try, or names no
   session, counts as running for every account. Otherwise the window stays open and the
   result carries `windowLeftOpen`. A window that fails to open or restart sets
   `restartNeeded` too, so the panel never reports a plain success for it.
4. One log line records the hand-over, without any transcript content.

With `autoHandover` on, a limit signal for an account hands over its most recently active running
session (the one named in the signal, when known): at most one session per signal, once per
account and reset time (30 minutes when the reset time is unknown), and never while the panel
holds an approval request for that session.

## Platform layer

`src/platform/mod.rs` holds the types both systems share (`Pid`, `Rect`, `WindowId`, the events
the app receives) and re-exports the backend of the target system, so shared code writes
`platform::process::...` or calls a trait method on `platform::Os` without `cfg` attributes. Any
other target stops the build with `compile_error!`.

| Traits | Concern | Windows | macOS |
| --- | --- | --- | --- |
| `traits/core.rs` | paths, processes, Claude discovery and launch, windows and focus, autostart, the link handler, shell and misc | registry, Toolhelp32, PEB command lines, `CreateProcessW`, `HKCU\...\Run` | Launch Services, `proc_listallpids` + `KERN_PROCARGS2`, `NSWorkspace`, `CGWindowListCopyWindowInfo`, `SMAppService` |
| `traits/ipc.rs` | the hook channel and the single-instance rule | named pipe, named mutex | Unix socket, `flock` on `instance.lock` |
| `traits/event.rs` | the UI event loop: timers, tray, hotkey, bubble, second launches, links, quit, posting work to the UI thread | Win32 message loop | `NSApplication` run loop, main dispatch queue |
| `traits/ui.rs` | bubble, tray, hotkey, gesture, panel window, smoke captures | layered window + Direct2D, notification-area icon, `RegisterHotKey`, WebView2 | `NSPanel` + Core Graphics, `NSStatusItem`, Carbon `RegisterEventHotKey`, WKWebView |

The bubble's scene (layout, colours, hit-testing) is built once in `ui::bubble` and drawn by each
backend, so both systems draw the same picture. The panel page, the IPC protocol, the account
store, session transfer, limits, hooks and the relay state machine are shared code.

## macOS

The macOS backend in `src/platform/macos/` uses AppKit and the system frameworks through the
`objc2` crates; nothing else is bundled.

- **App.** An accessory app (`LSUIElement`: no Dock icon, no visible menu bar) with a hidden main
  menu that carries the Edit commands, so the panel's text fields answer ⌘C, ⌘V and the rest. The
  bubble is a borderless, non-activating `NSPanel` at the status-window level that joins every
  Space; the menu bar icon is an `NSStatusItem` (click: panel, Control-click or right-click: menu).
  The hotkey (`⌃⇧Space` by default) is registered with Carbon's `RegisterEventHotKey`, which needs
  no permission.
- **Claude.** Claude Desktop is found by its bundle id `com.anthropic.claudefordesktop` (then
  `/Applications` and `~/Applications`); Main's data is `~/Library/Application Support/Claude`,
  extra accounts are `~/Library/Application Support/Claude-<name>`. Processes are listed with
  `proc_listallpids` and mapped to accounts by the `--user-data-dir` in their arguments
  (`sysctl` `KERN_PROCARGS2`). Accounts are started through `NSWorkspace openApplicationAtURL:`
  with a new instance and `--user-data-dir=<dir>`. A second start on a folder that already runs
  does not hand over to the running instance on macOS, so Sessionmux never starts an account that
  is running; it activates it instead, and with the opt-in Accessibility switch un-minimises and
  raises its window. Closing asks the app to quit (`NSRunningApplication terminate`), then ends
  what is left after 3 s.
- **IPC and single instance.** The hook helpers connect to
  `~/Library/Application Support/Sessionmux/hook.sock` (folder `0700`, socket `0600`, peer user
  checked with `getpeereid`; `sessionmux.sock` in the per-user temporary folder when the path is
  too long for a socket address) and send
  the same newline-delimited JSON as on Windows. `instance.lock` is held with `flock`; a second
  start sends `{"kind":"show"}` over the socket and exits, and a re-open from Finder or the Dock
  shows the panel. Both are per user, so two logged-in macOS users each run their own Sessionmux;
  the bundle deliberately does not set `LSMultipleInstancesProhibited`, which macOS would apply
  across users.
- **Sign-in routing.** See [sign-in-routing.md](sign-in-routing.md#macos).
- **Open at login.** `SMAppService.mainApp`; if registration fails, a LaunchAgent in
  `~/Library/LaunchAgents` that starts the app with `--hidden`. A launch as a login item starts
  hidden.
- **Panel.** wry hosts WKWebView with a non-persistent data store, the fraudulent-website check
  off and no developer tools. The asset origin is `sessionmux://app/`. The panel reads
  `getAppInfo().platform` to show macOS wording, key symbols and settings.
- **Running from the disk image.** A copy that runs translocated or from the disk image reports
  it (`getAppInfo().translocated`); the panel then asks the user to move the app to Applications
  and does not offer hooks, Open at login or sign-in routing.

## Data layout

| Data | Location |
| --- | --- |
| Account store and settings | `%APPDATA%\Sessionmux\sessionmux.json` |
| Log (rotated at 1 MB) | `%APPDATA%\Sessionmux\sessionmux.log` |
| Sign-in routing state (only while routing) | `%APPDATA%\Sessionmux\relay.json` |
| Panel WebView2 profile | `%APPDATA%\Sessionmux\WebView2\` |
| Start with Windows | `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`, value `Sessionmux` |
| Extra accounts' Claude data | `%APPDATA%\Claude-<name>\` |
| Main account's Claude data | `%LOCALAPPDATA%\Claude-Data\<family>\` (Store) or `%APPDATA%\Claude\` |
| Code-tab transcripts (shared by all accounts) | `%USERPROFILE%\.claude\projects\` |
| Claude Code user settings with Sessionmux's hook entries (only while approval is on) | `%USERPROFILE%\.claude\settings.json` |
| Backup of those settings before a change | `%USERPROFILE%\.claude\settings.json.sessionmux-bak` |

On macOS the same files live in `~/Library/Application Support/Sessionmux/` (`sessionmux.json`,
`sessionmux.log`, `relay.json`, plus `instance.lock` and `hook.sock`); the panel keeps no web data
on disk; Open at login is the app's login item (or
`~/Library/LaunchAgents/io.github.joyal-droid.sessionmux.plist`); Claude data folders are in
`~/Library/Application Support/`; Claude Code's files are in `~/.claude/`. The store's format
and keys are the same on both systems; `startWithWindows` holds the Open at login setting on
macOS.

The store is written atomically (temporary file and rename), as 2-space pretty JSON with a stable
key order:

```json
{
  "version": 2,
  "accounts": [
    { "id": "…", "name": "Work", "dataDir": "%APPDATA%\\Claude-Work", "color": "#5B9CF6", "label": null, "order": 1 }
  ],
  "settings": {
    "startWithWindows": false, "shareMcpConfig": true, "showBubble": true, "bubblePosition": null,
    "theme": "dark", "hotkey": "Ctrl+Shift+Space", "panelSide": "auto", "bubbleMode": "icon",
    "signInRouting": true
  },
  "ignoredDataDirs": []
}
```

(`dataDir` holds the expanded absolute path; the Main account has `dataDir: null`. The booleans
`autoHandover` and `approveFromPanel` are written only when they are on.) An unreadable
store is renamed to `sessionmux.json.corrupt-<timestamp>` and rebuilt. Every settings key is
validated; invalid values are ignored.

Account rules: names match `^[A-Za-z0-9][A-Za-z0-9 ._-]{0,31}$` after trimming and collapsing
spaces and are unique case-insensitively; a new account's folder is `%APPDATA%\Claude-<name>`
(suffixed `-2`, `-3`, … when taken). Data is deleted only for folders directly inside `%APPDATA%`
whose name starts with `Claude-`, never for Main and never while processes run from the folder.
Discovery offers `%APPDATA%\Claude-*` folders that contain a profile marker (`config.json`,
`Local State`, `Preferences`, `claude_desktop_config.json` or a `claude-code-sessions` folder),
excluding `*-NativeHost` folders, listed folders and ignored folders; it never adds anything by
itself.

## Panel and IPC

The panel page is embedded in the exe and served through the custom protocol `sessionmux://app`
(WebView2 exposes it as `https://sessionmux.app/`). `panel.html` carries a strict content security
policy (`default-src 'self'`). DevTools are disabled. Links are never navigated in the panel; they
open in the default browser through `openExternal`, which accepts only `http(s)` URLs.

`web/shim.js` is injected as the initialization script and defines `window.sessionmux`:

- **Transport:** `window.chrome.webview.postMessage(JSON.stringify({id, name, args}))`; the app
  answers with `window.__sessionmuxResolve(id, ok, payload)` and pushes events with
  `window.__sessionmuxEmit(name, payload)`. The page sends `{name: "__ready"}` once the DOM is ready.
- **Errors:** every failure resolves to `{ok: false, error, reason?}`.

| Group | Methods |
| --- | --- |
| Accounts | `listAccounts`, `openAccount`, `openAll`, `closeAccount`, `addAccount`, `renameAccount`, `removeAccount`, `reorderAccounts` |
| Discovery | `listDiscovered`, `adoptProfile`, `ignoreProfile` |
| Sessions | `listSessions`, `transferSession`, `deleteSession`, `openSessionFolder`, `handOverSession`, `listWaiting` |
| Approvals | `listApprovals`, `decideApproval` (`{id, decision: "allow" \| "deny", note?}`, answers `ok` or `gone`) |
| Hooks | `getHooksStatus`, `previewHooks`, `installHooks`, `removeHooks` |
| Settings | `getSettings` (settings plus read-only `appVersion` and `paths`), `setSettings` |
| Window | `togglePanel`, `hidePanel`, `pinPanel`, `quit` |
| App | `getAppInfo` (`{productName, version, buildDate, tagline, homeUrl, supportUrl, releasesUrl, sponsorUrl, licenseName, disclaimer}`), `openLicense`, `openExternal` |
| Sign-in | `getSignIn`, `cancelSignIn` |

| Event (`onX(cb)` returns an unsubscribe function) | Sent when |
| --- | --- |
| `onAccountsChanged` | the store changes or the 3 s poll sees a difference |
| `onActiveAccount` | the foreground Claude window maps to another account (else the last opened) |
| `onDiscoveredChanged` | the list of found profile folders changes |
| `onSettings` | settings change |
| `onShowTab` | the tray, hotkey or a second launch asks for a tab |
| `onSignInChanged` | sign-in routing starts or ends (with the reason) |

Validation happens in `app.rs`, not in the page: IDs must exist, session IDs match
`^local_[A-Za-z0-9-]{1,64}$`, strings are bounded, and requests over 64 KB are rejected.

## Bubble rendering

`WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE` popup with per-pixel alpha
through `UpdateLayeredWindow`, drawn into a premultiplied BGRA bitmap with a Direct2D WIC render
target and DirectWrite text. Every size scales with the monitor DPI (`GetDpiForWindow`,
`WM_DPICHANGED`). A press that moves at least 4 px is a drag; release snaps to the nearest left or
right screen edge with a 12 px margin. Visual details are in [design.md](design.md).

## Tamper response

`ui/gesture.rs` holds a cosmetic response to deliberate tampering. It plays when a debugger
attaches to the process (checked from 3 s after start, every 5 s on the poll thread, once per
attach; never in smoke mode), when the message window receives a forged task message (wrong key
or unknown id; an all-zero key and id is only logged), or when a `--relay` command line shows
argument injection: a quote or control character, an extra `--` argument, or a scheme other than
`claude:` (the relay process then posts `WM_SESSIONMUX_GESTURE`, `WM_APP + 3`, to the running
instance). An empty `--relay`, an over-long but well-formed link and every other shape the
registered `"<exe>" --relay "%1"` handler can produce are refused and logged only. A
click-through, never-activated layered window beside the bubble then shows a short pixel-art
animation drawn from constant cell grids, at most once per 10 s per kind, never while the app is
exiting, logged as `tamper: <reason>`. It blocks or changes nothing, and it cannot be disabled by
the user (there is no setting).

## Footprint

Measured on Windows 11 at 125 % scaling (the macOS app has not been measured on a real Mac yet;
its smoke run reports the same fields in CI):

| Measure | Value |
| --- | --- |
| Idle (bubble only, panel destroyed) | about 4 MB working set, 7.6 MB private; 1 process |
| Panel open | about 100 MB private commit across the app and 5 WebView2 processes |
| Cold start to bubble | about 130 to 170 ms |
| Panel re-open to content | about 0.6 to 0.9 s |
| Exe / installer / install folder | about 1.7 MB / 0.7 MB / 1.9 MB |

The smoke run reports the same figures (`rssIdleMb`, `privateIdleMb`, `privatePanelMb`,
`webviewProcs`, `panelReadyMs`) so regressions show up in CI logs.

## Build

On macOS `build.rs` only sets the build date; the app bundle (Info.plist, icon, ad-hoc signature,
universal binary) is assembled by `scripts/bundle-macos.sh` (see
[CONTRIBUTING.md](../CONTRIBUTING.md#building-on-a-mac)). On Windows, `build.rs` embeds `assets/icon.ico` (resource 1), `assets/tray.ico` (resource 2), the manifest
`assets/sessionmux.manifest` (PerMonitorV2 DPI awareness, Common Controls v6, `asInvoker`) and the
version resource, and sets `SESSIONMUX_BUILD_DATE` for `brand::build_date()`. The release profile
uses LTO, one codegen unit, `panic = "abort"` and stripped symbols. Dependencies: `wry`,
`raw-window-handle`, `serde`, `serde_json`, `uuid` on both systems; `windows` on Windows; the
`objc2` family, `block2`, `dispatch2` and `libc` on macOS; `tempfile` for tests; `winresource` for
the Windows build script.

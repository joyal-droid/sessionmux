# Sessionmux privacy notice

Short version: **Sessionmux collects nothing.** It has no telemetry, no analytics, no crash
reporting and no user accounts. Everything it knows stays on your PC.

## Network

Sessionmux itself makes no network connections. The only time anything goes online is when
you click a link in it (Website, Report a problem, the repository link, the license online,
the WebView2 download page); that link opens in your normal web browser, and the website's own privacy
policy applies there.

Sessionmux's panel is drawn by the Microsoft Edge WebView2 Runtime, a part of Windows that
Microsoft keeps up to date. The panel's pages are built into Sessionmux.exe and load from
the app itself, not from the internet. Microsoft's own services for Windows and WebView2
(for example updates) follow Microsoft's privacy statement.

## What Sessionmux stores, and where

| What | Where |
| --- | --- |
| Your account list (names, colours, labels, data-folder paths) and settings | `%APPDATA%\Sessionmux\sessionmux.json` |
| A log of what the app did (actions, folder paths, errors; no conversation contents, no passwords), kept under 1 MB | `%APPDATA%\Sessionmux\sessionmux.log` |
| The panel's WebView2 cache | `%APPDATA%\Sessionmux\WebView2\` |
| Start with Windows, when you turn it on | Registry: `HKEY_CURRENT_USER\Software\Microsoft\Windows\CurrentVersion\Run`, value `Sessionmux` |
| A separate Claude data folder for each extra account you add | `%APPDATA%\Claude-<name>\` |
| While an account signs in: which account, when it started, and Claude's previous `claude://` link handler (so it can be put back) | `%APPDATA%\Sessionmux\relay.json` (deleted when the sign-in ends) |
| Only while you have approval from the panel turned on: three hook entries that run `Sessionmux.exe` (see [Local hooks](#local-hooks-optional)) | `%USERPROFILE%\.claude\settings.json` |
| A copy of that file as it was before Sessionmux changed it | `%USERPROFILE%\.claude\settings.json.sessionmux-bak` |

Uninstalling Sessionmux removes its hook entries from `settings.json` and keeps everything else,
so a reinstall picks up where you left off. To remove the rest, delete the `%APPDATA%\Sessionmux`
folder and, if present, `settings.json.sessionmux-bak` after uninstalling. If a file named
`license.json` is present in that folder, Sessionmux deletes it; nothing else in that folder is removed.

## Sign-in routing

When you add an extra account and sign in with Google or Apple, your browser finishes the
sign-in by opening a `claude://` link. Windows normally hands every such link to your Main
Claude account. To get the link to the account that is signing in, Sessionmux changes one
per-user Windows setting, and only for as long as that sign-in is in progress:

- **What changes:** the handler for `claude://` links, the registry value
  `HKEY_CURRENT_USER\Software\Classes\claude\shell\open\command`. Sessionmux saves the
  value Claude wrote there, points it at `Sessionmux.exe --relay`, and puts the saved value back.
- **When:** only while an account that Sessionmux opened is waiting to sign in. It is put back
  as soon as that account is signed in, or when you cancel, after 15 minutes, when the
  account's window closes, when Sessionmux quits, or when it next starts (unless that account is
  still open and waiting to sign in, in which case routing continues). If Sessionmux was closed
  without quitting (a crash, a forced stop, signing out of Windows), the next `claude://` link
  goes to your main account and the handler is put back then. Claude Desktop also
  registers itself again every time it starts.
- **What is kept:** the saved handler value, the account's id, name and data folder, and the
  start time, in `%APPDATA%\Sessionmux\relay.json`. The file is deleted when the sign-in ends.
- **What Sessionmux does with a link:** it passes the link straight to the waiting account's
  Claude window on your PC. It does not store the link or send it anywhere, and its log records
  only the link's address without the sign-in code.

You can turn this off in Settings (**Route sign-in links to the account being signed in**);
Sessionmux then never takes over the handler (it only puts Claude's value back if an earlier
crash left Sessionmux registered).

## Local hooks (optional)

Off by default. When you turn on **Show waiting sessions and approve from Sessionmux** in
Settings, Sessionmux shows you the exact entries it is about to add and writes nothing until you
confirm:

- **Which file:** `%USERPROFILE%\.claude\settings.json`, Claude Code's user settings, which all
  your accounts share. Project settings files are never touched.
- **Backup:** before adding the entries, Sessionmux copies the file to
  `%USERPROFILE%\.claude\settings.json.sessionmux-bak`. The backup never contains Sessionmux's
  entries: a file that already has one is not backed up, and removing the entries leaves the
  backup as it was. Sessionmux does not delete the backup.
- **What the entries run:** three hook entries, each running `Sessionmux.exe` directly (no shell):
  `--approve-hook` when a session asks for permission, and `--notify-hook` when a session waits
  for you, finishes, or stops on or is paused by a usage limit. Nothing else in the file changes.
  If your settings turn hooks off (`disableAllHooks` or `allowManagedHooksOnly`), Sessionmux says
  so instead of pretending the option works.
- **What the helper receives:** Claude Code hands the event to the helper on your PC. The helper
  passes on the session id, the working folder, the tool name and a bounded summary of the tool's
  input (for example the command, the file path or the address; at most 4 KB) to the running
  Sessionmux, which shows it in the panel so you can decide; notifications carry Claude's message
  text, at most 2 KB. This travels over a named pipe that only your Windows user can open and is
  kept in memory only until it is answered or expires. The log records the session, the tool name
  and your decision, never the tool's input.
- **Nothing leaves the PC.** The helpers make no network connections either. The app never
  reads, uses or copies sign-in tokens, and the hook events carry none.
- **Removal:** turning the option off, or uninstalling Sessionmux, removes exactly the entries it
  added (recognised by the path of `Sessionmux.exe` and its arguments) and leaves the rest of the
  file as it was.

## What Sessionmux reads

Sessionmux reads Claude Desktop's local files **only to list your accounts and sessions and
to do what you ask**:

- each account's Claude data folder, to tell whether it is signed in (Sessionmux reads the
  account id and checks that a sign-in entry exists; it never uses, copies or sends your
  sign-in tokens), to count and list its Code sessions, and to copy MCP server settings
  when you turn that on;
- `%USERPROFILE%\.claude\` (Claude Code's session state and transcripts), to show which
  sessions are running, to move, copy or hand over a session when you ask, and to look for a
  limit message in the last 64 KB of an open session's transcript;
- `%USERPROFILE%\.claude\settings.json`, only to add, check or remove its own hook entries
  while you use approval from the panel;
- the list of running programs, to find Claude windows and bring the right one to the
  front.

Sessionmux does not read your conversations except to copy a session's transcript file when
you move, copy or hand over that session and to check the last lines of an open session's
transcript for a limit message, and it never sends any of it anywhere.

## Contact

Questions about this notice: open an issue at <https://github.com/joyal-droid/sessionmux/issues>.

Sessionmux is an independent, unofficial open-source project. It is not affiliated with, endorsed by
or sponsored by Anthropic, PBC. Claude is a trademark of Anthropic, PBC.

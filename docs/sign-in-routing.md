# Sign-in routing

How Sessionmux lets an extra Claude Desktop account sign in with Google or Apple, and why it needs
to touch the `claude://` protocol handler to do so. The sections up to [macOS](#macos) describe
Windows; the state machine in `core::relay` is shared, and the macOS section lists what differs.

## The problem

Choosing **Continue with Google** (or Apple) in a Claude window finishes the sign-in in the web
browser. Claude's desktop OAuth flow redirects to `https://claude.ai/desktop/callback`, and that
page asks the browser to open a `claude://…` link that carries the result back to the app.

On Windows that link is handled by a plain per-user registry value that Claude writes itself every
time it starts:

```
HKCU\Software\Classes\claude\shell\open\command  (Default) =
  "<Claude install folder>\app\Claude.exe" "%1"
```

The command has no `--user-data-dir`, and custom protocols have no `UserChoice` lock, so Windows
starts `Claude.exe "<url>"` with the **default** data folder. Claude's single-instance lock is keyed
by the data folder, so that launch forwards its arguments to the already running Main instance,
which is signed in already and just comes to the front. The extra account that opened the login
page never receives the link and stays signed out.

E-mail sign-in (typing a one-time code into the window) is not affected, because no link is
involved.

## The design

Sessionmux becomes the `claude://` handler **only while an account it opened is waiting to sign
in**, forwards each link to that account's running Claude instance, and restores the original
handler as soon as the sign-in completes or is abandoned. Because Claude re-registers its handler
on every start, even an interrupted relay heals on the next Claude launch.

### When routing starts

Routing starts when all of these hold:

- the setting `signInRouting` is on (default),
- Sessionmux opens or focuses an account that is not Main and not signed in (from **Open**,
  **Open all**, **Add account** or **Add** on a found profile),
- that account's window has appeared, plus 1.5 s so Claude has registered its own handler first.

If another account is already waiting, the newer one replaces it. Turning the setting on while such
an account is running starts routing for it.

### Begin, ensure, end

`core::relay` keeps the state in `<user data>\relay.json`:

```json
{
  "active": true,
  "accountId": "…",
  "accountName": "Work",
  "dataDir": "…\\Claude-Work",
  "since": "2026-10-26T09:00:00.000Z",
  "originalCommand": "\"…\\Claude.exe\" \"%1\""
}
```

- **Begin:** save the current handler command as `originalCommand` (unless it already points at
  Sessionmux), persist the state, then write `"<Sessionmux.exe>" --relay "%1"`.
- **Ensure** (every 2 s while active): if Claude re-registered itself, store its new command as
  `originalCommand` (so the newest Claude path is restored later) and write Sessionmux's command
  again. The same tick checks whether the account is now signed in (its `config.json` holds a
  token cache entry), whether its process is gone (or never appeared within 30 s), and whether
  15 minutes have passed.
- **End:** restore `originalCommand` only if the handler still points at Sessionmux, then delete
  `relay.json`. A saved command whose exe no longer exists is not restored; the located Claude exe
  is used instead (`"<exe>" "%1"`), or the `(Default)` value is deleted when Claude cannot be
  located.

Routing ends on sign-in, cancel (the notice's **Cancel** button), timeout (15 min), the account's
window closing, turning the setting off, quitting Sessionmux, and at the next start unless the
account is still running and still not signed in (then routing resumes). The `signin-changed`
event carries the reason: `signedIn`, `timeout`, `closed`, `cancelled`, `disabled` or `quit`.

### Forwarding a link (`--relay`)

`Sessionmux.exe --relay <url>` runs before the single-instance check, shows no UI and registers
nothing:

1. Accept only the exact shapes `--relay <url>`, `--relay=<url>` or a bare `claude://…` first
   argument; any extra argument is refused, so `%1` cannot smuggle in `--smoke` or `--hidden`.
2. Validate the URL: it starts with `claude://`, is at most 8192 characters, and contains no control
   characters and no `"`.
3. Decide the target: the waiting account's data folder when `relay.json` is active, not expired,
   and Sessionmux itself is running (its mutex exists). Otherwise the default Claude instance, and
   the handler is healed with **end**.
4. Start `"<Claude.exe>" [--user-data-dir="<dir>"] "<url>"`. Claude's single-instance lock for that
   data folder hands the link to the waiting window.
5. Log one line with the link's scheme, host and path, never its query string (which carries the
   sign-in code).

On any error the link still goes to the default Claude instance, so it is never swallowed.

`Sessionmux.exe --end-relay` runs **end** and exits. The uninstaller calls it after closing
Sessionmux and, as a fallback, deletes the handler's `(Default)` value if it still contains
`Sessionmux.exe` and `--relay`.

### Panel

- Accounts that are not signed in show **Sign in needed** (window open) or **Not signed in**
  (window closed); the bubble's status dot for a running, not-signed-in account uses the warning
  colour.
- While routing is active, the Accounts tab shows a notice ("Signing in {name} …") with a
  **Cancel** button; a toast confirms the outcome.
- When routing is off, the notice explains that Google and Apple links land in the Main account and
  offers **Turn on**.
- **Settings > Sign-in** has the toggle "Route sign-in links to the account being signed in".

### Related discovery rule

`%APPDATA%\Claude-NativeHost` is the folder of Claude's browser-extension native host, not a
profile, even though it matches `Claude-*`. Discovery therefore requires a profile marker
(`config.json`, `Local State`, `Preferences`, `claude_desktop_config.json` or a
`claude-code-sessions` folder) and skips every folder whose name ends in `-NativeHost`.

## Safety properties

- The real `claude` key is changed only while a sign-in is in progress, and only the
  `(Default)` value of `shell\open\command`.
- Restoring never overwrites a value that no longer points at Sessionmux (Claude may have
  re-registered in between).
- Smoke runs (and any run with an overridden user-data folder) never touch the registry; unit
  tests use a throw-away
  `HKCU\Software\Classes\sessionmux-test-<uuid>` key.
- Links are passed straight to the local Claude process; they are not stored, and the log omits
  their query string.

## macOS

The macOS app is a preview, and routing on a Mac has not been tried with a real sign-in yet.

**The problem on a Mac.** Launch Services opens a `claude://` link in the app registered for the
scheme, Claude, and delivers it as a get-URL Apple Event to a running Claude, which belongs to
whichever account macOS picks. A second start of Claude with `--user-data-dir` does not help: on
macOS a second start on a folder that already runs keeps running beside the first instead of
handing its arguments over (checked on GitHub's macOS 15 and 26 machines), so Sessionmux never
starts an account twice, and links are never sent that way.

**Taking and giving back the handler.** `platform::macos::url_handler` changes the handler with
`NSWorkspace setDefaultApplicationAtURL:toOpenURLsWithScheme:` and reads it back with
`URLForApplicationToOpenURL:`. `relay.json` (in `~/Library/Application Support/Sessionmux/`)
stores the previous handler as an app path instead of a command. Changes run on worker threads
only, never on the main thread, because the call may wait for the user.

- **macOS 13 to 26.3:** a change completes at once without a question (seen on macOS 15: take and
  restore in about 130 ms). Claude makes itself the handler again whenever one of its instances
  starts; routing takes it back once per sign-in.
- **macOS 26.4 and later:** every change shows a system "Use / Keep" question and completes only
  when the user answers. `changes_need_consent()` is true there, so the setting `signInRouting`
  defaults to off and the Add-account dialog recommends e-mail code sign-in. With routing on, a
  sign-in asks two or three times (Sessionmux takes the handler, Claude may claim it when it
  starts, Sessionmux gives it back), and routing never re-takes the handler after Claude claims
  it. A change with no answer yet is `pending`, not a failure; **Keep** is a clean refusal that is
  not retried. A declined restore sets `restoreDeclined`, and Settings then offers **Give sign-in
  links back to Claude**.

**Forwarding.** A link reaches Sessionmux as a get-URL Apple Event (installed before launch
finishes, so the link that launched the app is kept). The app validates it like `--relay` does
and sends it, as a new get-URL Apple Event addressed by process id (`typeKernelProcessID`), to the
waiting account's Claude process, starting that account first if it is not running. The first
send asks for Automation permission (`AEDeterminePermissionToAutomateTarget`, Info.plist
`NSAppleEventsUsageDescription`); when it is denied, the panel shows a notice with a button to the
Automation settings and recommends e-mail code sign-in. A link that arrives while no sign-in is
waiting goes to Main and gives the handler back to Claude if it still points at Sessionmux.

**Ending.** As on Windows, except that quitting and logging out never change the handler, since
a question could outlive the app: the next start ends the relay instead (or resumes it if the
account is still waiting). `--end-relay` does the same from Terminal, and **Remove Sessionmux's
changes** in Settings gives the links back as well.

**Safety.** The Info.plist declares `claude` with `LSHandlerRank` `Alternate`, so installing
Sessionmux never makes it the handler by itself. Smoke runs and tests never change the real
handler; the round-trip test uses a throw-away scheme and stand-in apps.

User-facing details are in [PRIVACY.md](../PRIVACY.md#sign-in-routing).

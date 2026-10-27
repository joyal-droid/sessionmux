// Copyright (c) 2026 joyal-droid. SPDX-License-Identifier: CPAL-1.0
//
// The contents of this file are subject to the Common Public Attribution License Version 1.0
// (the "License"); you may not use this file except in compliance with the License. You may
// obtain a copy of the License in the LICENSE file at the root of this repository or at
// https://opensource.org/license/cpal-1-0. The License is based on the Mozilla Public License
// Version 1.1 but Sections 14 and 15 have been added to cover use of software over a computer
// network and provide for limited attribution for the Original Developer. In addition, Exhibit A
// has been modified to be consistent with Exhibit B.
//
// Software distributed under the License is distributed on an "AS IS" basis, WITHOUT WARRANTY OF
// ANY KIND, either express or implied. See the License for the specific language governing rights
// and limitations under the License.
//
// The Original Code is Sessionmux. The Initial Developer of the Original Code is joyal-droid. All
// portions of the code written by joyal-droid are Copyright (c) 2026 joyal-droid. All Rights Reserved.
// Contributor(s): see the git history of this repository.
//
// Attribution (Exhibit B of the License): "Powered by Sessionmux", Copyright (c) 2026 joyal-droid,
// https://github.com/joyal-droid/sessionmux and the Sessionmux mark must stay visible in the user
// interface of every copy and derivative; see docs/attribution.md.

//! Sign-in routing for extra accounts.
//!
//! While an account Sessionmux opened is waiting to sign in, Sessionmux temporarily becomes the
//! per-user `claude://` handler (`HKCU\Software\Classes\claude\shell\open\command`), forwards each
//! link to that account's instance (`Sessionmux.exe --relay "%1"` → `"<Claude.exe>"
//! --user-data-dir="<dir>" "<url>"`) and restores the saved handler afterwards. The state lives in
//! `<user_data>\relay.json` so the short-lived `--relay` process can read it.
//!
//! Contract:
//! ```ignore
//! pub const PROTOCOL: &str = "claude"; pub const TIMEOUT_SECS: u64 = 900; pub const POLL_MS: u64 = 2000;
//! pub const FILE: &str = "relay.json";
//! pub struct RelayState { active, account_id, account_name, data_dir, since, original_command }
//! pub fn read_handler(protocol) -> Option<String>; pub fn write_handler(protocol, command) -> Result<(), String>;
//! pub fn our_command() -> String; pub fn points_at_us(command) -> bool; pub fn default_command(exe) -> String;
//! impl Relay { open(user_data), open_with(user_data, Option<protocol>), state(), begin(id, name, dir),
//!              ensure() -> Result<bool, String>, end(), expired(now_secs) }
//! pub fn validate_url(url) -> Result<String, String>; pub fn relay_target(Option<&RelayState>) -> Option<String>;
//! pub fn forward(url, exe, data_dir) -> Result<(), String>; pub fn redact(url) -> String; pub fn now_secs() -> u64;
//! pub fn signal_bad_link(); pub fn signal_bad_link_to(hwnd_msg) -> bool;
//! ```
//! Unit tests (`cfg(test)`) never touch the real `claude` key: `Relay::open` then has no
//! registry at all, and the registry tests use a throw-away `sessionmux-test-<uuid>` protocol.

use std::path::{Path, PathBuf};

use crate::brand;
use crate::core::claude_app;
use crate::util::{date, json, log, registry};

pub const PROTOCOL: &str = "claude";
pub const TIMEOUT_SECS: u64 = 15 * 60;
pub const POLL_MS: u64 = 2000;
/// `<user_data>\relay.json`.
pub const FILE: &str = brand::RELAY_FILE;
/// Longest link we forward (characters).
pub const MAX_URL_LEN: usize = 8192;

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RelayState {
    pub active: bool,
    #[serde(rename = "accountId")]
    pub account_id: String,
    #[serde(rename = "accountName")]
    pub account_name: String,
    /// The profile waiting to sign in (never Main).
    #[serde(rename = "dataDir")]
    pub data_dir: String,
    /// ISO-8601 UTC.
    pub since: String,
    /// The handler value that was there before us (None: there was none).
    #[serde(rename = "originalCommand")]
    pub original_command: Option<String>,
}

// ───────────────────────────────────────────────────────────── registry ──

fn command_key(protocol: &str) -> String {
    format!("Software\\Classes\\{protocol}\\shell\\open\\command")
}

/// `(Default)` of `HKCU\Software\Classes\<protocol>\shell\open\command`.
pub fn read_handler(protocol: &str) -> Option<String> {
    registry::read_default(&command_key(protocol))
}

/// Set that value, creating the key path when missing.
pub fn write_handler(protocol: &str, command: &str) -> Result<(), String> {
    registry::write_default(&command_key(protocol), command)
}

/// Full path of the running exe (GetModuleFileNameW; falls back to `std::env::current_exe`).
pub fn current_exe() -> String {
    use windows::Win32::System::LibraryLoader::GetModuleFileNameW;
    let mut buf = vec![0u16; 512];
    loop {
        // SAFETY: a None module selects the current exe; buf is a valid writable slice.
        let n = unsafe { GetModuleFileNameW(None, &mut buf) } as usize;
        if n == 0 {
            break;
        }
        if n < buf.len() {
            return String::from_utf16_lossy(&buf[..n]);
        }
        if buf.len() >= 32 * 1024 {
            break;
        }
        let len = buf.len() * 2;
        buf.resize(len, 0);
    }
    std::env::current_exe().map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|_| brand::EXE.to_string())
}

/// `"<full path of current exe>" --relay "%1"`.
pub fn our_command() -> String {
    format!("\"{}\" --relay \"%1\"", current_exe())
}

/// `"<exe>" "%1"` — what Claude itself registers; the fallback when nothing was saved.
pub fn default_command(exe: &str) -> String {
    format!("\"{}\" \"%1\"", exe)
}

/// argv[0] of a handler command line (quoted or bare).
fn command_exe(command: &str) -> &str {
    let c = command.trim_start();
    if let Some(rest) = c.strip_prefix('"') {
        rest.split('"').next().unwrap_or("")
    } else {
        c.split(char::is_whitespace).next().unwrap_or("")
    }
}

/// True when `command` runs an Sessionmux relay: its exe is the current exe or any `Sessionmux.exe`
/// (an older install location still counts — it must never be saved as the "original"), and it
/// carries `--relay`.
pub fn points_at_us(command: &str) -> bool {
    let exe = command_exe(command);
    if exe.is_empty() || !command.to_ascii_lowercase().contains("--relay") {
        return false;
    }
    let leaf = exe.rsplit(['\\', '/']).next().unwrap_or(exe);
    leaf.eq_ignore_ascii_case(brand::EXE) || crate::util::paths::same_path(exe, &current_exe())
}

// ───────────────────────────────────────────────────────────── state ──

/// The relay state of one Sessionmux user-data folder.
pub struct Relay {
    user_data: PathBuf,
    /// Protocol whose handler we manage; None = no registry at all (unit tests of other modules).
    protocol: Option<String>,
}

impl Relay {
    /// Relay for `user_data` managing the real `claude` handler (none under `cfg(test)`).
    pub fn open(user_data: &Path) -> Relay {
        Self::open_with(user_data, if cfg!(test) { None } else { Some(PROTOCOL) })
    }

    /// Full control: `protocol` None disables every registry access (relay.json only).
    pub fn open_with(user_data: &Path, protocol: Option<&str>) -> Relay {
        Relay { user_data: user_data.to_path_buf(), protocol: protocol.map(str::to_string) }
    }

    fn file(&self) -> PathBuf {
        self.user_data.join(FILE)
    }

    /// From relay.json; None when missing or unreadable (an unreadable file is kept as
    /// `relay.json.corrupt`).
    pub fn state(&self) -> Option<RelayState> {
        let file = self.file();
        if !file.exists() {
            return None;
        }
        match json::read::<RelayState>(&file) {
            Some(s) => Some(s),
            None => {
                let mut bad = file.clone().into_os_string();
                bad.push(".corrupt");
                let _ = std::fs::remove_file(&bad);
                let moved = std::fs::rename(&file, &bad).is_ok();
                log::warn(&format!("relay: {FILE} is unreadable (kept as .corrupt: {moved})"));
                None
            }
        }
    }

    fn persist(&self, st: &RelayState) -> Result<(), String> {
        json::write_atomic(&self.file(), st).map_err(|e| format!("could not write {FILE}: {e}"))
    }

    fn remove_file(&self) -> Result<(), String> {
        match std::fs::remove_file(self.file()) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(format!("could not delete {FILE}: {e}")),
        }
    }

    /// Save the current handler (unless it already points at us), persist state, write our command.
    pub fn begin(&self, account_id: &str, account_name: &str, data_dir: &str) -> Result<RelayState, String> {
        if data_dir.trim().is_empty() {
            return Err("relay: dataDir required".into());
        }
        let current = self.protocol.as_deref().and_then(read_handler);
        let original_command = match &current {
            // Already ours (a previous relay or a crash): keep whatever was saved before.
            Some(c) if points_at_us(c) => self.state().and_then(|s| s.original_command),
            other => other.clone(),
        };
        let st = RelayState {
            active: true,
            account_id: account_id.to_string(),
            account_name: account_name.to_string(),
            data_dir: data_dir.to_string(),
            since: date::iso_now(),
            original_command,
        };
        self.persist(&st)?;
        if let Some(p) = self.protocol.as_deref() {
            if let Err(e) = write_handler(p, &our_command()) {
                let _ = self.remove_file();
                return Err(format!("relay: {e}"));
            }
        }
        log::info(&format!("relay: routing sign-in links to {}", st.account_name));
        Ok(st)
    }

    /// Re-apply our command if Claude re-registered the handler (called every POLL_MS while
    /// active). The value Claude wrote becomes the one restored by [`Relay::end`].
    pub fn ensure(&self) -> Result<bool, String> {
        let Some(mut st) = self.state().filter(|s| s.active) else {
            return Ok(false);
        };
        let Some(p) = self.protocol.as_deref() else {
            return Ok(false);
        };
        let current = read_handler(p);
        if current.as_deref().is_some_and(points_at_us) {
            return Ok(false);
        }
        if current.is_some() && current != st.original_command {
            st.original_command = current;
            self.persist(&st)?;
        }
        write_handler(p, &our_command()).map_err(|e| format!("relay: {e}"))?;
        log::info("relay: handler was re-registered; re-applied");
        Ok(true)
    }

    /// Restore the saved command (only if the handler still points at us), delete relay.json.
    /// With nothing saved, Claude's own `"<exe>" "%1"` is restored when Claude can be located,
    /// else our value is removed (Claude re-registers itself on its next start).
    pub fn end(&self) -> Result<(), String> {
        let st = self.state();
        let mut result = Ok(());
        if let Some(p) = self.protocol.as_deref() {
            if read_handler(p).as_deref().is_some_and(points_at_us) {
                let restore = st
                    .as_ref()
                    .and_then(|s| s.original_command.clone())
                    // A Store update may have removed the saved Claude folder meanwhile.
                    .filter(|c| !points_at_us(c) && Path::new(command_exe(c)).is_file())
                    .or_else(|| claude_app::locate().ok().map(|l| default_command(&l.exe)));
                result = match restore {
                    Some(c) => write_handler(p, &c),
                    None => registry::delete_default(&command_key(p)),
                }
                .map_err(|e| format!("relay: {e}"));
            }
        }
        let removed = self.remove_file();
        if st.is_some() {
            log::info("relay: sign-in routing ended");
        }
        result.and(removed)
    }

    /// True when the active relay started `TIMEOUT_SECS` or more before `now_secs` (Unix
    /// seconds). An unreadable `since` counts as expired; no/inactive state never expires.
    pub fn expired(&self, now_secs: u64) -> bool {
        self.state().is_some_and(|s| state_expired(&s, now_secs))
    }
}

/// [`Relay::expired`] for one state.
fn state_expired(s: &RelayState, now_secs: u64) -> bool {
    s.active
        && match iso_to_secs(&s.since) {
            Some(t) => now_secs >= t.saturating_add(TIMEOUT_SECS),
            None => true,
        }
}

/// Unix seconds now (for [`Relay::expired`]).
pub fn now_secs() -> u64 {
    log::now_millis() / 1000
}

/// `YYYY-MM-DDTHH:MM:SS[.mmm]Z` → Unix seconds.
fn iso_to_secs(s: &str) -> Option<u64> {
    if !date::is_iso_utc(s) || s.len() < 20 {
        return None;
    }
    let (y, m, d) = date::parse_ymd(&s[..10])?;
    let b = s.as_bytes();
    if b[13] != b':' || b[16] != b':' {
        return None;
    }
    let n = |r: std::ops::Range<usize>| s[r].parse::<u64>().ok();
    let (hh, mm, ss) = (n(11..13)?, n(14..16)?, n(17..19)?);
    if hh > 23 || mm > 59 || ss > 60 {
        return None;
    }
    let days = u64::try_from(date::days_from_civil(y, m, d)).ok()?;
    Some(days * 86_400 + hh * 3600 + mm * 60 + ss)
}

// ───────────────────────────────────────────────────────────── --relay ──

/// A link we may forward: starts with `claude://` (any case), ≤ 8192 chars, no control chars,
/// no `"`. Returns it trimmed of surrounding whitespace.
pub fn validate_url(url: &str) -> Result<String, String> {
    let u = url.trim();
    if u.is_empty() {
        return Err("empty link".into());
    }
    let prefix = format!("{PROTOCOL}://");
    if u.len() < prefix.len() || !u.is_char_boundary(prefix.len()) || !u[..prefix.len()].eq_ignore_ascii_case(&prefix) {
        return Err("not a claude:// link".into());
    }
    if u.chars().count() > MAX_URL_LEN {
        return Err("link too long".into());
    }
    if u.chars().any(char::is_control) {
        return Err("link contains control characters".into());
    }
    if u.contains('"') {
        return Err("link contains quotes".into());
    }
    Ok(u.to_string())
}

/// A refused `--relay` launch whose command line shows argument injection tells the running
/// Sessionmux, if any, to play its tamper gesture. The relay process itself shows no UI; never blocks.
pub fn signal_bad_link() {
    if let Some(h) = crate::util::ui_thread::find_msg_window() {
        signal_bad_link_to(h);
    }
}

/// Post `WM_SESSIONMUX_GESTURE` to `hwnd_msg` (integers only; failure is ignored).
pub fn signal_bad_link_to(hwnd_msg: windows::Win32::Foundation::HWND) -> bool {
    use windows::Win32::Foundation::{LPARAM, WPARAM};
    use windows::Win32::UI::WindowsAndMessaging::PostMessageW;
    // SAFETY: posts a private message with integer parameters; a stale handle just fails.
    unsafe { PostMessageW(hwnd_msg, crate::util::ui_thread::WM_SESSIONMUX_GESTURE, WPARAM(0), LPARAM(0)) }.is_ok()
}

/// `Some(data_dir)` while a relay is active (and names a profile).
pub fn relay_target(state: Option<&RelayState>) -> Option<String> {
    state.filter(|s| s.active && !s.data_dir.trim().is_empty()).map(|s| s.data_dir.clone())
}

/// The `--relay` decision: `(target, stale)`. A relay that timed out, or whose Sessionmux is no
/// longer running (crash, kill, logoff), never routes: the link goes to the default Claude.
/// `stale` (active state but Sessionmux gone) tells the caller to `end()` it so the handler heals
/// now; an expired relay of a running Sessionmux is left to its own timer (it shows the toast).
pub fn relay_decision(state: Option<&RelayState>, now_secs: u64, sessionmux_running: bool) -> (Option<String>, bool) {
    let Some(s) = state.filter(|s| s.active) else {
        return (None, false);
    };
    if !sessionmux_running {
        return (None, true);
    }
    if state_expired(s, now_secs) {
        return (None, false);
    }
    (relay_target(Some(s)), false)
}

/// CreateProcessW: `"<exe>" [--user-data-dir="<dir>"] "<url>"`. The url is validated first.
pub fn forward(url: &str, exe: &str, data_dir: Option<&str>) -> Result<(), String> {
    let url = validate_url(url)?;
    claude_app::launch_url(exe, data_dir, &url).map(|_| ())
}

/// `scheme://host/path` of a link for the log: no query, no fragment, at most 200 chars.
pub fn redact(url: &str) -> String {
    let cut = url.find(['?', '#']).unwrap_or(url.len());
    url[..cut].chars().filter(|c| !c.is_control()).take(200).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Throw-away protocol `sessionmux-test-<uuid>`; its whole `HKCU\Software\Classes\<p>` tree
    /// is deleted on drop (also when the test panics).
    struct TestProtocol(String);

    impl TestProtocol {
        fn new() -> TestProtocol {
            TestProtocol(format!("sessionmux-test-{}", uuid::Uuid::new_v4()))
        }
        fn root(&self) -> String {
            format!("Software\\Classes\\{}", self.0)
        }
    }

    impl Drop for TestProtocol {
        fn drop(&mut self) {
            assert!(self.0.starts_with("sessionmux-test-"));
            registry::delete_tree(&self.root());
        }
    }

    const CLAUDE: &str = r#""C:\Program Files\WindowsApps\Claude_3.6556.2.0_x64__pzs8sxrjxfjjc\app\Claude.exe" "%1""#;

    fn st(active: bool, dir: &str) -> RelayState {
        RelayState {
            active,
            account_id: "a1".into(),
            account_name: "Work".into(),
            data_dir: dir.into(),
            since: "2026-10-26T08:00:00.000Z".into(),
            original_command: None,
        }
    }

    #[test]
    fn registry_round_trip() {
        let tp = TestProtocol::new();
        assert_eq!(read_handler(&tp.0), None);
        assert!(!registry::key_exists(&tp.root()));
        write_handler(&tp.0, CLAUDE).unwrap();
        assert_eq!(read_handler(&tp.0).as_deref(), Some(CLAUDE));
        assert!(!points_at_us(CLAUDE));
        let ours = our_command();
        write_handler(&tp.0, &ours).unwrap();
        assert_eq!(read_handler(&tp.0).as_deref(), Some(ours.as_str()));
        assert!(points_at_us(&ours));
        // A long value round-trips too (buffer growth).
        let long = format!("\"C:\\{}\\Claude.exe\" \"%1\"", "x".repeat(2000));
        write_handler(&tp.0, &long).unwrap();
        assert_eq!(read_handler(&tp.0).as_deref(), Some(long.as_str()));
        write_handler(&tp.0, CLAUDE).unwrap();
        assert_eq!(read_handler(&tp.0).as_deref(), Some(CLAUDE));
        registry::delete_default(&command_key(&tp.0)).unwrap();
        assert_eq!(read_handler(&tp.0), None);
        registry::delete_tree(&tp.root());
        assert!(!registry::key_exists(&tp.root()));
    }

    #[test]
    fn command_shapes() {
        let ours = our_command();
        assert!(ours.starts_with('"'));
        assert!(ours.ends_with(" --relay \"%1\""));
        assert!(Path::new(command_exe(&ours)).is_absolute());
        assert!(points_at_us(&ours));
        assert!(points_at_us(r#""C:\Other\Place\SESSIONMUX.EXE" --relay "%1""#));
        assert!(!points_at_us(r#""C:\Other\Place\Sessionmux.exe" "%1""#));
        assert!(!points_at_us(CLAUDE));
        assert!(!points_at_us(""));
        assert_eq!(default_command(r"C:\a b\Claude.exe"), r#""C:\a b\Claude.exe" "%1""#);
        assert_eq!(command_exe(r"C:\x\a.exe --relay %1"), r"C:\x\a.exe");
    }

    /// The bad-link hand-off reaches a message window as WM_SESSIONMUX_GESTURE. Uses a private
    /// window of this test only (never `signal_bad_link`, which would find a running Sessionmux).
    #[test]
    fn bad_link_posts_gesture_message() {
        use crate::util::ui_thread;
        use windows::Win32::UI::WindowsAndMessaging::{DestroyWindow, PeekMessageW, MSG, PM_REMOVE};
        let hwnd = ui_thread::create_msg_window(None);
        assert!(!hwnd.is_invalid());
        assert!(signal_bad_link_to(hwnd));
        let mut msg = MSG::default();
        let mut seen = 0;
        // SAFETY: reads this thread's queue for its own window, then destroys that window.
        unsafe {
            while PeekMessageW(&mut msg, hwnd, 0, 0, PM_REMOVE).as_bool() {
                if msg.message == ui_thread::WM_SESSIONMUX_GESTURE {
                    seen += 1;
                }
            }
            let _ = DestroyWindow(hwnd);
        }
        assert_eq!(seen, 1);
    }

    #[test]
    fn validate_url_rules() {
        assert_eq!(validate_url("claude://auth?x=1").as_deref(), Ok("claude://auth?x=1"));
        assert_eq!(validate_url("  CLAUDE://auth/callback?code=1 ").as_deref(), Ok("CLAUDE://auth/callback?code=1"));
        assert!(validate_url("http://claude.ai/").is_err());
        assert!(validate_url("https://claude://x").is_err());
        assert!(validate_url("claude:").is_err());
        assert!(validate_url("").is_err());
        assert!(validate_url("claude://a\"b").is_err());
        assert!(validate_url("claude://a\" --inject \"").is_err());
        assert!(validate_url("claude://a\nb").is_err());
        assert!(validate_url("claude://a\u{0}b").is_err());
        assert!(validate_url("claude://a\tb").is_err());
        let max = format!("claude://{}", "a".repeat(MAX_URL_LEN - 9));
        assert!(validate_url(&max).is_ok());
        assert!(validate_url(&format!("{max}a")).is_err());
        assert!(validate_url("ö").is_err());
    }

    #[test]
    fn relay_target_rules() {
        assert_eq!(relay_target(None), None);
        assert_eq!(relay_target(Some(&st(false, r"C:\D\Claude-Work"))), None);
        assert_eq!(relay_target(Some(&st(true, r"C:\D\Claude-Work"))).as_deref(), Some(r"C:\D\Claude-Work"));
        assert_eq!(relay_target(Some(&st(true, "  "))), None);
    }

    #[test]
    fn relay_decision_rules() {
        let dir = r"C:\D\Claude-Work";
        let s = st(true, dir);
        let t0 = iso_to_secs(&s.since).unwrap();
        assert_eq!(relay_decision(None, t0, true), (None, false));
        assert_eq!(relay_decision(Some(&st(false, dir)), t0, false), (None, false));
        assert_eq!(relay_decision(Some(&s), t0 + 60, true), (Some(dir.to_string()), false));
        // Timed out: default target; the running app's timer ends it.
        assert_eq!(relay_decision(Some(&s), t0 + TIMEOUT_SECS, true), (None, false));
        // Sessionmux gone (crash, kill, logoff): default target and heal now.
        assert_eq!(relay_decision(Some(&s), t0 + 60, false), (None, true));
        assert_eq!(relay_decision(Some(&s), t0 + TIMEOUT_SECS * 10, false), (None, true));
    }

    #[test]
    fn forward_builds_the_command_line() {
        // forward() delegates to claude_app::build_url_cmdline (no Claude is spawned here).
        let exe = r"C:\Program Files\WindowsApps\Claude_1\app\Claude.exe";
        let url = validate_url("claude://auth/callback?code=xyz").unwrap();
        assert_eq!(
            claude_app::build_url_cmdline(exe, Some(r"C:\Users\u\AppData\Roaming\Claude-Work"), &url).unwrap(),
            format!("\"{exe}\" --user-data-dir=\"C:\\Users\\u\\AppData\\Roaming\\Claude-Work\" \"{url}\"")
        );
        assert_eq!(claude_app::build_url_cmdline(exe, None, &url).unwrap(), format!("\"{exe}\" \"{url}\""));
        // Invalid links are refused before anything is started; a missing exe is an error.
        assert!(forward("http://x", exe, None).is_err());
        assert!(forward("claude://x", r"C:\definitely\missing\Claude.exe", None).is_err());
    }

    #[test]
    fn redact_drops_query_and_fragment() {
        assert_eq!(redact("claude://auth/callback?code=secret&state=1"), "claude://auth/callback");
        assert_eq!(redact("claude://host/path#frag"), "claude://host/path");
        assert_eq!(redact("claude://host/path"), "claude://host/path");
    }

    #[test]
    fn begin_ensure_end() {
        let tp = TestProtocol::new();
        let d = tempfile::tempdir().unwrap();
        let r = Relay::open_with(d.path(), Some(&tp.0));
        assert_eq!(r.state(), None);
        assert_eq!(r.ensure(), Ok(false));
        // end() only restores a saved command whose exe exists: use stand-in files.
        let fake = |v: &str| {
            let dir = d.path().join(v);
            std::fs::create_dir_all(&dir).unwrap();
            let exe = dir.join("Claude.exe");
            std::fs::write(&exe, b"").unwrap();
            default_command(&exe.to_string_lossy())
        };
        let (claude, claude2) = (fake("v1"), fake("v2"));
        let (c1, c2) = (claude.as_str(), claude2.as_str());

        // Fresh begin saves Claude's handler and writes ours.
        write_handler(&tp.0, c1).unwrap();
        let s = r.begin("a1", "Work", r"C:\D\Claude-Work").unwrap();
        assert!(s.active);
        assert_eq!(s.original_command.as_deref(), Some(c1));
        assert!(date::is_iso_utc(&s.since));
        assert_eq!(r.state(), Some(s.clone()));
        assert!(points_at_us(&read_handler(&tp.0).unwrap()));
        let raw: serde_json::Value = json::read(&d.path().join(FILE)).unwrap();
        assert_eq!(raw["accountId"], "a1");
        assert_eq!(raw["accountName"], "Work");
        assert_eq!(raw["dataDir"], r"C:\D\Claude-Work");
        assert_eq!(raw["originalCommand"], c1);
        assert_eq!(r.ensure(), Ok(false));

        // begin again while the handler already points at us: original_command is kept.
        let s2 = r.begin("a2", "Home", r"C:\D\Claude-Home").unwrap();
        assert_eq!(s2.original_command.as_deref(), Some(c1));
        assert_eq!(relay_target(r.state().as_ref()).as_deref(), Some(r"C:\D\Claude-Home"));

        // Claude re-registered (new version) in between: ensure re-writes and remembers it.
        write_handler(&tp.0, c2).unwrap();
        assert_eq!(r.ensure(), Ok(true));
        assert!(points_at_us(&read_handler(&tp.0).unwrap()));
        assert_eq!(r.state().unwrap().original_command.as_deref(), Some(c2));
        assert_eq!(r.ensure(), Ok(false));

        // end restores the saved handler and deletes relay.json.
        r.end().unwrap();
        assert_eq!(read_handler(&tp.0).as_deref(), Some(c2));
        assert_eq!(r.state(), None);
        assert!(!d.path().join(FILE).exists());
        assert_eq!(r.ensure(), Ok(false));
        r.end().unwrap(); // idempotent

        // end when someone else changed the handler: leave it alone, still delete relay.json.
        r.begin("a1", "Work", r"C:\D\Claude-Work").unwrap();
        let other = r#""C:\Other\Thing.exe" "%1""#;
        write_handler(&tp.0, other).unwrap();
        r.end().unwrap();
        assert_eq!(read_handler(&tp.0).as_deref(), Some(other));
        assert!(!d.path().join(FILE).exists());

        // The saved Claude folder vanished (Store update): never restore a dead path.
        write_handler(&tp.0, c1).unwrap();
        r.begin("a1", "Work", r"C:\D\Claude-Work").unwrap();
        std::fs::remove_file(d.path().join("v1").join("Claude.exe")).unwrap();
        r.end().unwrap();
        let after = read_handler(&tp.0);
        assert_ne!(after.as_deref(), Some(c1));
        assert!(!after.as_deref().is_some_and(points_at_us));
    }

    #[test]
    fn begin_without_a_previous_handler() {
        let tp = TestProtocol::new();
        let d = tempfile::tempdir().unwrap();
        let r = Relay::open_with(d.path(), Some(&tp.0));
        let s = r.begin("a1", "Work", r"C:\D\Claude-Work").unwrap();
        assert_eq!(s.original_command, None);
        assert!(points_at_us(&read_handler(&tp.0).unwrap()));
        r.end().unwrap();
        // Restored to Claude's default command (when Claude is installed) or removed; never ours.
        assert!(!read_handler(&tp.0).as_deref().is_some_and(points_at_us));
        assert_eq!(r.state(), None);
        assert!(r.begin("a1", "Work", "  ").is_err());
    }

    #[test]
    fn no_registry_mode_and_corrupt_file() {
        let d = tempfile::tempdir().unwrap();
        let r = Relay::open(d.path()); // cfg(test): no registry
        assert!(r.protocol.is_none());
        r.begin("a1", "Work", r"C:\D\Claude-Work").unwrap();
        assert!(r.state().unwrap().active);
        assert_eq!(r.ensure(), Ok(false));
        r.end().unwrap();
        assert_eq!(r.state(), None);

        std::fs::write(d.path().join(FILE), b"{nope").unwrap();
        assert_eq!(r.state(), None);
        assert!(d.path().join("relay.json.corrupt").exists());
        assert!(!r.expired(u64::MAX));
    }

    #[test]
    fn expired_boundary() {
        let d = tempfile::tempdir().unwrap();
        let r = Relay::open_with(d.path(), None);
        assert!(!r.expired(u64::MAX), "no state never expires");
        let mut s = st(true, r"C:\D\Claude-Work");
        s.since = "2026-10-26T08:00:00.000Z".into();
        json::write_atomic(&d.path().join(FILE), &s).unwrap();
        let t0 = iso_to_secs(&s.since).unwrap();
        assert_eq!(log::iso_from_millis(t0 * 1000), "2026-10-26T08:00:00.000Z");
        assert_eq!(iso_to_secs("1970-01-02T00:00:01Z"), Some(86_401));
        assert!(!r.expired(t0));
        assert!(!r.expired(t0 + TIMEOUT_SECS - 1));
        assert!(r.expired(t0 + TIMEOUT_SECS));
        assert!(r.expired(t0 + TIMEOUT_SECS + 1));

        s.active = false;
        json::write_atomic(&d.path().join(FILE), &s).unwrap();
        assert!(!r.expired(t0 + TIMEOUT_SECS * 10));

        s.active = true;
        s.since = "garbage".into();
        json::write_atomic(&d.path().join(FILE), &s).unwrap();
        assert!(r.expired(0), "an unreadable start time ends the relay");
        assert!(now_secs() > t0 - 86_400 * 365);
    }
}

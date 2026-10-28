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

//! The per-user link handler on Windows: `(Default)` of
//! `HKCU\Software\Classes\<scheme>\shell\open\command`. Ours is `"<Sessionmux.exe>" --relay "%1"`;
//! a link is forwarded as `"<Claude.exe>" [--user-data-dir="<dir>"] "<url>"`. Every change is
//! immediate and asks nobody, so changes are `Done` or `Failed`.

use std::path::Path;

use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::PostMessageW;

use super::registry;
use crate::brand;
use crate::core::claude_app::Located;
use crate::platform::{ForwardError, HandlerChange, HandlerId, Os, Paths, Pid, UrlHandler};

fn command_key(scheme: &str) -> String {
    format!("Software\\Classes\\{scheme}\\shell\\open\\command")
}

/// `(Default)` of `HKCU\Software\Classes\<scheme>\shell\open\command`.
fn read_handler(scheme: &str) -> Option<String> {
    registry::read_default(&command_key(scheme))
}

/// Set that value, creating the key path when missing.
fn write_handler(scheme: &str, command: &str) -> Result<(), String> {
    registry::write_default(&command_key(scheme), command)
}

/// `"<full path of current exe>" --relay "%1"`.
fn our_command() -> String {
    format!("\"{}\" --relay \"%1\"", <Os as Paths>::current_exe())
}

/// `"<exe>" "%1"` — what Claude itself registers; the fallback when nothing was saved.
fn default_command(exe: &str) -> String {
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

/// True when `command` runs a Sessionmux relay: its exe is the current exe or any `Sessionmux.exe`
/// (an older install location still counts — it must never be saved as the "original"), and it
/// carries `--relay`.
fn points_at_us(command: &str) -> bool {
    let exe = command_exe(command);
    if exe.is_empty() || !command.to_ascii_lowercase().contains("--relay") {
        return false;
    }
    let leaf = exe.rsplit(['\\', '/']).next().unwrap_or(exe);
    leaf.eq_ignore_ascii_case(brand::EXE) || <Os as Paths>::same_path(exe, &<Os as Paths>::current_exe())
}

fn changed(r: Result<(), String>) -> HandlerChange {
    match r {
        Ok(()) => HandlerChange::Done,
        Err(e) => HandlerChange::Failed(e),
    }
}

/// Post `WM_SESSIONMUX_GESTURE` to `hwnd_msg` (integers only; failure is ignored).
fn signal_bad_link_to(hwnd_msg: HWND) -> bool {
    // SAFETY: posts a private message with integer parameters; a stale handle just fails.
    unsafe { PostMessageW(hwnd_msg, super::event_loop::WM_SESSIONMUX_GESTURE, WPARAM(0), LPARAM(0)) }.is_ok()
}

impl UrlHandler for Os {
    fn current(scheme: &str) -> Option<HandlerId> {
        read_handler(scheme).map(HandlerId)
    }

    fn take(scheme: &str) -> HandlerChange {
        changed(write_handler(scheme, &our_command()))
    }

    /// A saved command that is ours, or whose exe no longer exists (an update removed the old
    /// folder), is never written back; Claude's own `"<exe>" "%1"` is used instead, and without a
    /// located Claude our value is removed (Claude registers itself again on its next start).
    fn restore(scheme: &str, prev: Option<&HandlerId>, l: Option<&Located>) -> HandlerChange {
        let restore = prev
            .map(|h| h.0.clone())
            .filter(|c| !points_at_us(c) && Path::new(command_exe(c)).is_file())
            .or_else(|| l.map(|l| default_command(&l.exe)));
        changed(match restore {
            Some(c) => write_handler(scheme, &c),
            None => registry::delete_default(&command_key(scheme)),
        })
    }

    fn is_us(h: &HandlerId) -> bool {
        points_at_us(&h.0)
    }

    fn changes_need_consent() -> bool {
        false
    }

    /// Quitting gives the handler back before the process ends.
    fn restore_at_exit() -> bool {
        true
    }

    /// Claude registering itself again on a start is taken back every time.
    fn retake_limit() -> Option<u32> {
        None
    }

    /// Every copy can take the per-user handler.
    fn unavailable() -> Option<String> {
        None
    }

    /// CreateProcessW: `"<exe>" [--user-data-dir="<dir>"] "<url>"`; Claude's single-instance lock
    /// hands the link to the running instance of that folder.
    fn forward(l: &Located, data_dir: Option<&str>, _pid: Option<Pid>, url: &str) -> Result<(), ForwardError> {
        super::claude::launch_url(&l.exe, data_dir, url).map(|_| ()).map_err(ForwardError::from)
    }

    fn signal_bad_link() {
        if let Some(h) = super::event_loop::find_msg_window() {
            signal_bad_link_to(h);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Throw-away scheme `sessionmux-test-<uuid>`; its whole `HKCU\Software\Classes\<s>` tree
    /// is deleted on drop (also when the test panics).
    struct TestScheme(String);

    impl TestScheme {
        fn new() -> TestScheme {
            TestScheme(format!("sessionmux-test-{}", uuid::Uuid::new_v4()))
        }
        fn root(&self) -> String {
            format!("Software\\Classes\\{}", self.0)
        }
    }

    impl Drop for TestScheme {
        fn drop(&mut self) {
            assert!(self.0.starts_with("sessionmux-test-"));
            registry::delete_tree(&self.root());
        }
    }

    const CLAUDE: &str = r#""C:\Program Files\WindowsApps\Claude_3.6556.2.0_x64__pzs8sxrjxfjjc\app\Claude.exe" "%1""#;

    #[test]
    fn registry_round_trip() {
        let tp = TestScheme::new();
        assert_eq!(read_handler(&tp.0), None);
        assert!(!registry::key_exists(&tp.root()));
        write_handler(&tp.0, CLAUDE).unwrap();
        assert_eq!(read_handler(&tp.0).as_deref(), Some(CLAUDE));
        assert!(!points_at_us(CLAUDE));
        assert_eq!(Os::take(&tp.0), HandlerChange::Done);
        let ours = our_command();
        assert_eq!(read_handler(&tp.0).as_deref(), Some(ours.as_str()));
        assert!(Os::is_us(&Os::current(&tp.0).unwrap()));
        // A long value round-trips too (buffer growth).
        let long = format!("\"C:\\{}\\Claude.exe\" \"%1\"", "x".repeat(2000));
        write_handler(&tp.0, &long).unwrap();
        assert_eq!(read_handler(&tp.0).as_deref(), Some(long.as_str()));
        write_handler(&tp.0, CLAUDE).unwrap();
        assert_eq!(Os::current(&tp.0), Some(HandlerId(CLAUDE.into())));
        registry::delete_default(&command_key(&tp.0)).unwrap();
        assert_eq!(read_handler(&tp.0), None);
        registry::delete_tree(&tp.root());
        assert!(!registry::key_exists(&tp.root()));
    }

    #[test]
    fn restore_rules() {
        let tp = TestScheme::new();
        let d = tempfile::tempdir().unwrap();
        let exe = d.path().join("Claude.exe");
        std::fs::write(&exe, b"").unwrap();
        let saved = HandlerId(default_command(&exe.to_string_lossy()));
        let located = Located { exe: r"C:\Located\Claude.exe".into(), ..Located::default() };

        // A saved command whose exe exists is written back.
        assert_eq!(Os::take(&tp.0), HandlerChange::Done);
        assert_eq!(Os::restore(&tp.0, Some(&saved), Some(&located)), HandlerChange::Done);
        assert_eq!(Os::current(&tp.0), Some(saved.clone()));

        // Ours, or a vanished exe, is never restored: the located Claude is used instead.
        for bad in [HandlerId(our_command()), HandlerId(default_command(r"C:\definitely\missing\Claude.exe"))] {
            assert_eq!(Os::restore(&tp.0, Some(&bad), Some(&located)), HandlerChange::Done);
            assert_eq!(Os::current(&tp.0), Some(HandlerId(default_command(&located.exe))));
        }
        // Nothing usable and no Claude: the value is removed.
        assert_eq!(Os::restore(&tp.0, None, None), HandlerChange::Done);
        assert_eq!(Os::current(&tp.0), None);
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
        assert!(!Os::changes_need_consent());
        assert!(Os::restore_at_exit());
        assert_eq!(Os::retake_limit(), None);
    }

    /// The bad-link hand-off reaches a message window as WM_SESSIONMUX_GESTURE. Uses a private
    /// window of this test only (never `signal_bad_link`, which would find a running Sessionmux).
    #[test]
    fn bad_link_posts_gesture_message() {
        use crate::platform::windows::event_loop;
        use windows::Win32::UI::WindowsAndMessaging::{DestroyWindow, PeekMessageW, MSG, PM_REMOVE};
        let hwnd = event_loop::create_msg_window(None);
        assert!(!hwnd.is_invalid());
        assert!(signal_bad_link_to(hwnd));
        let mut msg = MSG::default();
        let mut seen = 0;
        // SAFETY: reads this thread's queue for its own window, then destroys that window.
        unsafe {
            while PeekMessageW(&mut msg, hwnd, 0, 0, PM_REMOVE).as_bool() {
                if msg.message == event_loop::WM_SESSIONMUX_GESTURE {
                    seen += 1;
                }
            }
            let _ = DestroyWindow(hwnd);
        }
        assert_eq!(seen, 1);
    }
}

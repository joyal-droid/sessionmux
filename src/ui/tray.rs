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

//! Notification-area icon and its context menu.
//!
//! API:
//! ```ignore
//! impl Tray {
//!   pub fn create(hwnd_msg:HWND, icon:HICON) -> Result<Tray,String>
//!   pub fn set_menu(&mut self, accounts:&[(String,String,bool)], bubble_shown:bool, bubble_mode:&str)
//!   pub fn set_waiting(&self, n:usize)   // count in the tooltip
//!   pub fn handle(&self, msg:u32, wparam:WPARAM, lparam:LPARAM) -> Option<TrayEvent>
//! }
//! pub enum TrayEvent { LeftClick, OpenAccount(String), OpenAll, ToggleBubble, BubbleMode(String), OpenSettings, Quit }
//! ```
//!
//! How it works:
//! - The icon (Shell_NotifyIconW, NOTIFYICON_VERSION_4, uID 1) reports to `hwnd_msg` with
//!   [`WM_TRAY_CALLBACK`]; the app's window procedure passes every message to [`Tray::handle`].
//! - A hidden top-level "owner" window (class `SessionmuxTrayOwner`, never shown) is created because a
//!   message-only window can neither own a popup menu (it cannot be foreground) nor receive the
//!   `TaskbarCreated` broadcast. On `TaskbarCreated` (Explorer restarted) it posts
//!   [`WM_TRAY_RECREATE`] to `hwnd_msg`, and `handle` re-adds the icon.
//! - Left click / keyboard select → `LeftClick`. Right click (WM_CONTEXTMENU) shows the menu built
//!   from the last `set_menu` state and returns the chosen item's event (TrackPopupMenuEx with
//!   TPM_RETURNCMD, so `handle` runs a modal menu loop: do not hold a RefCell borrow that queued
//!   tasks need while calling it).
//! - Menu: one item per account (radio bullet when running) or a disabled "No accounts",
//!   separator, "Open all", "Show bubble"/"Hide bubble", "Floating button" ▸ (Icon | Mini tray,
//!   radio), "Settings…", separator, "Quit Sessionmux".
//! - Dropping the Tray removes the icon and destroys the owner window.

use crate::brand;
use crate::util::{log, win};
use std::cell::Cell;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::{Duration, Instant};
use windows::core::PCWSTR;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::UI::Shell::{
    Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_SHOWTIP, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY, NIM_SETVERSION,
    NINF_KEY, NIN_SELECT, NOTIFYICONDATAW, NOTIFYICON_VERSION_4,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CheckMenuRadioItem, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu, DestroyWindow,
    GetCursorPos, GetSystemMetrics, GetWindowLongPtrW, PostMessageW, RegisterClassExW, RegisterWindowMessageW,
    SetForegroundWindow, SetWindowLongPtrW, TrackPopupMenuEx, GWLP_USERDATA, HICON, HMENU, MF_BYCOMMAND, MF_GRAYED,
    MF_POPUP, MF_SEPARATOR, MF_STRING, SM_MENUDROPALIGNMENT, TPM_BOTTOMALIGN, TPM_LEFTALIGN, TPM_NONOTIFY,
    TPM_RETURNCMD, TPM_RIGHTALIGN, TPM_RIGHTBUTTON, WM_APP, WM_CONTEXTMENU, WM_NULL, WNDCLASSEXW, WS_EX_TOOLWINDOW,
    WS_POPUP,
};

/// Callback message of the notification icon (posted to `hwnd_msg`).
pub const WM_TRAY_CALLBACK: u32 = WM_APP + 0x0F00;
/// Posted to `hwnd_msg` by the owner window when Explorer restarts (TaskbarCreated).
pub const WM_TRAY_RECREATE: u32 = WM_APP + 0x0F01;

const TRAY_UID: u32 = 1;
/// Keyboard select (Space/Enter on the focused icon) = NIN_SELECT | NINF_KEY.
const NIN_KEYSELECT: u32 = NIN_SELECT | NINF_KEY;
const TOOLTIP: &str = brand::TRAY_TOOLTIP;

const ID_OPEN_ALL: u32 = 1;
const ID_TOGGLE_BUBBLE: u32 = 2;
const ID_MODE_ICON: u32 = 3;
const ID_MODE_TRAY: u32 = 4;
const ID_SETTINGS: u32 = 5;
const ID_QUIT: u32 = 6;
const ID_ACCOUNT_BASE: u32 = 1000;

#[derive(Clone, Debug, PartialEq)]
pub enum TrayEvent {
    LeftClick,
    /// Account id.
    OpenAccount(String),
    OpenAll,
    ToggleBubble,
    /// "icon" | "tray"
    BubbleMode(String),
    OpenSettings,
    Quit,
}

pub struct Tray {
    hwnd_msg: HWND,
    icon: HICON,
    owner: HWND,
    /// (id, name, running)
    accounts: Vec<(String, String, bool)>,
    bubble_shown: bool,
    bubble_mode: String,
    added: Cell<bool>,
    last_select: Cell<Option<Instant>>,
    /// Requests waiting for the user's approval, shown in the tooltip.
    waiting: Cell<usize>,
}

static OWNER_CLASS_REGISTERED: AtomicBool = AtomicBool::new(false);
static TASKBAR_CREATED: AtomicU32 = AtomicU32::new(0);
const OWNER_CLASS: &str = brand::CLASS_TRAY_OWNER;

unsafe extern "system" fn owner_wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let tc = TASKBAR_CREATED.load(Ordering::Relaxed);
    if tc != 0 && msg == tc {
        // SAFETY: GWLP_USERDATA of our own owner window holds the message window handle value
        // stored by Tray::create (or 0).
        let target = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) };
        if target != 0 {
            // SAFETY: posting an integer-only private message to our own message window.
            let _ = unsafe { PostMessageW(HWND(target as _), WM_TRAY_RECREATE, WPARAM(0), LPARAM(0)) };
        }
        return LRESULT(0);
    }
    // SAFETY: default processing with the original arguments for a window we own.
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

fn create_owner_window(hwnd_msg: HWND) -> Result<HWND, String> {
    let class = win::wide(OWNER_CLASS);
    let hinst = win::hinstance();
    if !OWNER_CLASS_REGISTERED.load(Ordering::Acquire) {
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(owner_wndproc),
            hInstance: hinst,
            lpszClassName: PCWSTR(class.as_ptr()),
            ..Default::default()
        };
        // SAFETY: `wc` is fully initialised and `class` outlives the call.
        if unsafe { RegisterClassExW(&wc) } == 0 {
            return Err(format!("RegisterClassExW({OWNER_CLASS}): {}", win::last_error()));
        }
        OWNER_CLASS_REGISTERED.store(true, Ordering::Release);
    }
    let tc_name = win::wide("TaskbarCreated");
    // SAFETY: `tc_name` is a NUL-terminated buffer that outlives the call.
    let tc = unsafe { RegisterWindowMessageW(PCWSTR(tc_name.as_ptr())) };
    TASKBAR_CREATED.store(tc, Ordering::Relaxed);
    // SAFETY: class registered above; strings outlive the call. A never-shown WS_POPUP tool
    // window: it exists only to own the popup menu and receive broadcasts.
    let hwnd = unsafe {
        CreateWindowExW(
            WS_EX_TOOLWINDOW,
            PCWSTR(class.as_ptr()),
            PCWSTR(class.as_ptr()),
            WS_POPUP,
            0,
            0,
            0,
            0,
            None,
            None,
            hinst,
            None,
        )
    }
    .map_err(|e| format!("CreateWindowExW({OWNER_CLASS}): {e}"))?;
    // SAFETY: storing an integer (the message window handle) in our own window's user data.
    unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, hwnd_msg.0 as isize) };
    Ok(hwnd)
}

fn loword(v: usize) -> u32 {
    (v & 0xFFFF) as u32
}

fn signed_words(v: usize) -> (i32, i32) {
    ((v & 0xFFFF) as u16 as i16 as i32, ((v >> 16) & 0xFFFF) as u16 as i16 as i32)
}

/// The tooltip, with the number of requests waiting for approval when there are any.
fn tooltip_text(waiting: usize) -> String {
    match waiting {
        0 => TOOLTIP.to_string(),
        1 => format!("{} · 1 request waiting for your approval", brand::NAME),
        n => format!("{} · {n} requests waiting for your approval", brand::NAME),
    }
}

/// Menu label with '&' escaped (otherwise it becomes a mnemonic).
fn menu_label(s: &str) -> Vec<u16> {
    let clean: String = s.chars().filter(|c| !c.is_control()).collect();
    win::wide(&clean.replace('&', "&&"))
}

impl Tray {
    pub fn create(hwnd_msg: HWND, icon: HICON) -> Result<Tray, String> {
        if hwnd_msg.is_invalid() {
            return Err("tray: invalid message window".into());
        }
        let owner = create_owner_window(hwnd_msg)?;
        let tray = Tray {
            hwnd_msg,
            icon,
            owner,
            accounts: Vec::new(),
            bubble_shown: true,
            bubble_mode: "icon".into(),
            added: Cell::new(false),
            last_select: Cell::new(None),
            waiting: Cell::new(0),
        };
        if let Err(e) = tray.add_icon() {
            // Explorer may not be up yet (early logon): TaskbarCreated will re-add it.
            log::warn(&format!("tray: {e}; will retry when the taskbar is (re)created"));
        }
        Ok(tray)
    }

    fn icon_data(&self) -> NOTIFYICONDATAW {
        let mut nid = NOTIFYICONDATAW {
            cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: self.hwnd_msg,
            uID: TRAY_UID,
            uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP | NIF_SHOWTIP,
            uCallbackMessage: WM_TRAY_CALLBACK,
            hIcon: self.icon,
            ..Default::default()
        };
        let tip: Vec<u16> = tooltip_text(self.waiting.get()).encode_utf16().take(nid.szTip.len() - 1).collect();
        nid.szTip[..tip.len()].copy_from_slice(&tip);
        nid
    }

    /// Show `n` requests waiting for approval in the tooltip (0 = the plain tooltip).
    pub fn set_waiting(&self, n: usize) {
        if self.waiting.replace(n) == n || !self.added.get() {
            return;
        }
        let nid = self.icon_data();
        // SAFETY: identifies our icon by hWnd + uID; fully initialised struct.
        if !unsafe { Shell_NotifyIconW(NIM_MODIFY, &nid) }.as_bool() {
            log::debug("tray: NIM_MODIFY failed");
        }
    }

    fn add_icon(&self) -> Result<(), String> {
        let mut nid = self.icon_data();
        // SAFETY: `nid` is a fully initialised NOTIFYICONDATAW with the correct cbSize.
        let ok = unsafe { Shell_NotifyIconW(NIM_ADD, &nid) }.as_bool();
        if !ok {
            return Err(format!("Shell_NotifyIcon(NIM_ADD) failed: {}", win::last_error()));
        }
        nid.Anonymous.uVersion = NOTIFYICON_VERSION_4;
        // SAFETY: as above; NIM_SETVERSION reads uVersion from the union.
        if !unsafe { Shell_NotifyIconW(NIM_SETVERSION, &nid) }.as_bool() {
            log::warn("tray: NIM_SETVERSION(4) failed");
        }
        self.added.set(true);
        Ok(())
    }

    fn remove_icon(&self) {
        if !self.added.replace(false) {
            return;
        }
        let nid = self.icon_data();
        // SAFETY: identifies our icon by hWnd + uID; fully initialised struct.
        let _ = unsafe { Shell_NotifyIconW(NIM_DELETE, &nid) };
    }

    /// `accounts` = (id, name, running).
    pub fn set_menu(&mut self, accounts: &[(String, String, bool)], bubble_shown: bool, bubble_mode: &str) {
        self.accounts = accounts.to_vec();
        self.bubble_shown = bubble_shown;
        self.bubble_mode = if bubble_mode == "tray" { "tray".into() } else { "icon".into() };
    }

    pub fn handle(&self, msg: u32, wparam: WPARAM, lparam: LPARAM) -> Option<TrayEvent> {
        if msg == WM_TRAY_RECREATE {
            self.added.set(false);
            if let Err(e) = self.add_icon() {
                log::warn(&format!("tray re-add: {e}"));
            }
            return None;
        }
        if msg != WM_TRAY_CALLBACK {
            return None;
        }
        // NOTIFYICON_VERSION_4: LOWORD(lparam) = event, HIWORD(lparam) = icon id,
        // wparam = anchor x/y in screen coordinates.
        let event = loword(lparam.0 as usize);
        match event {
            NIN_SELECT => Some(TrayEvent::LeftClick),
            NIN_KEYSELECT => {
                // Enter is reported twice; collapse repeats within 300 ms into one toggle.
                let now = Instant::now();
                let dup = self.last_select.get().is_some_and(|t| now.duration_since(t) < Duration::from_millis(300));
                self.last_select.set(Some(now));
                (!dup).then_some(TrayEvent::LeftClick)
            }
            WM_CONTEXTMENU => {
                let (mut x, mut y) = signed_words(wparam.0);
                if x == 0 && y == 0 {
                    let mut p = POINT::default();
                    // SAFETY: valid out-pointer.
                    if unsafe { GetCursorPos(&mut p) }.is_ok() {
                        x = p.x;
                        y = p.y;
                    }
                }
                self.show_menu(x, y)
            }
            _ => None,
        }
    }

    fn build_menu(&self) -> Result<(HMENU, HMENU), String> {
        // SAFETY: menu creation and population with buffers that outlive each call; on any
        // failure the caller destroys the returned/partial menus.
        unsafe {
            let menu = CreatePopupMenu().map_err(|e| format!("CreatePopupMenu: {e}"))?;
            let sub = match CreatePopupMenu() {
                Ok(s) => s,
                Err(e) => {
                    let _ = DestroyMenu(menu);
                    return Err(format!("CreatePopupMenu: {e}"));
                }
            };
            let res = (|| -> windows::core::Result<()> {
                if self.accounts.is_empty() {
                    let l = win::wide("No accounts");
                    AppendMenuW(menu, MF_STRING | MF_GRAYED, 0, PCWSTR(l.as_ptr()))?;
                } else {
                    for (i, (_, name, running)) in self.accounts.iter().enumerate() {
                        let id = ID_ACCOUNT_BASE + i as u32;
                        let l = menu_label(name);
                        AppendMenuW(menu, MF_STRING, id as usize, PCWSTR(l.as_ptr()))?;
                        if *running {
                            // first == last == checked → a single radio bullet on this item.
                            let _ = CheckMenuRadioItem(menu, id, id, id, MF_BYCOMMAND.0);
                        }
                    }
                }
                AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null())?;
                let l = win::wide("Open all");
                AppendMenuW(menu, MF_STRING, ID_OPEN_ALL as usize, PCWSTR(l.as_ptr()))?;
                let l = win::wide(if self.bubble_shown { "Hide bubble" } else { "Show bubble" });
                AppendMenuW(menu, MF_STRING, ID_TOGGLE_BUBBLE as usize, PCWSTR(l.as_ptr()))?;
                let l = win::wide("Icon");
                AppendMenuW(sub, MF_STRING, ID_MODE_ICON as usize, PCWSTR(l.as_ptr()))?;
                let l = win::wide("Mini tray");
                AppendMenuW(sub, MF_STRING, ID_MODE_TRAY as usize, PCWSTR(l.as_ptr()))?;
                let checked = if self.bubble_mode == "tray" { ID_MODE_TRAY } else { ID_MODE_ICON };
                let _ = CheckMenuRadioItem(sub, ID_MODE_ICON, ID_MODE_TRAY, checked, MF_BYCOMMAND.0);
                let l = win::wide("Floating button");
                AppendMenuW(menu, MF_STRING | MF_POPUP, sub.0 as usize, PCWSTR(l.as_ptr()))?;
                let l = win::wide("Settings\u{2026}");
                AppendMenuW(menu, MF_STRING, ID_SETTINGS as usize, PCWSTR(l.as_ptr()))?;
                AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null())?;
                let l = win::wide(&format!("Quit {}", brand::NAME));
                AppendMenuW(menu, MF_STRING, ID_QUIT as usize, PCWSTR(l.as_ptr()))?;
                Ok(())
            })();
            match res {
                Ok(()) => Ok((menu, sub)),
                Err(e) => {
                    // `sub` may or may not be attached yet; destroying both is safe because
                    // DestroyMenu on an already-destroyed submenu just fails.
                    let _ = DestroyMenu(menu);
                    let _ = DestroyMenu(sub);
                    Err(format!("AppendMenuW: {e}"))
                }
            }
        }
    }

    /// Map a menu command id to its event.
    fn command_event(&self, cmd: u32) -> Option<TrayEvent> {
        match cmd {
            0 => None,
            ID_OPEN_ALL => Some(TrayEvent::OpenAll),
            ID_TOGGLE_BUBBLE => Some(TrayEvent::ToggleBubble),
            ID_MODE_ICON => Some(TrayEvent::BubbleMode("icon".into())),
            ID_MODE_TRAY => Some(TrayEvent::BubbleMode("tray".into())),
            ID_SETTINGS => Some(TrayEvent::OpenSettings),
            ID_QUIT => Some(TrayEvent::Quit),
            n if n >= ID_ACCOUNT_BASE => {
                self.accounts.get((n - ID_ACCOUNT_BASE) as usize).map(|a| TrayEvent::OpenAccount(a.0.clone()))
            }
            _ => None,
        }
    }

    fn show_menu(&self, x: i32, y: i32) -> Option<TrayEvent> {
        let (menu, _sub) = match self.build_menu() {
            Ok(m) => m,
            Err(e) => {
                log::warn(&format!("tray menu: {e}"));
                return None;
            }
        };
        // SAFETY: `menu` and `owner` are live handles we created on this thread. The owner is
        // made foreground first so the menu closes on click-away; WM_NULL afterwards is the
        // documented fix for the menu-reopen glitch (KB135788). DestroyMenu also frees the
        // attached submenu.
        let cmd = unsafe {
            let _ = SetForegroundWindow(self.owner);
            let align = if GetSystemMetrics(SM_MENUDROPALIGNMENT) != 0 { TPM_RIGHTALIGN } else { TPM_LEFTALIGN };
            let flags = TPM_RETURNCMD | TPM_NONOTIFY | TPM_RIGHTBUTTON | TPM_BOTTOMALIGN | align;
            let r = TrackPopupMenuEx(menu, flags.0, x, y, self.owner, None);
            let _ = PostMessageW(self.owner, WM_NULL, WPARAM(0), LPARAM(0));
            let _ = DestroyMenu(menu);
            r.0 as u32
        };
        self.command_event(cmd)
    }
}

impl Drop for Tray {
    fn drop(&mut self) {
        self.remove_icon();
        // SAFETY: the owner window was created by this Tray on this thread and is destroyed once.
        let _ = unsafe { DestroyWindow(self.owner) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake(accounts: &[(&str, &str, bool)]) -> Tray {
        Tray {
            hwnd_msg: HWND::default(),
            icon: HICON::default(),
            owner: HWND::default(),
            accounts: accounts.iter().map(|(a, b, c)| (a.to_string(), b.to_string(), *c)).collect(),
            bubble_shown: true,
            bubble_mode: "icon".into(),
            added: Cell::new(false),
            last_select: Cell::new(None),
            waiting: Cell::new(0),
        }
    }

    #[test]
    fn tooltip_counts_waiting_requests() {
        assert_eq!(tooltip_text(0), TOOLTIP);
        assert_eq!(tooltip_text(1), "Sessionmux · 1 request waiting for your approval");
        assert_eq!(tooltip_text(3), "Sessionmux · 3 requests waiting for your approval");
        assert!(tooltip_text(1000).encode_utf16().count() < 128);
        let t = fake(&[]);
        t.set_waiting(2); // not added: only remembered
        assert_eq!(t.waiting.get(), 2);
    }

    #[test]
    fn maps_commands() {
        let t = fake(&[("main", "Main", true), ("w", "Work", false)]);
        assert_eq!(t.command_event(0), None);
        assert_eq!(t.command_event(ID_OPEN_ALL), Some(TrayEvent::OpenAll));
        assert_eq!(t.command_event(ID_TOGGLE_BUBBLE), Some(TrayEvent::ToggleBubble));
        assert_eq!(t.command_event(ID_MODE_ICON), Some(TrayEvent::BubbleMode("icon".into())));
        assert_eq!(t.command_event(ID_MODE_TRAY), Some(TrayEvent::BubbleMode("tray".into())));
        assert_eq!(t.command_event(ID_SETTINGS), Some(TrayEvent::OpenSettings));
        assert_eq!(t.command_event(ID_QUIT), Some(TrayEvent::Quit));
        assert_eq!(t.command_event(ID_ACCOUNT_BASE + 1), Some(TrayEvent::OpenAccount("w".into())));
        assert_eq!(t.command_event(ID_ACCOUNT_BASE + 2), None);
    }

    #[test]
    fn callback_events() {
        let t = fake(&[]);
        let cb = |ev: u32| t.handle(WM_TRAY_CALLBACK, WPARAM(0), LPARAM(((TRAY_UID << 16) | ev) as isize));
        assert_eq!(cb(NIN_SELECT), Some(TrayEvent::LeftClick));
        assert_eq!(cb(NIN_KEYSELECT), Some(TrayEvent::LeftClick));
        assert_eq!(cb(NIN_KEYSELECT), None, "duplicate Enter collapsed");
        assert_eq!(cb(0x0201 /* WM_LBUTTONDOWN */), None);
        assert_eq!(t.handle(WM_APP + 5, WPARAM(0), LPARAM(NIN_SELECT as isize)), None);
        assert_eq!(signed_words(0xFFF6_0010), (16, -10));
    }

    #[test]
    fn builds_menu_and_labels() {
        let mut t = fake(&[("a", "R&D", true), ("b", "Work", false)]);
        t.set_menu(&[("a".into(), "R&D".into(), true)], false, "tray");
        assert_eq!(t.bubble_mode, "tray");
        let (menu, _) = t.build_menu().expect("menu");
        // SAFETY: destroying the menu we just built.
        unsafe {
            let _ = DestroyMenu(menu);
        }
        assert_eq!(win::from_wide(&menu_label("R&D")), "R&&D");
        t.set_menu(&[], true, "bogus");
        assert_eq!(t.bubble_mode, "icon");
        let (menu, _) = t.build_menu().expect("empty menu");
        // SAFETY: as above.
        unsafe {
            let _ = DestroyMenu(menu);
        }
    }

    #[test]
    fn create_and_drop_on_message_window() {
        let hwnd = crate::util::ui_thread::create_msg_window(None);
        assert!(!hwnd.is_invalid());
        // A null icon still exercises the add/remove path; dropping removes the icon again.
        let t = Tray::create(hwnd, HICON::default()).expect("tray");
        drop(t);
        // SAFETY: destroying the message window this test created on this thread.
        unsafe {
            let _ = DestroyWindow(hwnd);
        }
    }
}

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

//! Marshalling work onto the UI thread through a hidden message-only window.
//!
//! API:
//! `pub const WM_SESSIONMUX_TASK: u32; pub fn post(hwnd_msg: HWND, f: Box<dyn FnOnce() + Send>);`
//! `pub fn create_msg_window(wndproc) -> HWND`
//!
//! `create_msg_window` registers class `SessionmuxMsgWindow` and creates a message-only window
//! (parent `HWND_MESSAGE`). Its internal window procedure runs `WM_SESSIONMUX_TASK` closures itself
//! and forwards every other message to the caller's `wndproc` (stored in GWLP_USERDATA), or to
//! `DefWindowProcW` when none is given. Returns a null HWND on failure (logged).
//!
//! Single-instance hand-off: message-only windows are invisible to `FindWindowW`, so a second
//! instance must use [`find_msg_window`] (FindWindowExW with HWND_MESSAGE) and post
//! [`WM_SESSIONMUX_SHOW_PANEL`].
//!
//! A `WM_SESSIONMUX_TASK` with a wrong key or an unknown id never runs anything; it only calls
//! the hook set with [`set_forged_hook`] (the app plays its tamper gesture there). One whose key
//! and id are both zero is treated as stray garbage: logged, never passed to the hook.

use crate::brand;
use crate::util::{log, win};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};
use windows::core::PCWSTR;
use windows::Win32::Foundation::{GetLastError, ERROR_CLASS_ALREADY_EXISTS, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, FindWindowExW, GetWindowLongPtrW, PostMessageW, RegisterClassExW,
    SetWindowLongPtrW, GWLP_USERDATA, HWND_MESSAGE, WINDOW_EX_STYLE, WINDOW_STYLE, WM_APP, WNDCLASSEXW, WNDPROC,
};

/// Run a closure queued by [`post`] (wparam = per-process key, lparam = task id). The closure
/// itself never leaves this process: the message carries only an id into [`TASKS`], so a
/// WM_APP+1 posted by anyone else (another process, a stray sender) is ignored.
pub const WM_SESSIONMUX_TASK: u32 = WM_APP + 1;
/// Sent by a second instance: show the panel.
pub const WM_SESSIONMUX_SHOW_PANEL: u32 = WM_APP + 2;
/// Sent by a `--relay` launch whose link was refused: play the tamper gesture (cosmetic only).
pub const WM_SESSIONMUX_GESTURE: u32 = WM_APP + 3;

/// Window class of the message window.
pub const MSG_WINDOW_CLASS: &str = brand::CLASS_MSG;

type Task = Box<dyn FnOnce() + Send>;

/// Pending tasks by id.
static TASKS: Mutex<Option<HashMap<usize, Task>>> = Mutex::new(None);
static NEXT_ID: AtomicUsize = AtomicUsize::new(1);
/// Called on the UI thread for every forged `WM_SESSIONMUX_TASK`.
static FORGED_HOOK: Mutex<Option<fn()>> = Mutex::new(None);
/// Forged task messages seen by this process.
static FORGED_COUNT: AtomicUsize = AtomicUsize::new(0);

/// Set (or clear) the function called when a forged task message arrives. It runs on the
/// thread that owns the message window and must return quickly.
pub fn set_forged_hook(f: Option<fn()>) {
    if let Ok(mut g) = FORGED_HOOK.lock() {
        *g = f;
    }
}

/// Whether an ignored task message counts as forged (reaches the hook): anything but an
/// all-zero key and id, which a crafted sender would not bother to produce.
fn reaches_hook(wparam: WPARAM, lparam: LPARAM) -> bool {
    wparam.0 != 0 || lparam.0 != 0
}

fn forged(what: &str, wparam: WPARAM, lparam: LPARAM) {
    log::debug(&format!("ui_thread: ignored WM_SESSIONMUX_TASK ({what})"));
    if !reaches_hook(wparam, lparam) {
        return;
    }
    FORGED_COUNT.fetch_add(1, Ordering::Relaxed);
    let hook = FORGED_HOOK.lock().ok().and_then(|g| *g);
    if let Some(f) = hook {
        f();
    }
}

/// Per-process key carried in wparam (not guessable from outside without reading our memory).
fn key() -> usize {
    static KEY: OnceLock<usize> = OnceLock::new();
    *KEY.get_or_init(|| {
        use std::hash::{BuildHasher, Hasher};
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u32(std::process::id());
        (h.finish() as usize) | 1
    })
}

/// Queue `f` to run on the thread that owns `hwnd_msg`. Never blocks. If posting fails (window
/// destroyed, queue full) the closure is dropped and the failure is logged.
pub fn post(hwnd_msg: HWND, f: Box<dyn FnOnce() + Send>) {
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    if let Ok(mut g) = TASKS.lock() {
        g.get_or_insert_with(HashMap::new).insert(id, f);
    } else {
        log::warn("ui_thread::post: task table poisoned");
        return;
    }
    // SAFETY: PostMessageW only copies the integer parameters.
    let res = unsafe { PostMessageW(hwnd_msg, WM_SESSIONMUX_TASK, WPARAM(key()), LPARAM(id as isize)) };
    if let Err(e) = res {
        drop(take_task(id));
        log::warn(&format!("ui_thread::post failed: {e}"));
    }
}

fn take_task(id: usize) -> Option<Task> {
    // A poisoned table still holds our own tasks: a real id must never look forged.
    let mut g = TASKS.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    g.as_mut().and_then(|m| m.remove(&id))
}

/// Run the task a `WM_SESSIONMUX_TASK` message refers to; anything not produced by [`post`] in this
/// process (wrong key, unknown or already-run id) is ignored.
pub fn run_task(wparam: WPARAM, lparam: LPARAM) {
    if wparam.0 != key() {
        forged("wrong key", wparam, lparam);
        return;
    }
    // Take it out before running so the lock is not held while the task runs (it may post).
    match take_task(lparam.0 as usize) {
        Some(task) => task(),
        None => forged("unknown id", wparam, lparam),
    }
}

unsafe extern "system" fn msg_wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if msg == WM_SESSIONMUX_TASK {
        run_task(wparam, lparam);
        return LRESULT(0);
    }
    // SAFETY: GWLP_USERDATA holds either 0 or a `WNDPROC` function pointer stored by
    // create_msg_window; transmuting it back to the same fn-pointer type is sound.
    unsafe {
        let user = GetWindowLongPtrW(hwnd, GWLP_USERDATA);
        if user != 0 {
            let f: unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT = std::mem::transmute(user);
            return f(hwnd, msg, wparam, lparam);
        }
        DefWindowProcW(hwnd, msg, wparam, lparam)
    }
}

static CLASS_REGISTERED: AtomicBool = AtomicBool::new(false);

/// Create the hidden message-only window on the current thread. `wndproc` receives every
/// message except `WM_SESSIONMUX_TASK`. Returns a null HWND on failure.
pub fn create_msg_window(wndproc: WNDPROC) -> HWND {
    let class = win::wide(MSG_WINDOW_CLASS);
    let hinst = win::hinstance();
    if !CLASS_REGISTERED.load(Ordering::Acquire) {
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(msg_wndproc),
            hInstance: hinst,
            lpszClassName: PCWSTR(class.as_ptr()),
            ..Default::default()
        };
        // SAFETY: `wc` is fully initialised and `class` outlives the call.
        let atom = unsafe { RegisterClassExW(&wc) };
        // Two threads (parallel tests) may race to register; "already exists" is success.
        // SAFETY: GetLastError reads this thread's last-error value set by RegisterClassExW.
        if atom == 0 && unsafe { GetLastError() } != ERROR_CLASS_ALREADY_EXISTS {
            log::error(&format!("RegisterClassExW({MSG_WINDOW_CLASS}) failed: {}", win::last_error()));
            return HWND::default();
        }
        CLASS_REGISTERED.store(true, Ordering::Release);
    }
    // SAFETY: class registered above; strings outlive the call; HWND_MESSAGE makes a
    // message-only window with no visible surface.
    let created = unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            PCWSTR(class.as_ptr()),
            PCWSTR(class.as_ptr()),
            WINDOW_STYLE(0),
            0,
            0,
            0,
            0,
            HWND_MESSAGE,
            None,
            hinst,
            None,
        )
    };
    match created {
        Ok(hwnd) => {
            if let Some(f) = wndproc {
                // SAFETY: hwnd is our freshly created window; we store a plain fn pointer that
                // msg_wndproc transmutes back to the identical type.
                unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, f as usize as isize) };
            }
            hwnd
        }
        Err(e) => {
            log::error(&format!("CreateWindowExW({MSG_WINDOW_CLASS}) failed: {e}"));
            HWND::default()
        }
    }
}

/// Find another instance's message window (message-only windows need FindWindowExW + HWND_MESSAGE).
pub fn find_msg_window() -> Option<HWND> {
    let class = win::wide(MSG_WINDOW_CLASS);
    // SAFETY: `class` outlives the call; FindWindowExW only reads it.
    let found = unsafe { FindWindowExW(HWND_MESSAGE, None, PCWSTR(class.as_ptr()), PCWSTR::null()) };
    found.ok().filter(|h| !h.is_invalid())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU32;
    use std::sync::Arc;
    use windows::Win32::UI::WindowsAndMessaging::{DestroyWindow, DispatchMessageW, PeekMessageW, MSG, PM_REMOVE};

    #[test]
    fn post_runs_on_owner_thread() {
        let hwnd = create_msg_window(None);
        assert!(!hwnd.is_invalid());
        let hits = Arc::new(AtomicU32::new(0));
        let h2 = hits.clone();
        let raw = hwnd.0 as isize;
        std::thread::spawn(move || {
            post(
                HWND(raw as _),
                Box::new(move || {
                    h2.fetch_add(1, Ordering::SeqCst);
                }),
            );
        })
        .join()
        .unwrap();
        // Forged messages (wrong key / unknown id) are ignored, not dereferenced.
        // SAFETY: posting plain integers to our own window.
        unsafe {
            let _ = PostMessageW(hwnd, WM_SESSIONMUX_TASK, WPARAM(0), LPARAM(0x1234_5678));
            let _ = PostMessageW(hwnd, WM_SESSIONMUX_TASK, WPARAM(key()), LPARAM(0x7FFF_FFF0));
            // All-zero key and id: stray garbage, ignored without reaching the hook.
            let _ = PostMessageW(hwnd, WM_SESSIONMUX_TASK, WPARAM(0), LPARAM(0));
        }
        let forged_before = FORGED_COUNT.load(Ordering::SeqCst);
        static HOOK_HITS: AtomicUsize = AtomicUsize::new(0);
        fn hook() {
            HOOK_HITS.fetch_add(1, Ordering::SeqCst);
        }
        set_forged_hook(Some(hook));
        let mut msg = MSG::default();
        // SAFETY: plain message pump on the thread that owns `hwnd`.
        unsafe {
            while PeekMessageW(&mut msg, hwnd, 0, 0, PM_REMOVE).as_bool() {
                DispatchMessageW(&msg);
            }
            let _ = DestroyWindow(hwnd);
        }
        assert_eq!(hits.load(Ordering::SeqCst), 1);
        // Both forgeries reached the forged-message path (the tamper trigger), the real task did not.
        set_forged_hook(None);
        assert!(FORGED_COUNT.load(Ordering::SeqCst) >= forged_before + 2);
        assert_eq!(HOOK_HITS.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn zero_task_message_is_not_forged() {
        assert!(!reaches_hook(WPARAM(0), LPARAM(0)));
        assert!(reaches_hook(WPARAM(0), LPARAM(1)));
        assert!(reaches_hook(WPARAM(1), LPARAM(0)));
        assert!(reaches_hook(WPARAM(key()), LPARAM(0x7FFF_FFF0)));
    }
}

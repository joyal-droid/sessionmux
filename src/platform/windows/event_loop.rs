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

//! The UI thread's loop on Windows: a hidden message-only window owned by the UI thread.
//!
//! - [`EventLoop::post`] marshals closures onto the UI thread through `WM_SESSIONMUX_TASK`.
//! - The window procedure turns the app's messages into `PlatformEvent`s: `WM_TIMER` → `Timer`,
//!   `WM_HOTKEY` → `Hotkey`, the tray icon's callbacks → `TrayNotify`, [`WM_SESSIONMUX_SHOW_PANEL`]
//!   (posted by a second instance) → `ShowPanel`, [`WM_SESSIONMUX_GESTURE`] (posted by a refused
//!   `--relay` launch) → `Gesture(BadLink)`, and `WM_CLOSE` (sent by the installer and the
//!   uninstaller) → `QuitRequested`.
//! - A foreground hook reports each change of the foreground window to another process (not the
//!   taskbar or its tray flyout) as `FrontChanged`.
//! - The tray icon and the global hotkey are bound to the same window ([`create_tray`],
//!   [`register_hotkey`]).
//!
//! `create_msg_window` registers class `SessionmuxMsgWindow` and creates a message-only window
//! (parent `HWND_MESSAGE`). Its internal window procedure runs `WM_SESSIONMUX_TASK` closures itself
//! and forwards every other message to the caller's `wndproc` (stored in GWLP_USERDATA), or to
//! `DefWindowProcW` when none is given. Returns a null HWND on failure (logged).
//!
//! Single-instance hand-off: a second instance finds this window through
//! `platform::windows::instance` and posts [`WM_SESSIONMUX_SHOW_PANEL`] to it.
//!
//! A `WM_SESSIONMUX_TASK` with a wrong key or an unknown id never runs anything; it only reaches
//! the app as `Gesture(ForgedMessage)` (the app plays its tamper gesture there). One whose key
//! and id are both zero is treated as stray garbage: logged, never passed on.

use crate::brand;
use crate::platform::windows::win;
use crate::platform::{Desktop, EventLoop, Os, PlatformEvent, Rect, TimerId, WindowId};
use crate::ui::gesture::Reason;
use crate::ui::tray::{self, Tray, TrayEvent};
use crate::util::log;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};
use windows::core::PCWSTR;
use windows::Win32::Foundation::{GetLastError, ERROR_CLASS_ALREADY_EXISTS, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MonitorFromRect, MONITORINFO, MONITOR_DEFAULTTONEAREST};
use windows::Win32::UI::Accessibility::{SetWinEventHook, UnhookWinEvent, HWINEVENTHOOK};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetForegroundWindow, GetMessageW,
    GetSystemMetrics, GetWindowLongPtrW, KillTimer, LoadImageW, PostMessageW, PostQuitMessage, RegisterClassExW,
    SetTimer, SetWindowLongPtrW, TranslateMessage, EVENT_SYSTEM_FOREGROUND, GWLP_USERDATA, HICON, HWND_MESSAGE,
    IMAGE_ICON, LR_DEFAULTCOLOR, MSG, SM_CXSMICON, SM_CYSMICON, WINDOW_EX_STYLE, WINDOW_STYLE, WINEVENT_OUTOFCONTEXT,
    WINEVENT_SKIPOWNPROCESS, WM_APP, WM_CLOSE, WM_HOTKEY, WM_TIMER, WNDCLASSEXW, WNDPROC,
};

/// Run a closure queued by [`post_to`] (wparam = per-process key, lparam = task id). The closure
/// itself never leaves this process: the message carries only an id into [`TASKS`], so a WM_APP+1
/// posted by anyone else (another process, a stray sender) is ignored.
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
/// Forged task messages seen by this process.
static FORGED_COUNT: AtomicUsize = AtomicUsize::new(0);
/// The app's message window (0 = none).
static APP_WINDOW: AtomicIsize = AtomicIsize::new(0);
/// The app's event handler.
static HANDLER: Mutex<Option<fn(PlatformEvent)>> = Mutex::new(None);

/// Hand `ev` to the app's handler, if one is set. The lock is not held while the handler runs:
/// it may open a modal loop that dispatches further events.
fn dispatch(ev: PlatformEvent) {
    let handler = HANDLER.lock().ok().and_then(|g| *g);
    if let Some(f) = handler {
        f(ev);
    }
}

/// Whether an ignored task message counts as forged (reaches the app): anything but an all-zero
/// key and id, which a crafted sender would not bother to produce.
fn reaches_hook(wparam: WPARAM, lparam: LPARAM) -> bool {
    wparam.0 != 0 || lparam.0 != 0
}

fn forged(what: &str, wparam: WPARAM, lparam: LPARAM) {
    log::debug(&format!("event_loop: ignored WM_SESSIONMUX_TASK ({what})"));
    if !reaches_hook(wparam, lparam) {
        return;
    }
    FORGED_COUNT.fetch_add(1, Ordering::Relaxed);
    dispatch(PlatformEvent::Gesture(Reason::ForgedMessage));
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
pub fn post_to(hwnd_msg: HWND, f: Task) {
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    if let Ok(mut g) = TASKS.lock() {
        g.get_or_insert_with(HashMap::new).insert(id, f);
    } else {
        log::warn("event_loop::post: task table poisoned");
        return;
    }
    // SAFETY: PostMessageW only copies the integer parameters.
    let res = unsafe { PostMessageW(hwnd_msg, WM_SESSIONMUX_TASK, WPARAM(key()), LPARAM(id as isize)) };
    if let Err(e) = res {
        drop(take_task(id));
        log::warn(&format!("event_loop::post failed: {e}"));
    }
}

fn take_task(id: usize) -> Option<Task> {
    // A poisoned table still holds our own tasks: a real id must never look forged.
    let mut g = TASKS.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    g.as_mut().and_then(|m| m.remove(&id))
}

/// Run the task a `WM_SESSIONMUX_TASK` message refers to; anything not produced by [`post_to`] in
/// this process (wrong key, unknown or already-run id) is ignored.
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

/// Find another instance's message window.
pub fn find_msg_window() -> Option<HWND> {
    crate::platform::windows::instance::find_msg_window()
}

/// The app's message window; null before `init` and after `shutdown`.
fn app_window() -> HWND {
    HWND(APP_WINDOW.load(Ordering::Acquire) as _)
}

/// A tray icon notification as it reached the message window, for [`tray_event`].
pub struct TrayNotify {
    msg: u32,
    wparam: usize,
    lparam: isize,
}

/// Let the tray read a notification: the event it stands for, if any. A right click runs the
/// menu's modal loop inside this call.
pub fn tray_event(t: &Tray, n: &TrayNotify) -> Option<TrayEvent> {
    t.handle(n.msg, WPARAM(n.wparam), LPARAM(n.lparam))
}

/// The timer ids on the message window.
fn timer_number(id: TimerId) -> usize {
    match id {
        TimerId::DestroyPanel => 101,
        TimerId::Pin => 102,
        TimerId::StartupTrim => 103,
        TimerId::SignIn => 104,
    }
}

fn timer_of(n: usize) -> Option<TimerId> {
    [TimerId::DestroyPanel, TimerId::Pin, TimerId::StartupTrim, TimerId::SignIn]
        .into_iter()
        .find(|&id| timer_number(id) == n)
}

/// The app's messages as events; None for everything else.
fn translate(msg: u32, wparam: WPARAM, lparam: LPARAM) -> Option<PlatformEvent> {
    Some(match msg {
        WM_SESSIONMUX_SHOW_PANEL => PlatformEvent::ShowPanel,
        WM_SESSIONMUX_GESTURE => PlatformEvent::Gesture(Reason::BadLink),
        WM_CLOSE => PlatformEvent::QuitRequested,
        WM_HOTKEY => PlatformEvent::Hotkey(wparam.0 as u32),
        WM_TIMER => PlatformEvent::Timer(timer_of(wparam.0)?),
        tray::WM_TRAY_CALLBACK | tray::WM_TRAY_RECREATE => {
            PlatformEvent::TrayNotify(TrayNotify { msg, wparam: wparam.0, lparam: lparam.0 })
        }
        _ => return None,
    })
}

unsafe extern "system" fn app_wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match translate(msg, wparam, lparam) {
        Some(ev) => {
            dispatch(ev);
            LRESULT(0)
        }
        // A timer we do not know is consumed, as the app's own timers are.
        None if msg == WM_TIMER => LRESULT(0),
        // SAFETY: default processing for a window we own, with the original arguments.
        None => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

impl EventLoop for Os {
    fn init(on_event: fn(PlatformEvent)) -> Result<(), String> {
        if let Ok(mut g) = HANDLER.lock() {
            *g = Some(on_event);
        }
        let hwnd = create_msg_window(Some(app_wndproc));
        if hwnd.is_invalid() {
            return Err(format!("message window: {}", win::last_error()));
        }
        APP_WINDOW.store(hwnd.0 as isize, Ordering::Release);
        watch_foreground();
        Ok(())
    }

    fn run() -> i32 {
        let mut msg = MSG::default();
        // SAFETY: standard message loop on the thread that owns all our windows; `msg` is a valid
        // out-pointer for each call.
        unsafe {
            loop {
                let r = GetMessageW(&mut msg, None, 0, 0);
                if r.0 <= 0 {
                    if r.0 < 0 {
                        log::error(&format!("GetMessageW failed: {}", win::last_error()));
                    }
                    break;
                }
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        // WM_QUIT carries the exit code given to PostQuitMessage.
        msg.wParam.0 as i32
    }

    fn post(f: Box<dyn FnOnce() + Send>) {
        let hwnd = app_window();
        if hwnd.is_invalid() {
            log::warn("event_loop::post: no message window");
            return;
        }
        post_to(hwnd, f);
    }

    fn set_timer(id: TimerId, ms: u32) {
        let n = timer_number(id);
        // SAFETY: timer on our own message window; WM_TIMER is handled in app_wndproc.
        if unsafe { SetTimer(app_window(), n, ms, None) } == 0 {
            log::warn(&format!("SetTimer {n}: {}", win::last_error()));
        }
    }

    fn kill_timer(id: TimerId) {
        // SAFETY: as above; killing a timer that is not set is harmless.
        let _ = unsafe { KillTimer(app_window(), timer_number(id)) };
    }

    fn quit() {
        // SAFETY: posts WM_QUIT to the calling (UI) thread's queue; no preconditions.
        unsafe { PostQuitMessage(0) };
    }

    fn shutdown() {
        let hook = FOREGROUND_HOOK.swap(0, Ordering::AcqRel);
        if hook != 0 {
            // SAFETY: the hook was set on this thread by `init`; the swap above clears it first,
            // so it is removed once.
            let _ = unsafe { UnhookWinEvent(HWINEVENTHOOK(hook as _)) };
        }
        let h = APP_WINDOW.swap(0, Ordering::AcqRel);
        if h != 0 {
            // SAFETY: the window was created on this thread by `init`; the swap above clears it
            // first, so it is destroyed once.
            let _ = unsafe { DestroyWindow(HWND(h as _)) };
        }
        if let Ok(mut g) = HANDLER.lock() {
            *g = None;
        }
    }
}

/// The hook reporting each change of the foreground window (0 = none).
static FOREGROUND_HOOK: AtomicIsize = AtomicIsize::new(0);

/// Report every change of the foreground window to another process as `FrontChanged`, from the
/// UI thread's message loop. A failure is logged; the app then reads the foreground itself.
fn watch_foreground() {
    // SAFETY: an out-of-context hook needs no module; its callback runs on this thread while it
    // waits for messages, and is removed in `shutdown`.
    let hook = unsafe {
        SetWinEventHook(
            EVENT_SYSTEM_FOREGROUND,
            EVENT_SYSTEM_FOREGROUND,
            None,
            Some(on_foreground),
            0,
            0,
            WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
        )
    };
    if hook.is_invalid() {
        log::warn(&format!("foreground changes not reported: {}", win::last_error()));
        return;
    }
    FOREGROUND_HOOK.store(hook.0 as isize, Ordering::Release);
}

unsafe extern "system" fn on_foreground(_: HWINEVENTHOOK, _: u32, hwnd: HWND, _: i32, _: i32, _: u32, _: u32) {
    if let Some(pid) = super::window::user_window_pid(hwnd) {
        // Posted, so the app handles it as any other task, outside of whatever is running now.
        <Os as EventLoop>::post(Box::new(move || dispatch(PlatformEvent::FrontChanged(pid))));
    }
}

/// The tray icon, bound to the message window; its notifications arrive as `TrayNotify`.
pub fn create_tray() -> Result<Tray, String> {
    Tray::create(app_window(), load_tray_icon())
}

fn load_tray_icon() -> HICON {
    // SAFETY: loads icon resource IDI_TRAY embedded by build.rs from our own module.
    unsafe {
        let cx = GetSystemMetrics(SM_CXSMICON);
        let cy = GetSystemMetrics(SM_CYSMICON);
        match LoadImageW(win::hinstance(), win::make_int_resource(win::IDI_TRAY), IMAGE_ICON, cx, cy, LR_DEFAULTCOLOR) {
            Ok(h) => HICON(h.0),
            Err(e) => {
                log::warn(&format!("tray icon load failed: {e}"));
                HICON::default()
            }
        }
    }
}

/// Register the global hotkey `spec` ("Ctrl+Shift+Space") on the message window; it arrives as
/// `Hotkey(id)`.
pub fn register_hotkey(id: u32, spec: &str) -> Result<(), String> {
    crate::ui::hotkey::register(app_window(), id as i32, spec)
}

pub fn unregister_hotkey(id: u32) {
    crate::ui::hotkey::unregister(app_window(), id as i32);
}

impl From<RECT> for Rect {
    fn from(r: RECT) -> Rect {
        Rect { x: r.left, y: r.top, w: r.right - r.left, h: r.bottom - r.top }
    }
}

impl From<Rect> for RECT {
    fn from(r: Rect) -> RECT {
        RECT { left: r.x, top: r.y, right: r.x + r.w, bottom: r.y + r.h }
    }
}

impl Desktop for Os {
    fn work_area(r: &Rect) -> Rect {
        let rc = RECT::from(*r);
        let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        // SAFETY: `rc` and `mi` are valid for the duration of the calls; MonitorFromRect with
        // DEFAULTTONEAREST always returns a monitor; GetMonitorInfoW only writes into `mi`.
        let ok = unsafe { GetMonitorInfoW(MonitorFromRect(&rc, MONITOR_DEFAULTTONEAREST), &mut mi).as_bool() };
        if ok {
            Rect::from(mi.rcWork)
        } else {
            *r
        }
    }

    fn units_per_dip(r: &Rect) -> f32 {
        let rc = RECT::from(*r);
        let (mut x, mut y) = (0u32, 0u32);
        // SAFETY: `rc`, `x` and `y` are valid for the duration of the calls; MonitorFromRect with
        // DEFAULTTONEAREST always returns a monitor.
        let dpi = unsafe {
            GetDpiForMonitor(MonitorFromRect(&rc, MONITOR_DEFAULTTONEAREST), MDT_EFFECTIVE_DPI, &mut x, &mut y)
        };
        if dpi.is_ok() && x != 0 {
            x as f32 / 96.0
        } else {
            win::dpi_scale(HWND::default())
        }
    }

    fn is_foreground(w: WindowId) -> bool {
        // SAFETY: no preconditions.
        let fg = unsafe { GetForegroundWindow() };
        !fg.is_invalid() && fg.0 as usize == w.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU32;
    use std::sync::Arc;
    use windows::Win32::UI::WindowsAndMessaging::{PeekMessageW, PM_REMOVE};

    #[test]
    fn post_runs_on_owner_thread() {
        let hwnd = create_msg_window(None);
        assert!(!hwnd.is_invalid());
        let hits = Arc::new(AtomicU32::new(0));
        let h2 = hits.clone();
        let raw = hwnd.0 as isize;
        std::thread::spawn(move || {
            post_to(
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
            // All-zero key and id: stray garbage, ignored without reaching the app.
            let _ = PostMessageW(hwnd, WM_SESSIONMUX_TASK, WPARAM(0), LPARAM(0));
        }
        let forged_before = FORGED_COUNT.load(Ordering::SeqCst);
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
        assert!(FORGED_COUNT.load(Ordering::SeqCst) >= forged_before + 2);
    }

    #[test]
    fn zero_task_message_is_not_forged() {
        assert!(!reaches_hook(WPARAM(0), LPARAM(0)));
        assert!(reaches_hook(WPARAM(0), LPARAM(1)));
        assert!(reaches_hook(WPARAM(1), LPARAM(0)));
        assert!(reaches_hook(WPARAM(key()), LPARAM(0x7FFF_FFF0)));
    }

    #[test]
    fn messages_become_events() {
        let ev = |msg: u32, w: usize| translate(msg, WPARAM(w), LPARAM(0));
        assert!(matches!(ev(WM_SESSIONMUX_SHOW_PANEL, 0), Some(PlatformEvent::ShowPanel)));
        assert!(matches!(ev(WM_SESSIONMUX_GESTURE, 0), Some(PlatformEvent::Gesture(Reason::BadLink))));
        assert!(matches!(ev(WM_CLOSE, 0), Some(PlatformEvent::QuitRequested)));
        assert!(matches!(ev(WM_HOTKEY, 1), Some(PlatformEvent::Hotkey(1))));
        assert!(matches!(ev(WM_TIMER, 104), Some(PlatformEvent::Timer(TimerId::SignIn))));
        assert!(matches!(ev(tray::WM_TRAY_CALLBACK, 0), Some(PlatformEvent::TrayNotify(_))));
        assert!(matches!(ev(tray::WM_TRAY_RECREATE, 0), Some(PlatformEvent::TrayNotify(_))));
        assert!(ev(WM_TIMER, 999).is_none());
        assert!(ev(WM_APP + 0x200, 0).is_none());
    }

    #[test]
    fn timer_ids_round_trip() {
        for id in [TimerId::DestroyPanel, TimerId::Pin, TimerId::StartupTrim, TimerId::SignIn] {
            assert_eq!(timer_of(timer_number(id)), Some(id));
        }
        assert_eq!(timer_of(0), None);
    }

    #[test]
    fn rects_convert_both_ways() {
        let r = Rect { x: -10, y: 20, w: 300, h: 40 };
        let rc = RECT::from(r);
        assert_eq!((rc.left, rc.top, rc.right, rc.bottom), (-10, 20, 290, 60));
        assert_eq!(Rect::from(rc), r);
        assert_eq!(RECT::from(Rect::default()), RECT::default());
        assert!(<Os as Desktop>::units_per_dip(&r) >= 1.0);
        let wa = <Os as Desktop>::work_area(&Rect { x: 0, y: 0, w: 1, h: 1 });
        assert!(wa.w > 0 && wa.h > 0);
    }
}

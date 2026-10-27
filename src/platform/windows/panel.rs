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

//! The panel's window on Windows: WS_POPUP `PANEL_W`×`PANEL_H` DIP (scaled to the target
//! monitor's DPI), WS_EX_TOOLWINDOW | WS_EX_TOPMOST, DWM rounded corners, opaque class background
//! #101114. WebView2 (through wry) is built into it as a child.
//!
//! `WM_ACTIVATE` (inactive while visible) reaches the app as the synthetic request `__blur`,
//! `WM_CLOSE` (Alt+F4) as `hidePanel`.

use crate::brand;
use crate::platform::windows::win;
use crate::platform::{PanelHost, Rect, WindowId};
use crate::ui::ipc::{IpcHandler, IpcRequest};
use crate::ui::panel::{rect_beside, rect_near_point, scaled, PANEL_GAP, PANEL_H, PANEL_W};
use crate::util::log;
use raw_window_handle::{HandleError, HasWindowHandle, RawWindowHandle, Win32WindowHandle, WindowHandle};
use std::num::NonZeroIsize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND};
use windows::Win32::Graphics::Gdi::{
    CreateSolidBrush, GetMonitorInfoW, MonitorFromPoint, MonitorFromRect, UpdateWindow, HBRUSH, HMONITOR, MONITORINFO,
    MONITOR_DEFAULTTONEAREST, MONITOR_DEFAULTTOPRIMARY,
};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetCursorPos, GetWindowLongPtrW, IsWindowVisible, LoadCursorW,
    RegisterClassExW, SetForegroundWindow, SetWindowLongPtrW, SetWindowPos, ShowWindow, GWLP_USERDATA, HWND_TOPMOST,
    IDC_ARROW, SWP_NOACTIVATE, SWP_SHOWWINDOW, SW_HIDE, WA_INACTIVE, WM_ACTIVATE, WM_CLOSE, WM_DPICHANGED,
    WM_NCDESTROY, WNDCLASSEXW, WS_CLIPCHILDREN, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};
use wry::{WebView, WebViewBuilder, WebViewBuilderExtWindows};

const CLASS_NAME: &str = brand::CLASS_PANEL;
/// #101114 as COLORREF (0x00BBGGRR).
const BG_COLORREF: u32 = 0x0014_1110;
/// WebView2 browser arguments (replaces wry's default, which is the first switch).
/// No GPU process and no GPU heaps (software compositing is plenty for one 380x580 card), the
/// network service inside the browser process, one renderer.
const BROWSER_ARGS: &str = "--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection --disable-gpu --disable-gpu-compositing --enable-features=NetworkServiceInProcess2 --renderer-process-limit=1 --disable-background-networking";

fn monitor_area_and_scale(mon: HMONITOR) -> (Rect, f32) {
    let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
    // SAFETY: `mi` is a properly sized out-struct; an invalid monitor handle only makes it fail.
    let area = if unsafe { GetMonitorInfoW(mon, &mut mi) }.as_bool() {
        Rect::from(mi.rcWork)
    } else {
        Rect { x: 0, y: 0, w: 1920, h: 1080 }
    };
    let (mut dx, mut dy) = (0u32, 0u32);
    // SAFETY: valid out-pointers; failure leaves 0 and we fall back to the system DPI.
    let scale = match unsafe { GetDpiForMonitor(mon, MDT_EFFECTIVE_DPI, &mut dx, &mut dy) } {
        Ok(()) if dx > 0 => dx as f32 / 96.0,
        _ => win::dpi_scale(HWND::default()),
    };
    (area, scale)
}

// ---------------------------------------------------------------- window class

struct WndData {
    ipc: Arc<dyn IpcHandler>,
}

fn synthetic(ipc: &Arc<dyn IpcHandler>, name: &str) {
    ipc.handle(IpcRequest { id: 0, name: name.to_string(), args: Vec::new() });
}

unsafe extern "system" fn panel_wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    // SAFETY: GWLP_USERDATA is 0 or a `Box<WndData>` pointer installed by PanelWindow::create and
    // freed only in WM_NCDESTROY below, so dereferencing it here is valid.
    let data = unsafe { (GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const WndData).as_ref() };
    match msg {
        WM_ACTIVATE => {
            // SAFETY: IsWindowVisible tolerates any handle.
            if (wparam.0 & 0xFFFF) as u32 == WA_INACTIVE && unsafe { IsWindowVisible(hwnd) }.as_bool() {
                if let Some(d) = data {
                    synthetic(&d.ipc, "__blur");
                }
            }
        }
        WM_CLOSE => {
            // Alt+F4: let the app hide (and later destroy) the panel through its normal path.
            if let Some(d) = data {
                synthetic(&d.ipc, "hidePanel");
            }
            return LRESULT(0);
        }
        WM_DPICHANGED => {
            // show_near sizes the window for the target monitor itself; ignore the suggestion.
            return LRESULT(0);
        }
        WM_NCDESTROY => {
            // SAFETY: reclaim the Box installed by PanelWindow::create exactly once, then clear
            // the slot.
            unsafe {
                let p = SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0) as *mut WndData;
                if !p.is_null() {
                    drop(Box::from_raw(p));
                }
            }
        }
        _ => {}
    }
    // SAFETY: default processing with the original arguments for a window we own.
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

static CLASS_REGISTERED: AtomicBool = AtomicBool::new(false);

fn register_class() -> Result<(), String> {
    if CLASS_REGISTERED.load(Ordering::Acquire) {
        return Ok(());
    }
    let class = win::wide(CLASS_NAME);
    // SAFETY: creating a solid brush and loading a system cursor have no preconditions; the brush
    // lives for the rest of the process as the class background.
    let (brush, cursor) =
        unsafe { (CreateSolidBrush(COLORREF(BG_COLORREF)), LoadCursorW(None, IDC_ARROW).unwrap_or_default()) };
    let wc = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        lpfnWndProc: Some(panel_wndproc),
        hInstance: win::hinstance(),
        hCursor: cursor,
        hbrBackground: HBRUSH(brush.0),
        lpszClassName: PCWSTR(class.as_ptr()),
        ..Default::default()
    };
    // SAFETY: `wc` is fully initialised and `class` outlives the call.
    if unsafe { RegisterClassExW(&wc) } == 0 {
        return Err(format!("RegisterClassExW({CLASS_NAME}): {}", win::last_error()));
    }
    CLASS_REGISTERED.store(true, Ordering::Release);
    Ok(())
}

/// Where the panel goes for `anchor` (see `show_near`), in physical pixels.
fn near_rect(anchor: Rect, side_pref: &str) -> Rect {
    let has_rect = anchor.w > 0 && anchor.h > 0;
    if has_rect {
        let rc = RECT::from(anchor);
        // SAFETY: MonitorFromRect reads the RECT; DEFAULTTONEAREST always yields a monitor.
        let mon = unsafe { MonitorFromRect(&rc, MONITOR_DEFAULTTONEAREST) };
        let (area, s) = monitor_area_and_scale(mon);
        rect_beside(anchor, area, side_pref, scaled(PANEL_W, s), scaled(PANEL_H, s), scaled(PANEL_GAP, s))
    } else {
        let mut pt = POINT { x: anchor.x, y: anchor.y };
        if pt.x == 0 && pt.y == 0 {
            // SAFETY: valid out-pointer; on failure pt stays (0,0).
            let _ = unsafe { GetCursorPos(&mut pt) };
        }
        // SAFETY: MonitorFromPoint takes the point by value.
        let mon = unsafe { MonitorFromPoint(pt, MONITOR_DEFAULTTONEAREST) };
        let (area, s) = monitor_area_and_scale(mon);
        rect_near_point((pt.x, pt.y), area, scaled(PANEL_W, s), scaled(PANEL_H, s), scaled(PANEL_GAP, s))
    }
}

/// WebView2 enables its default context menu (Back/Reload/Save as/Print) unless told otherwise;
/// turn it off so a right-click outside the account rows does nothing. The controller and its
/// settings are `webview2-com` types, as wry hands them out.
fn disable_default_context_menu(webview: &WebView) {
    use wry::WebViewExtWindows;
    let controller = webview.controller();
    // SAFETY: COM calls on the live controller of the WebView we just built, on its UI thread.
    let r = unsafe {
        controller
            .CoreWebView2()
            .and_then(|core| core.Settings())
            .and_then(|s| s.SetAreDefaultContextMenusEnabled(false))
    };
    if let Err(e) = r {
        log::warn(&format!("panel: could not disable the default context menu: {e}"));
    }
}

// ---------------------------------------------------------------- PanelWindow

/// The panel's top-level window. Not `Send`: it lives on the UI thread that created it.
pub struct PanelWindow {
    hwnd: HWND,
}

impl PanelWindow {
    fn place(&self, r: Rect, activate: bool) {
        let mut flags = SWP_SHOWWINDOW;
        if !activate {
            flags |= SWP_NOACTIVATE;
        }
        // SAFETY: repositioning our own window; integers only.
        if let Err(e) = unsafe { SetWindowPos(self.hwnd, HWND_TOPMOST, r.x, r.y, r.w, r.h, flags) } {
            log::warn(&format!("panel: SetWindowPos failed: {e}"));
        }
    }
}

/// The HWND for wry, which builds WebView2 into it while the window certainly exists.
impl HasWindowHandle for PanelWindow {
    fn window_handle(&self) -> Result<WindowHandle<'_>, HandleError> {
        let hwnd = NonZeroIsize::new(self.hwnd.0 as isize).ok_or(HandleError::Unavailable)?;
        let mut h = Win32WindowHandle::new(hwnd);
        h.hinstance = NonZeroIsize::new(win::hinstance().0 as isize);
        // SAFETY: the HWND is a live window owned by this PanelWindow, which outlives the
        // borrow; wry only uses the handle during `build`.
        Ok(unsafe { WindowHandle::borrow_raw(RawWindowHandle::Win32(h)) })
    }
}

impl PanelHost for PanelWindow {
    const ENGINE: &'static str = "WebView2";

    fn create(ipc: Arc<dyn IpcHandler>, near: Option<(Rect, &str)>) -> Result<Self, String> {
        register_class()?;
        let class = win::wide(CLASS_NAME);
        let title = win::wide(brand::NAME);
        let scale = win::dpi_scale(HWND::default());
        // SAFETY: class registered above; strings outlive the call. The window starts hidden.
        let hwnd = unsafe {
            CreateWindowExW(
                WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
                PCWSTR(class.as_ptr()),
                PCWSTR(title.as_ptr()),
                WS_POPUP | WS_CLIPCHILDREN,
                -32000,
                -32000,
                scaled(PANEL_W, scale),
                scaled(PANEL_H, scale),
                None,
                None,
                win::hinstance(),
                None,
            )
        }
        .map_err(|e| format!("CreateWindowExW({CLASS_NAME}): {e}"))?;

        let data = Box::into_raw(Box::new(WndData { ipc }));
        // SAFETY: hwnd is our new window; ownership of `data` passes to it and is reclaimed in
        // WM_NCDESTROY.
        unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, data as isize) };

        let pref = DWMWCP_ROUND;
        // SAFETY: passes a pointer to a live 4-byte DWM_WINDOW_CORNER_PREFERENCE with its size.
        if let Err(e) = unsafe {
            DwmSetWindowAttribute(
                hwnd,
                DWMWA_WINDOW_CORNER_PREFERENCE,
                &pref as *const _ as *const core::ffi::c_void,
                std::mem::size_of_val(&pref) as u32,
            )
        } {
            log::debug(&format!("panel: rounded corners unavailable: {e}"));
        }

        if let Some((anchor, side)) = near {
            let r = near_rect(anchor, side);
            // SAFETY: positioning and showing our own new window without activating it.
            unsafe {
                let _ = SetWindowPos(hwnd, HWND_TOPMOST, r.x, r.y, r.w, r.h, SWP_SHOWWINDOW | SWP_NOACTIVATE);
                let _ = UpdateWindow(hwnd);
            }
        }
        Ok(PanelWindow { hwnd })
    }

    fn configure(builder: WebViewBuilder<'_>) -> WebViewBuilder<'_> {
        builder
            .with_browser_accelerator_keys(false)
            .with_https_scheme(true)
            // Footprint: wry's default feature switches, plus the GPU work in the
            // browser process and a single renderer — the panel is one small static page.
            .with_additional_browser_args(BROWSER_ARGS)
    }

    fn after_build(&self, webview: &WebView) {
        disable_default_context_menu(webview);
    }

    fn show_near(&self, anchor: Rect, side: &str) {
        self.place(near_rect(anchor, side), true);
    }

    fn show_inactive_at(&self, x: i32, y: i32) {
        // SAFETY: MonitorFromPoint takes the point by value.
        let mon = unsafe { MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY) };
        let (_, s) = monitor_area_and_scale(mon);
        self.place(Rect { x, y, w: scaled(PANEL_W, s), h: scaled(PANEL_H, s) }, false);
    }

    fn activate(&self) {
        // SAFETY: plain call on our own window; failure (foreground lock) is harmless.
        let _ = unsafe { SetForegroundWindow(self.hwnd) };
    }

    fn hide(&self) {
        // SAFETY: hiding our own window; the return value is the previous visibility.
        let _ = unsafe { ShowWindow(self.hwnd, SW_HIDE) };
    }

    fn is_visible(&self) -> bool {
        // SAFETY: IsWindowVisible tolerates any handle.
        unsafe { IsWindowVisible(self.hwnd) }.as_bool()
    }

    fn window(&self) -> WindowId {
        WindowId(self.hwnd.0 as usize)
    }

    fn destroy(self) {
        // SAFETY: our own window, destroyed once on its owning thread (PanelWindow is !Send and
        // consumed here).
        let _ = unsafe { DestroyWindow(self.hwnd) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::{Capture, Os};
    use crate::ui::panel::Panel;
    use crate::util::paths;
    use serde_json::json;
    use std::sync::Mutex;
    use windows::Win32::UI::WindowsAndMessaging::{DispatchMessageW, PeekMessageW, TranslateMessage, MSG, PM_REMOVE};

    #[derive(Default)]
    struct Recorder {
        reqs: Mutex<Vec<IpcRequest>>,
    }
    impl IpcHandler for Recorder {
        fn handle(&self, req: IpcRequest) {
            if let Ok(mut v) = self.reqs.lock() {
                v.push(req);
            }
        }
    }

    #[test]
    fn window_without_web_view() {
        let rec: Arc<dyn IpcHandler> = Arc::new(Recorder::default());
        let w = PanelWindow::create(rec, None).expect("window");
        assert!(!w.is_visible());
        assert_ne!(w.window(), WindowId(0));
        w.show_inactive_at(-10_000, 100);
        assert!(w.is_visible());
        w.hide();
        assert!(!w.is_visible());
        w.destroy();
        // An anchor rect places the panel beside it, inside the work area.
        let r = near_rect(Rect { x: 200, y: 200, w: 80, h: 80 }, "right");
        assert!(r.w >= PANEL_W && r.h >= PANEL_H);
    }

    fn fake_account(id: &str, name: &str, color: &str, main: bool, running: bool) -> serde_json::Value {
        json!({"id": id, "name": name, "dataDir": if main { serde_json::Value::Null } else { json!(format!("C:\\fake\\Claude-{name}")) },
               "color": color, "initials": &name[..1], "label": null, "isMain": main, "signedIn": true,
               "accountUuid": null, "running": running, "pid": if running { json!(1234) } else { serde_json::Value::Null },
               "sessionCount": 3, "codeReady": true, "sameSignInAs": null})
    }

    /// Pump messages for `ms`, answering page requests with canned data. Returns all requests seen.
    fn pump(panel: &Panel, rec: &Recorder, ms: u64, seen: &mut Vec<IpcRequest>) {
        let end = std::time::Instant::now() + std::time::Duration::from_millis(ms);
        while std::time::Instant::now() < end {
            let mut msg = MSG::default();
            // SAFETY: plain message pump on the thread that owns the panel window.
            unsafe {
                while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }
            let reqs: Vec<IpcRequest> = rec.reqs.lock().map(|mut v| std::mem::take(&mut *v)).unwrap_or_default();
            for q in reqs {
                let payload = match q.name.as_str() {
                    "listAccounts" => json!([
                        fake_account("main", "Main", "#F08A5D", true, true),
                        fake_account("w", "Work", "#5B9CF6", false, false),
                        fake_account("p", "Personal", "#3DD6A1", false, true),
                    ]),
                    "getSettings" => json!({"startWithWindows": false, "shareMcpConfig": true, "showBubble": true,
                        "bubblePosition": null, "theme": "dark", "hotkey": "Ctrl+Shift+Space", "panelSide": "auto",
                        "bubbleMode": "icon", "appVersion": "1.0.0", "paths": {"store": "C:\\x\\sessionmux.json",
                        "log": "C:\\x\\sessionmux.log", "transcripts": "C:\\x\\.claude\\projects", "mainDataDir": "C:\\x"}}),
                    "listDiscovered" | "listSessions" => json!([]),
                    _ => json!({"ok": true}),
                };
                panel.resolve(q.id, true, &payload);
                seen.push(q);
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    /// Manual check: `cargo test -p sessionmux panel::tests::renders_real_ui -- --ignored --nocapture`.
    /// Creates the panel (never shown on screen: placed at x = -10000), waits for `__ready`,
    /// switches tabs and writes `%TEMP%\sessionmux-panel-<n>-<tab>.png`.
    #[test]
    #[ignore]
    fn renders_real_ui() {
        let tmp = tempfile::tempdir().unwrap();
        paths::set_user_data(Some(tmp.path().to_path_buf()));
        let rec = Arc::new(Recorder::default());
        let t0 = std::time::Instant::now();
        let panel = Panel::create(rec.clone()).expect("panel");
        println!("create: {} ms", t0.elapsed().as_millis());
        // Emitted before __ready: must be queued, then delivered.
        panel.emit("show-tab", &json!("settings"));
        let mut seen = Vec::new();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
        while !panel.is_ready() && std::time::Instant::now() < deadline {
            pump(&panel, &rec, 50, &mut seen);
        }
        assert!(panel.is_ready(), "no __ready; requests: {:?}", seen.iter().map(|r| &r.name).collect::<Vec<_>>());
        println!("ready: {} ms", t0.elapsed().as_millis());
        panel.show_inactive_at(-10_000, 100);
        let out = std::env::temp_dir();
        // Off-screen, Chromium only produces frames when asked: a throw-away capture starts the
        // card-in entrance animation, which has finished by the next capture.
        let _ = <Os as Capture>::panel_png(panel.window(), &out.join("sessionmux-panel-warmup.png"));
        for (i, tab) in ["accounts", "sessions", "settings"].into_iter().enumerate() {
            panel.emit("show-tab", &json!(tab));
            pump(&panel, &rec, 700, &mut seen);
            let path = out.join(format!("sessionmux-panel-{i}-{tab}.png"));
            let size = <Os as Capture>::panel_png(panel.window(), &path).expect("capture");
            println!("captured {} {:?}", path.display(), size);
        }
        let names: Vec<&str> = seen.iter().map(|r| r.name.as_str()).collect();
        println!("requests: {names:?}");
        let errors: Vec<&IpcRequest> = seen.iter().filter(|r| r.name == "__error").collect();
        assert!(errors.is_empty(), "page errors: {errors:?}");
        assert!(names.contains(&"__ready") && names.contains(&"listAccounts"));
        // window.onerror reaches the host as "__error".
        panel.eval("setTimeout(function(){throw new Error('sessionmux-test-boom')},0)");
        let mut later = Vec::new();
        pump(&panel, &rec, 400, &mut later);
        assert!(
            later.iter().any(|r| r.name == "__error"
                && r.args.first().and_then(|a| a.as_str()).is_some_and(|m| m.contains("sessionmux-test-boom"))),
            "no __error for a thrown error: {later:?}"
        );
        assert!(panel.is_visible());
        panel.hide();
        assert!(!panel.is_visible());
        panel.destroy();
        paths::set_user_data(None);
    }
}

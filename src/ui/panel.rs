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

//! Account panel — frameless popup hosting WebView2 (wry) only while open.
//!
//! API:
//! ```ignore
//! impl Panel {
//!   pub fn create(ipc: Arc<dyn IpcHandler>) -> Result<Panel,String>
//!   pub fn show_near(&self, anchor:RECT, side_pref:&str)
//!   pub fn hide(&self);  pub fn is_visible(&self)->bool;  pub fn hwnd(&self)->HWND
//!   pub fn emit(&self, event:&str, payload:&serde_json::Value)
//!   pub fn resolve(&self, id:u64, ok:bool, payload:&serde_json::Value)
//!   pub fn destroy(self)
//! }
//! ```
//!
//! Window: WS_POPUP 380×580 DIP (scaled to the target monitor's DPI), WS_EX_TOOLWINDOW |
//! WS_EX_TOPMOST, DWM rounded corners, opaque class background #101114. Created hidden (or, via `create_near`, shown
//! inactive at its target place before WebView2 starts).
//!
//! WebView (wry 0.47, WebView2): custom protocol `sessionmux` serving the embedded `web/*` files
//! (`sessionmux://app/panel.html`; on Windows wry maps it to `https://sessionmux.app/panel.html` — https
//! because `.app` is HSTS-preloaded), `web/shim.js` as the initialization script, IPC messages →
//! [`parse_request`] → [`IpcHandler::handle`] (on the UI thread), devtools off, zoom hotkeys and
//! browser accelerator keys off, background #101114, navigation restricted to the embedded
//! origin, new windows denied (http/https ones are forwarded as `openExternal`). WebView2 user
//! data lives in `<user_data>\WebView2`.
//!
//! Behaviour notes for the app:
//! - `create` blocks ~100–500 ms while WebView2 starts and **pumps the thread's message queue**
//!   meanwhile (wry's `wait_with_pump`), so posted UI tasks can run re-entrantly during it.
//! - `emit` calls made before the page reported `__ready` are queued and flushed on `__ready`
//!   (at most 64, oldest dropped). `resolve` is always delivered immediately.
//! - Synthetic requests delivered to the handler with id 0: `__blur` (panel deactivated while
//!   visible), `hidePanel` (WM_CLOSE / Alt+F4), `openExternal` [url] (window.open / target=_blank).
//!   `__error` messages from the page are logged here as warnings and forwarded too.
//! - `show_near(anchor, side)`: a non-empty anchor rect = the bubble → beside it ("left", "right",
//!   anything else = right if it fits, else left), vertically centred and clamped to the work
//!   area; an empty rect whose left/top is not (0,0) = an anchor point (tray icon) and a fully
//!   zero rect = the cursor → above the point (below if no room), horizontally centred.
//! - `destroy` (or dropping the Panel) closes the WebView2 controller first, then destroys the
//!   window, so the WebView2 processes exit.
//!
//! Helpers for smoke: [`Panel::show_inactive_at`], [`capture_window_png`], [`encode_png`].

use crate::brand;
pub use crate::ui::ipc::IpcHandler;
use crate::ui::ipc::{parse_request, IpcRequest};
use crate::util::{log, paths, win};
use raw_window_handle::{
    DisplayHandle, HandleError, HasDisplayHandle, HasWindowHandle, RawWindowHandle, Win32WindowHandle, WindowHandle,
};
use std::borrow::Cow;
use std::cell::{Cell, RefCell};
use std::num::NonZeroIsize;
use std::path::Path;
use std::rc::{Rc, Weak};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{BOOL, COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND};
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, CreateDIBSection, CreateSolidBrush, DeleteDC, DeleteObject, GetDC, GetMonitorInfoW,
    MonitorFromPoint, MonitorFromRect, ReleaseDC, SelectObject, UpdateWindow, BITMAPINFO, BITMAPINFOHEADER, BI_RGB,
    DIB_RGB_COLORS, HBRUSH, HDC, HGDIOBJ, HMONITOR, MONITORINFO, MONITOR_DEFAULTTONEAREST, MONITOR_DEFAULTTOPRIMARY,
};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetCursorPos, GetWindowLongPtrW, GetWindowRect, IsWindowVisible,
    LoadCursorW, RegisterClassExW, SetForegroundWindow, SetWindowLongPtrW, SetWindowPos, ShowWindow, GWLP_USERDATA,
    HWND_TOPMOST, IDC_ARROW, PW_RENDERFULLCONTENT, SWP_NOACTIVATE, SWP_SHOWWINDOW, SW_HIDE, WA_INACTIVE, WM_ACTIVATE,
    WM_CLOSE, WM_DPICHANGED, WM_NCDESTROY, WNDCLASSEXW, WS_CLIPCHILDREN, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};
use wry::http::{header::CONTENT_TYPE, HeaderValue, Request, Response, StatusCode};
use wry::{WebContext, WebView, WebViewBuilder, WebViewBuilderExtWindows};

/// Panel size in DIPs.
pub const PANEL_W: i32 = 380;
pub const PANEL_H: i32 = 580;
/// Gap between the bubble (or tray point) and the panel, DIPs.
const PANEL_GAP: i32 = 10;
/// Custom protocol name serving `web/*`: the scheme of `brand::URL_SCHEME_HOST` ("sessionmux").
fn scheme() -> &'static str {
    brand::URL_SCHEME_HOST.split("://").next().unwrap_or(brand::URL_SCHEME_HOST)
}

/// URL the panel loads, `sessionmux://app/panel.html` (wry rewrites it to
/// https://sessionmux.app/panel.html on Windows).
pub fn panel_url() -> String {
    format!("{}/panel.html", brand::URL_SCHEME_HOST)
}

/// Origin the page actually runs on: `https://<scheme>.<host>/` (wry's Windows mapping).
fn origin_prefix() -> String {
    let host = brand::URL_SCHEME_HOST.split("://").nth(1).unwrap_or("app");
    format!("https://{}.{}/", scheme(), host)
}

const CLASS_NAME: &str = brand::CLASS_PANEL;
/// #101114 as COLORREF (0x00BBGGRR).
const BG_COLORREF: u32 = 0x0014_1110;
const MAX_QUEUED_EMITS: usize = 64;
/// WebView2 browser arguments (replaces wry's default, which is the first switch).
/// No GPU process and no GPU heaps (software compositing is plenty for one 380x580 card), the
/// network service inside the browser process, one renderer.
const BROWSER_ARGS: &str = "--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection --disable-gpu --disable-gpu-compositing --enable-features=NetworkServiceInProcess2 --renderer-process-limit=1 --disable-background-networking";

// ---------------------------------------------------------------- embedded web assets

const SHIM_JS: &str = include_str!("../../web/shim.js");

/// (path, mime, bytes) for every file served under the `sessionmux` protocol.
const ASSETS: &[(&str, &str, &[u8])] = &[
    ("panel.html", "text/html; charset=utf-8", include_bytes!("../../web/panel.html")),
    ("panel.css", "text/css; charset=utf-8", include_bytes!("../../web/panel.css")),
    ("shared.css", "text/css; charset=utf-8", include_bytes!("../../web/shared.css")),
    ("panel.js", "text/javascript; charset=utf-8", include_bytes!("../../web/panel.js")),
    ("shim.js", "text/javascript; charset=utf-8", include_bytes!("../../web/shim.js")),
];

/// Look up an embedded asset by URL path ("/panel.html", "panel.css?x", "" → panel.html).
fn asset_for(path: &str) -> Option<(&'static str, &'static [u8])> {
    let p = path.split(['?', '#']).next().unwrap_or("").trim_start_matches('/');
    let p = if p.is_empty() { "panel.html" } else { p };
    ASSETS.iter().find(|(name, _, _)| *name == p).map(|(_, mime, bytes)| (*mime, *bytes))
}

fn serve(req: &Request<Vec<u8>>) -> Response<Cow<'static, [u8]>> {
    let found = if req.method() == wry::http::Method::GET { asset_for(req.uri().path()) } else { None };
    match found {
        Some((mime, bytes)) => {
            let mut r = Response::new(Cow::Borrowed(bytes));
            r.headers_mut().insert(CONTENT_TYPE, HeaderValue::from_static(mime));
            r.headers_mut().insert("Cache-Control", HeaderValue::from_static("no-store"));
            r
        }
        None => {
            log::warn(&format!("panel: no embedded asset for {} {}", req.method(), req.uri()));
            let mut r = Response::new(Cow::Borrowed(&b"not found"[..]));
            *r.status_mut() = StatusCode::NOT_FOUND;
            r.headers_mut().insert(CONTENT_TYPE, HeaderValue::from_static("text/plain; charset=utf-8"));
            r
        }
    }
}

// ---------------------------------------------------------------- JS call encoding

/// JSON text that is also a safe JavaScript expression (U+2028/2029 escaped).
fn js_value(v: &serde_json::Value) -> String {
    let s = serde_json::to_string(v).unwrap_or_else(|_| "null".into());
    s.replace('\u{2028}', "\\u2028").replace('\u{2029}', "\\u2029")
}

fn emit_script(event: &str, payload: &serde_json::Value) -> String {
    format!(
        "window.__sessionmuxEmit&&window.__sessionmuxEmit({},{})",
        js_value(&serde_json::Value::String(event.to_string())),
        js_value(payload)
    )
}

fn resolve_script(id: u64, ok: bool, payload: &serde_json::Value) -> String {
    format!("window.__sessionmuxResolve&&window.__sessionmuxResolve({id},{ok},{})", js_value(payload))
}

// ---------------------------------------------------------------- geometry (pure)

fn clamp(v: i32, min: i32, max: i32) -> i32 {
    if max < min {
        min
    } else {
        v.clamp(min, max)
    }
}

fn clamp_into(x: i32, y: i32, w: i32, h: i32, area: RECT) -> RECT {
    let x = clamp(x, area.left, area.right - w);
    let y = clamp(y, area.top, area.bottom - h);
    RECT { left: x, top: y, right: x + w, bottom: y + h }
}

/// Panel rect beside a bubble rect.
fn rect_beside(bubble: RECT, area: RECT, side: &str, w: i32, h: i32, gap: i32) -> RECT {
    let right_x = bubble.right + gap;
    let left_x = bubble.left - gap - w;
    let fits_right = right_x + w <= area.right;
    let fits_left = left_x >= area.left;
    let x = match side {
        "left" => left_x,
        "right" => right_x,
        _ if fits_right => right_x,
        _ if fits_left => left_x,
        _ => right_x,
    };
    let y = bubble.top + (bubble.bottom - bubble.top) / 2 - h / 2;
    clamp_into(x, y, w, h, area)
}

/// Panel rect above (or below, if no room) a point such as the tray icon.
fn rect_near_point(pt: POINT, area: RECT, w: i32, h: i32, gap: i32) -> RECT {
    let mut y = pt.y - h - gap;
    if y < area.top {
        y = pt.y + gap;
    }
    clamp_into(pt.x - w / 2, y, w, h, area)
}

fn monitor_area_and_scale(mon: HMONITOR) -> (RECT, f32) {
    let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
    // SAFETY: `mi` is a properly sized out-struct; an invalid monitor handle only makes it fail.
    let area = if unsafe { GetMonitorInfoW(mon, &mut mi) }.as_bool() {
        mi.rcWork
    } else {
        RECT { left: 0, top: 0, right: 1920, bottom: 1080 }
    };
    let (mut dx, mut dy) = (0u32, 0u32);
    // SAFETY: valid out-pointers; failure leaves 0 and we fall back to the system DPI.
    let scale = match unsafe { GetDpiForMonitor(mon, MDT_EFFECTIVE_DPI, &mut dx, &mut dy) } {
        Ok(()) if dx > 0 => dx as f32 / 96.0,
        _ => win::dpi_scale(HWND::default()),
    };
    (area, scale)
}

fn scaled(v: i32, scale: f32) -> i32 {
    (v as f32 * scale).round() as i32
}

// ---------------------------------------------------------------- raw-window-handle wrapper

/// Borrowed HWND for wry. Valid while the owning Panel's window exists.
struct HwndHandle(HWND);

impl HasWindowHandle for HwndHandle {
    fn window_handle(&self) -> Result<WindowHandle<'_>, HandleError> {
        let hwnd = NonZeroIsize::new(self.0 .0 as isize).ok_or(HandleError::Unavailable)?;
        let mut h = Win32WindowHandle::new(hwnd);
        h.hinstance = NonZeroIsize::new(win::hinstance().0 as isize);
        // SAFETY: the HWND is a live window owned by the Panel being built; wry only uses the
        // handle during `build`, while the window certainly exists.
        Ok(unsafe { WindowHandle::borrow_raw(RawWindowHandle::Win32(h)) })
    }
}

impl HasDisplayHandle for HwndHandle {
    fn display_handle(&self) -> Result<DisplayHandle<'_>, HandleError> {
        Ok(DisplayHandle::windows())
    }
}

// ---------------------------------------------------------------- window class

struct WndData {
    ipc: Arc<dyn IpcHandler>,
}

fn synthetic(ipc: &Arc<dyn IpcHandler>, name: &str, args: Vec<serde_json::Value>) {
    ipc.handle(IpcRequest { id: 0, name: name.to_string(), args });
}

unsafe extern "system" fn panel_wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    // SAFETY: GWLP_USERDATA is 0 or a `Box<WndData>` pointer installed by Panel::create and freed
    // only in WM_NCDESTROY below, so dereferencing it here is valid.
    let data = unsafe { (GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const WndData).as_ref() };
    match msg {
        WM_ACTIVATE => {
            // SAFETY: IsWindowVisible tolerates any handle.
            if (wparam.0 & 0xFFFF) as u32 == WA_INACTIVE && unsafe { IsWindowVisible(hwnd) }.as_bool() {
                if let Some(d) = data {
                    synthetic(&d.ipc, "__blur", Vec::new());
                }
            }
        }
        WM_CLOSE => {
            // Alt+F4: let the app hide (and later destroy) the panel through its normal path.
            if let Some(d) = data {
                synthetic(&d.ipc, "hidePanel", Vec::new());
            }
            return LRESULT(0);
        }
        WM_DPICHANGED => {
            // show_near sizes the window for the target monitor itself; ignore the suggestion.
            return LRESULT(0);
        }
        WM_NCDESTROY => {
            // SAFETY: reclaim the Box installed by Panel::create exactly once, then clear the slot.
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
fn near_rect(anchor: RECT, side_pref: &str) -> RECT {
    let has_rect = anchor.right > anchor.left && anchor.bottom > anchor.top;
    if has_rect {
        // SAFETY: MonitorFromRect reads the RECT; DEFAULTTONEAREST always yields a monitor.
        let mon = unsafe { MonitorFromRect(&anchor, MONITOR_DEFAULTTONEAREST) };
        let (area, s) = monitor_area_and_scale(mon);
        rect_beside(anchor, area, side_pref, scaled(PANEL_W, s), scaled(PANEL_H, s), scaled(PANEL_GAP, s))
    } else {
        let mut pt = POINT { x: anchor.left, y: anchor.top };
        if pt.x == 0 && pt.y == 0 {
            // SAFETY: valid out-pointer; on failure pt stays (0,0).
            let _ = unsafe { GetCursorPos(&mut pt) };
        }
        // SAFETY: MonitorFromPoint takes the point by value.
        let mon = unsafe { MonitorFromPoint(pt, MONITOR_DEFAULTTONEAREST) };
        let (area, s) = monitor_area_and_scale(mon);
        rect_near_point(pt, area, scaled(PANEL_W, s), scaled(PANEL_H, s), scaled(PANEL_GAP, s))
    }
}

/// wry 0.47 force-enables WebView2's default context menu (Back/Reload/Save as/Print); turn it
/// off so a right-click outside the account rows does nothing.
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

// ---------------------------------------------------------------- Panel

/// State shared with the IPC closure (UI thread only).
struct Shared {
    ready: Cell<bool>,
    queue: RefCell<Vec<String>>,
    webview: RefCell<Weak<WebView>>,
}

impl Shared {
    fn on_ready(&self) {
        self.ready.set(true);
        let queued = std::mem::take(&mut *self.queue.borrow_mut());
        let wv = self.webview.borrow().upgrade();
        if let Some(wv) = wv {
            for js in queued {
                if let Err(e) = wv.evaluate_script(&js) {
                    log::warn(&format!("panel: queued emit failed: {e}"));
                }
            }
        }
    }
}

pub struct Panel {
    hwnd: HWND,
    ipc: Arc<dyn IpcHandler>,
    webview: Option<Rc<WebView>>,
    shared: Rc<Shared>,
    _context: WebContext,
}

impl Panel {
    pub fn create(ipc: Arc<dyn IpcHandler>) -> Result<Panel, String> {
        Self::create_near(ipc, None)
    }

    /// Like [`Panel::create`], but when `near` is given the native window (opaque #101114, rounded)
    /// is shown there without activation right away, before WebView2 starts, so a cold reopen
    /// paints something within a frame instead of nothing for ~0.5-1 s. The caller still calls
    /// `show_near` afterwards to activate it.
    pub fn create_near(ipc: Arc<dyn IpcHandler>, near: Option<(RECT, &str)>) -> Result<Panel, String> {
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

        let data = Box::into_raw(Box::new(WndData { ipc: ipc.clone() }));
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
                let _ = SetWindowPos(
                    hwnd,
                    HWND_TOPMOST,
                    r.left,
                    r.top,
                    r.right - r.left,
                    r.bottom - r.top,
                    SWP_SHOWWINDOW | SWP_NOACTIVATE,
                );
                let _ = UpdateWindow(hwnd);
            }
        }

        match Self::build_webview(hwnd, ipc.clone()) {
            Ok((webview, shared, context)) => {
                Ok(Panel { hwnd, ipc, webview: Some(webview), shared, _context: context })
            }
            Err(e) => {
                // SAFETY: destroying the window we just created on this thread.
                let _ = unsafe { DestroyWindow(hwnd) };
                Err(e)
            }
        }
    }

    fn build_webview(hwnd: HWND, ipc: Arc<dyn IpcHandler>) -> Result<(Rc<WebView>, Rc<Shared>, WebContext), String> {
        let shared = Rc::new(Shared {
            ready: Cell::new(false),
            queue: RefCell::new(Vec::new()),
            webview: RefCell::new(Weak::new()),
        });
        let mut context = WebContext::new(Some(paths::user_data().join(brand::WEBVIEW_DIR)));

        let sh = shared.clone();
        let ipc_msg = ipc.clone();
        let ipc_win = ipc.clone();
        let parent = HwndHandle(hwnd);
        let origin = origin_prefix();
        let webview = WebViewBuilder::with_web_context(&mut context)
            .with_url(panel_url())
            .with_initialization_script(SHIM_JS)
            .with_custom_protocol(scheme().into(), |_id, req| serve(&req))
            .with_ipc_handler(move |req: Request<String>| {
                let Some(r) = parse_request(req.body()) else {
                    log::warn(&format!("panel: rejected ipc message ({} bytes)", req.body().len()));
                    return;
                };
                match r.name.as_str() {
                    "__ready" => sh.on_ready(),
                    "__error" => {
                        let parts: Vec<String> = r
                            .args
                            .iter()
                            .map(|a| a.as_str().map(str::to_string).unwrap_or_else(|| a.to_string()))
                            .filter(|s| !s.is_empty() && s != "0")
                            .collect();
                        log::warn(&format!("panel js: {}", parts.join(" | ")));
                    }
                    _ => {}
                }
                ipc_msg.handle(r);
            })
            .with_navigation_handler(move |url| {
                let ok = url.starts_with(&origin);
                if !ok {
                    log::warn(&format!("panel: blocked navigation to {url}"));
                }
                ok
            })
            .with_new_window_req_handler(move |url| {
                let lower = url.to_ascii_lowercase();
                if lower.starts_with("https://") || lower.starts_with("http://") {
                    synthetic(&ipc_win, "openExternal", vec![serde_json::Value::String(url)]);
                }
                false
            })
            .with_devtools(false)
            .with_transparent(false)
            .with_background_color((0x10, 0x11, 0x14, 0xFF))
            .with_autoplay(false)
            .with_hotkeys_zoom(false)
            // No clipboard-read auto-grant (wry's clipboard flag only grants reads); the page
            // only writes (navigator.clipboard.writeText), which needs no permission.
            .with_clipboard(false)
            .with_browser_accelerator_keys(false)
            .with_https_scheme(true)
            // Footprint: wry's default feature switches, plus the GPU work in the
            // browser process and a single renderer — the panel is one small static page.
            .with_additional_browser_args(BROWSER_ARGS)
            .build(&parent)
            .map_err(|e| format!("WebView2: {e}"))?;
        disable_default_context_menu(&webview);
        let webview = Rc::new(webview);
        *shared.webview.borrow_mut() = Rc::downgrade(&webview);
        Ok((webview, shared, context))
    }

    /// Position (DPI-aware), show, activate and focus the panel.
    pub fn show_near(&self, anchor: RECT, side_pref: &str) {
        self.place(near_rect(anchor, side_pref), true);
    }

    /// Show without activating at (x, y) on the primary monitor's scale — for smoke captures,
    /// typically off-screen (e.g. x = -10000).
    pub fn show_inactive_at(&self, x: i32, y: i32) {
        // SAFETY: MonitorFromPoint takes the point by value.
        let mon = unsafe { MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY) };
        let (_, s) = monitor_area_and_scale(mon);
        let (w, h) = (scaled(PANEL_W, s), scaled(PANEL_H, s));
        self.place(RECT { left: x, top: y, right: x + w, bottom: y + h }, false);
    }

    fn place(&self, r: RECT, activate: bool) {
        let mut flags = SWP_SHOWWINDOW;
        if !activate {
            flags |= SWP_NOACTIVATE;
        }
        // SAFETY: repositioning our own window; integers only.
        if let Err(e) =
            unsafe { SetWindowPos(self.hwnd, HWND_TOPMOST, r.left, r.top, r.right - r.left, r.bottom - r.top, flags) }
        {
            log::warn(&format!("panel: SetWindowPos failed: {e}"));
        }
        if let Some(wv) = &self.webview {
            let _ = wv.set_visible(true);
            if activate {
                // SAFETY: plain call on our own window; failure (foreground lock) is harmless.
                let _ = unsafe { SetForegroundWindow(self.hwnd) };
                let _ = wv.focus();
            }
        }
    }

    pub fn hide(&self) {
        if let Some(wv) = &self.webview {
            // Marks the page hidden (panel.js closes menus/dialogs) and lets WebView2 throttle.
            let _ = wv.set_visible(false);
        }
        // SAFETY: hiding our own window; the return value is the previous visibility.
        let _ = unsafe { ShowWindow(self.hwnd, SW_HIDE) };
    }

    pub fn is_visible(&self) -> bool {
        // SAFETY: IsWindowVisible tolerates any handle.
        unsafe { IsWindowVisible(self.hwnd) }.as_bool()
    }

    pub fn hwnd(&self) -> HWND {
        self.hwnd
    }

    /// Whether the page has sent `__ready` (DOM loaded, panel.js subscribed).
    pub fn is_ready(&self) -> bool {
        self.shared.ready.get()
    }

    pub fn emit(&self, event: &str, payload: &serde_json::Value) {
        let js = emit_script(event, payload);
        if !self.shared.ready.get() {
            let mut q = self.shared.queue.borrow_mut();
            if q.len() >= MAX_QUEUED_EMITS {
                q.remove(0);
            }
            q.push(js);
            return;
        }
        self.eval(&js);
    }

    pub fn resolve(&self, id: u64, ok: bool, payload: &serde_json::Value) {
        if id == 0 {
            return; // notifications expect no reply
        }
        self.eval(&resolve_script(id, ok, payload));
    }

    fn eval(&self, js: &str) {
        if let Some(wv) = &self.webview {
            if let Err(e) = wv.evaluate_script(js) {
                log::warn(&format!("panel: evaluate_script failed: {e}"));
            }
        }
    }

    pub fn destroy(self) {
        drop(self);
    }
}

impl Drop for Panel {
    fn drop(&mut self) {
        let _ = &self.ipc;
        // Close the WebView2 controller while the parent window still exists (wry detaches its
        // parent subclass with a SendMessage), then destroy the window.
        self.webview.take();
        // SAFETY: our own window, destroyed once on its owning thread (Panel is !Send).
        let _ = unsafe { DestroyWindow(self.hwnd) };
    }
}

// ---------------------------------------------------------------- capture (smoke helpers)

#[link(name = "user32")]
extern "system" {
    // Declared here because windows-rs puts PrintWindow behind the Win32_Storage_Xps feature.
    fn PrintWindow(hwnd: HWND, hdc: HDC, flags: u32) -> BOOL;
}

/// Capture `hwnd` with `PrintWindow(PW_RENDERFULLCONTENT)` and write it as a PNG.
/// Returns the image size. The window must be shown (it may be off-screen).
pub fn capture_window_png(hwnd: HWND, path: &Path) -> Result<(u32, u32), String> {
    let mut wr = RECT::default();
    // SAFETY: valid out-pointer.
    unsafe { GetWindowRect(hwnd, &mut wr) }.map_err(|e| format!("GetWindowRect: {e}"))?;
    let (w, h) = (wr.right - wr.left, wr.bottom - wr.top);
    if w <= 0 || h <= 0 {
        return Err("window has no size".into());
    }
    let bmi = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: w,
            biHeight: -h, // top-down
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut pixels: Vec<u8>;
    // SAFETY: standard GDI capture sequence; every handle created here is released below, and
    // `bits` points to w*h*4 bytes owned by the DIB section until DeleteObject.
    unsafe {
        let screen = GetDC(None);
        let mem = CreateCompatibleDC(screen);
        let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
        let dib = CreateDIBSection(mem, &bmi, DIB_RGB_COLORS, &mut bits, None, 0);
        let dib = match dib {
            Ok(d) if !bits.is_null() => d,
            other => {
                if let Ok(d) = other {
                    let _ = DeleteObject(d);
                }
                let _ = DeleteDC(mem);
                ReleaseDC(None, screen);
                return Err(format!("CreateDIBSection: {}", win::last_error()));
            }
        };
        let old = SelectObject(mem, HGDIOBJ(dib.0));
        let ok = PrintWindow(hwnd, mem, PW_RENDERFULLCONTENT).as_bool();
        pixels = std::slice::from_raw_parts(bits as *const u8, (w * h * 4) as usize).to_vec();
        SelectObject(mem, old);
        let _ = DeleteObject(dib);
        let _ = DeleteDC(mem);
        ReleaseDC(None, screen);
        if !ok {
            return Err(format!("PrintWindow: {}", win::last_error()));
        }
    }
    for px in pixels.as_chunks_mut::<4>().0 {
        px[3] = 0xFF; // PrintWindow leaves alpha undefined for opaque windows
    }
    let png = encode_png(w as u32, h as u32, &pixels);
    std::fs::write(path, png).map_err(|e| format!("write {}: {e}", path.display()))?;
    Ok((w as u32, h as u32))
}

fn crc32(data: &[u8]) -> u32 {
    let mut table = [0u32; 256];
    for (i, t) in table.iter_mut().enumerate() {
        let mut c = i as u32;
        for _ in 0..8 {
            c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
        }
        *t = c;
    }
    let mut crc = 0xFFFF_FFFFu32;
    for &b in data {
        crc = table[((crc ^ b as u32) & 0xFF) as usize] ^ (crc >> 8);
    }
    crc ^ 0xFFFF_FFFF
}

/// Minimal PNG encoder (RGBA8, zlib stored blocks) for top-down BGRA pixels.
pub fn encode_png(width: u32, height: u32, bgra: &[u8]) -> Vec<u8> {
    let mut raw = Vec::with_capacity((width as usize * 4 + 1) * height as usize);
    for row in bgra.chunks_exact(width as usize * 4).take(height as usize) {
        raw.push(0); // filter: none
        for px in row.as_chunks::<4>().0 {
            raw.extend_from_slice(&[px[2], px[1], px[0], px[3]]);
        }
    }
    let mut z = vec![0x78, 0x01];
    let chunks: Vec<&[u8]> = if raw.is_empty() { vec![&[][..]] } else { raw.chunks(65_535).collect() };
    let n = chunks.len();
    for (i, c) in chunks.into_iter().enumerate() {
        z.push(if i + 1 == n { 1 } else { 0 });
        let len = c.len() as u16;
        z.extend_from_slice(&len.to_le_bytes());
        z.extend_from_slice(&(!len).to_le_bytes());
        z.extend_from_slice(c);
    }
    let (mut a, mut b) = (1u32, 0u32);
    for &byte in &raw {
        a = (a + byte as u32) % 65_521;
        b = (b + a) % 65_521;
    }
    z.extend_from_slice(&((b << 16) | a).to_be_bytes());

    let mut out = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    let mut chunk = |kind: &[u8; 4], data: &[u8]| {
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        let mut c = Vec::with_capacity(4 + data.len());
        c.extend_from_slice(kind);
        c.extend_from_slice(data);
        out.extend_from_slice(&c);
        out.extend_from_slice(&crc32(&c).to_be_bytes());
    };
    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
    chunk(b"IHDR", &ihdr);
    chunk(b"IDAT", &z);
    chunk(b"IEND", &[]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn r(l: i32, t: i32, rr: i32, b: i32) -> RECT {
        RECT { left: l, top: t, right: rr, bottom: b }
    }

    #[test]
    fn assets_and_mime() {
        assert_eq!(asset_for("/panel.html").map(|a| a.0), Some("text/html; charset=utf-8"));
        assert_eq!(asset_for("/").map(|a| a.0), Some("text/html; charset=utf-8"));
        assert_eq!(asset_for("/panel.css?v=1").map(|a| a.0), Some("text/css; charset=utf-8"));
        assert_eq!(asset_for("/shared.css").map(|a| a.0), Some("text/css; charset=utf-8"));
        assert_eq!(asset_for("/panel.js").map(|a| a.0), Some("text/javascript; charset=utf-8"));
        assert!(asset_for("/../Cargo.toml").is_none());
        assert!(asset_for("/missing.png").is_none());
        let req = Request::builder().uri("https://sessionmux.app/panel.html").body(Vec::new()).unwrap();
        let resp = serve(&req);
        assert_eq!(resp.status(), StatusCode::OK);
        assert!(std::str::from_utf8(resp.body()).unwrap().contains("<script src=\"panel.js\">"));
        let req = Request::builder().uri("https://sessionmux.app/nope").body(Vec::new()).unwrap();
        assert_eq!(serve(&req).status(), StatusCode::NOT_FOUND);
        let req = Request::builder().method("POST").uri("https://sessionmux.app/panel.html").body(Vec::new()).unwrap();
        assert_eq!(serve(&req).status(), StatusCode::NOT_FOUND);
        // The shim exposes exactly the documented method names.
        for name in [
            "listAccounts",
            "openAccount",
            "openAll",
            "closeAccount",
            "addAccount",
            "renameAccount",
            "removeAccount",
            "reorderAccounts",
            "listSessions",
            "transferSession",
            "deleteSession",
            "openSessionFolder",
            "getSettings",
            "setSettings",
            "listDiscovered",
            "adoptProfile",
            "ignoreProfile",
            "togglePanel",
            "hidePanel",
            "pinPanel",
            "quit",
            "openExternal",
            "getAppInfo",
            "openLicense",
            "getSignIn",
            "cancelSignIn",
            "onAccountsChanged",
            "onActiveAccount",
            "onSettings",
            "onShowTab",
            "onDiscoveredChanged",
            "onSignInChanged",
        ] {
            assert!(SHIM_JS.contains(&format!("    {name}: ")), "shim lacks {name}");
        }
    }

    #[test]
    fn brand_urls() {
        assert_eq!(scheme(), "sessionmux");
        assert_eq!(panel_url(), "sessionmux://app/panel.html");
        assert_eq!(origin_prefix(), "https://sessionmux.app/");
    }

    #[test]
    fn scripts_are_escaped() {
        let js = emit_script("show-tab", &json!("accounts"));
        assert_eq!(js, r#"window.__sessionmuxEmit&&window.__sessionmuxEmit("show-tab","accounts")"#);
        let js = emit_script("x\"y", &json!({"n": "a\u{2028}b</script>\\"}));
        assert!(js.contains(r#""x\"y""#));
        assert!(js.contains(r"a\u2028b</script>\\"));
        assert!(!js.contains('\u{2028}'));
        assert_eq!(
            resolve_script(5, false, &json!({"ok": false})),
            r#"window.__sessionmuxResolve&&window.__sessionmuxResolve(5,false,{"ok":false})"#
        );
    }

    #[test]
    fn geometry_beside_bubble() {
        let area = r(0, 0, 1920, 1040);
        // Bubble at the right edge → panel goes left of it, vertically centred.
        let b = r(1828, 900, 1908, 980);
        let p = rect_beside(b, area, "auto", 380, 580, 10);
        assert_eq!((p.left, p.right), (1828 - 10 - 380, 1828 - 10));
        assert_eq!(p.bottom, 1040, "clamped to the work area bottom");
        // Bubble at the left edge → right of it.
        let b = r(12, 400, 92, 480);
        let p = rect_beside(b, area, "auto", 380, 580, 10);
        assert_eq!((p.left, p.top), (102, 440 - 290));
        // Forced side, clamped back on screen.
        let p = rect_beside(b, area, "left", 380, 580, 10);
        assert_eq!(p.left, 0);
        // Tiny screen: neither fits → right, clamped.
        let p = rect_beside(r(100, 0, 180, 80), r(0, 0, 400, 600), "auto", 380, 580, 10);
        assert_eq!((p.left, p.top), (20, 0));
    }

    #[test]
    fn geometry_near_point() {
        let area = r(0, 0, 1920, 1040);
        let p = rect_near_point(POINT { x: 1800, y: 1060 }, area, 380, 580, 10);
        assert_eq!((p.left, p.top, p.right, p.bottom), (1540, 460, 1920, 1040));
        // Taskbar on top: no room above → below the point.
        let p = rect_near_point(POINT { x: 500, y: 20 }, area, 380, 580, 10);
        assert_eq!((p.left, p.top), (310, 30));
    }

    #[test]
    fn png_encoder() {
        let px = [0u8, 0, 255, 255, 255, 0, 0, 128]; // red, blue@50%
        let png = encode_png(2, 1, &px);
        assert_eq!(&png[..8], &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]);
        assert_eq!(&png[12..16], b"IHDR");
        assert_eq!(&png[png.len() - 8..png.len() - 4], b"IEND");
        assert_eq!(crc32(b"IEND"), 0xAE42_6082);
        // Multi-block payload (> 65535 bytes) still yields one final block marker.
        let big = vec![7u8; 200 * 100 * 4];
        let png = encode_png(200, 100, &big);
        assert!(png.len() > 80_000);
    }

    // ------------------------------------------------------------ live render check (manual)

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
        let _ = capture_window_png(panel.hwnd(), &out.join("sessionmux-panel-warmup.png"));
        for (i, tab) in ["accounts", "sessions", "settings"].into_iter().enumerate() {
            panel.emit("show-tab", &json!(tab));
            pump(&panel, &rec, 700, &mut seen);
            let path = out.join(format!("sessionmux-panel-{i}-{tab}.png"));
            let size = capture_window_png(panel.hwnd(), &path).expect("capture");
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

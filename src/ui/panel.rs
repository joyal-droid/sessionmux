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

//! Account panel — the web page in a frameless window, hosted only while open.
//!
//! API:
//! ```ignore
//! impl Panel {
//!   pub fn create(ipc: Arc<dyn IpcHandler>) -> Result<Panel,String>
//!   pub fn show_near(&self, anchor:Rect, side_pref:&str)
//!   pub fn hide(&self);  pub fn is_visible(&self)->bool;  pub fn window(&self)->WindowId
//!   pub fn emit(&self, event:&str, payload:&serde_json::Value)
//!   pub fn resolve(&self, id:u64, ok:bool, payload:&serde_json::Value)
//!   pub fn destroy(self)
//! }
//! ```
//!
//! Window: `platform::panel`, [`PANEL_W`]×[`PANEL_H`] DIP, opaque #101114 with rounded corners.
//! Created hidden (or, via `create_near`, shown inactive at its target place before the web view
//! starts).
//!
//! WebView (wry 0.57): custom protocol `sessionmux` serving the embedded `web/*` files
//! (`sessionmux://app/panel.html`; on Windows wry maps it to `https://sessionmux.app/panel.html` — https
//! because `.app` is HSTS-preloaded), `web/shim.js` as the initialization script, IPC messages →
//! [`parse_request`] → [`IpcHandler::handle`] (on the UI thread), devtools off, zoom hotkeys off,
//! background #101114, navigation restricted to the embedded origin ([`origin_prefix`]), new
//! windows denied (http/https ones are forwarded as `openExternal`). The window host adds what
//! only its system has (on Windows: browser accelerator keys off, the https mapping, the browser
//! arguments, no default context menu). Web data lives in `<user_data>\WebView2` on Windows; on
//! macOS the page keeps nothing (a non-persistent data store) and the fraudulent-website warning,
//! which would look the page's address up, is off.
//!
//! Behaviour notes for the app:
//! - `create` blocks ~100–500 ms while the web view starts and **pumps the thread's message queue**
//!   meanwhile (wry's `wait_with_pump` on Windows), so posted UI tasks can run re-entrantly during it.
//! - `emit` calls made before the page reported `__ready` are queued and flushed on `__ready`
//!   (at most 64, oldest dropped). `resolve` is always delivered immediately.
//! - Synthetic requests delivered to the handler with id 0: `__blur` (panel deactivated while
//!   visible), `hidePanel` (close request, e.g. Alt+F4), `openExternal` [url] (window.open /
//!   target=_blank). `__error` messages from the page are logged here as warnings and forwarded too.
//! - `show_near(anchor, side)`: a non-empty anchor rect = the bubble → beside it ("left", "right",
//!   anything else = right if it fits, else left), vertically centred and clamped to the work
//!   area; an empty rect whose x/y is not (0,0) = an anchor point (tray icon) and a fully
//!   zero rect = the cursor → above the point (below if no room), horizontally centred.
//! - `destroy` (or dropping the Panel) drops the web view first, then destroys the window, so the
//!   web engine's processes exit.
//!
//! Helpers for smoke: [`Panel::show_inactive_at`], [`encode_png`].

use crate::brand;
use crate::platform::panel::PanelWindow;
use crate::platform::{PanelHost, Rect, WindowId};
pub use crate::ui::ipc::IpcHandler;
use crate::ui::ipc::{parse_request, IpcRequest};
use crate::util::{log, paths};
use std::borrow::Cow;
use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};
use std::sync::Arc;
use wry::http::{header::CONTENT_TYPE, HeaderValue, Request, Response, StatusCode};
use wry::{NewWindowResponse, WebContext, WebView, WebViewBuilder};

/// Panel size in DIPs.
pub const PANEL_W: i32 = 380;
pub const PANEL_H: i32 = 580;
/// Gap between the bubble (or tray point) and the panel, DIPs.
pub const PANEL_GAP: i32 = 10;
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
#[cfg(windows)]
pub fn origin_prefix() -> String {
    let host = brand::URL_SCHEME_HOST.split("://").nth(1).unwrap_or("app");
    format!("https://{}.{}/", scheme(), host)
}

/// Origin the page actually runs on: the custom protocol itself, `<scheme>://<host>/`.
#[cfg(target_os = "macos")]
pub fn origin_prefix() -> String {
    format!("{}/", brand::URL_SCHEME_HOST)
}

/// Page background #101114.
pub const BACKGROUND: (u8, u8, u8, u8) = (0x10, 0x11, 0x14, 0xFF);
const MAX_QUEUED_EMITS: usize = 64;

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

fn clamp_into(x: i32, y: i32, w: i32, h: i32, area: Rect) -> Rect {
    let x = clamp(x, area.x, area.x + area.w - w);
    let y = clamp(y, area.y, area.y + area.h - h);
    Rect { x, y, w, h }
}

/// Panel rect beside a bubble rect.
pub fn rect_beside(bubble: Rect, area: Rect, side: &str, w: i32, h: i32, gap: i32) -> Rect {
    let right_x = bubble.x + bubble.w + gap;
    let left_x = bubble.x - gap - w;
    let fits_right = right_x + w <= area.x + area.w;
    let fits_left = left_x >= area.x;
    let x = match side {
        "left" => left_x,
        "right" => right_x,
        _ if fits_right => right_x,
        _ if fits_left => left_x,
        _ => right_x,
    };
    let y = bubble.y + bubble.h / 2 - h / 2;
    clamp_into(x, y, w, h, area)
}

/// Panel rect above (or below, if no room) a point such as the tray icon.
pub fn rect_near_point(pt: (i32, i32), area: Rect, w: i32, h: i32, gap: i32) -> Rect {
    let (px, py) = pt;
    let mut y = py - h - gap;
    if y < area.y {
        y = py + gap;
    }
    clamp_into(px - w / 2, y, w, h, area)
}

/// `v` DIPs in units of `scale` per DIP, rounded.
pub fn scaled(v: i32, scale: f32) -> i32 {
    (v as f32 * scale).round() as i32
}

// ---------------------------------------------------------------- Panel

fn synthetic(ipc: &Arc<dyn IpcHandler>, name: &str, args: Vec<serde_json::Value>) {
    ipc.handle(IpcRequest { id: 0, name: name.to_string(), args });
}

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
    /// None only while dropping.
    host: Option<PanelWindow>,
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
    /// is shown there without activation right away, before the web view starts, so a cold
    /// reopen paints something within a frame instead of nothing for ~0.5-1 s. The caller still
    /// calls `show_near` afterwards to activate it.
    pub fn create_near(ipc: Arc<dyn IpcHandler>, near: Option<(Rect, &str)>) -> Result<Panel, String> {
        let host = PanelWindow::create(ipc.clone(), near)?;
        match Self::build_webview(&host, ipc.clone()) {
            Ok((webview, shared, context)) => {
                Ok(Panel { host: Some(host), ipc, webview: Some(webview), shared, _context: context })
            }
            Err(e) => {
                host.destroy();
                Err(e)
            }
        }
    }

    fn build_webview(
        host: &PanelWindow,
        ipc: Arc<dyn IpcHandler>,
    ) -> Result<(Rc<WebView>, Rc<Shared>, WebContext), String> {
        let shared = Rc::new(Shared {
            ready: Cell::new(false),
            queue: RefCell::new(Vec::new()),
            webview: RefCell::new(Weak::new()),
        });
        let mut context = WebContext::new(Some(paths::user_data().join(brand::WEBVIEW_DIR)));

        let sh = shared.clone();
        let ipc_msg = ipc.clone();
        let ipc_win = ipc.clone();
        let origin = origin_prefix();
        let builder = WebViewBuilder::new_with_web_context(&mut context)
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
            .with_new_window_req_handler(move |url, _features| {
                let lower = url.to_ascii_lowercase();
                if lower.starts_with("https://") || lower.starts_with("http://") {
                    synthetic(&ipc_win, "openExternal", vec![serde_json::Value::String(url)]);
                }
                NewWindowResponse::Deny
            })
            .with_devtools(false)
            .with_transparent(false)
            .with_background_color(BACKGROUND)
            .with_autoplay(false)
            .with_hotkeys_zoom(false)
            // No clipboard-read auto-grant (wry's clipboard flag only grants reads); the page
            // only writes (navigator.clipboard.writeText), which needs no permission.
            .with_clipboard(false);
        // Nothing the page does is kept between openings.
        #[cfg(target_os = "macos")]
        let builder = builder.with_incognito(true);
        let webview = PanelWindow::configure(builder)
            .build(host)
            .map_err(|e| format!("{}: {e}", <PanelWindow as PanelHost>::ENGINE))?;
        host.after_build(&webview);
        #[cfg(target_os = "macos")]
        no_fraud_lookups(&webview);
        let webview = Rc::new(webview);
        *shared.webview.borrow_mut() = Rc::downgrade(&webview);
        Ok((webview, shared, context))
    }

    /// Position (DPI-aware), show, activate and focus the panel.
    pub fn show_near(&self, anchor: Rect, side_pref: &str) {
        if let Some(h) = &self.host {
            h.show_near(anchor, side_pref);
        }
        self.reveal(true);
    }

    /// Show without activating at (x, y) on the primary display's scale — for smoke captures,
    /// typically off-screen (e.g. x = -10000).
    pub fn show_inactive_at(&self, x: i32, y: i32) {
        if let Some(h) = &self.host {
            h.show_inactive_at(x, y);
        }
        self.reveal(false);
    }

    /// The page is visible again; with `activate` the window and the page take the focus.
    fn reveal(&self, activate: bool) {
        if let Some(wv) = &self.webview {
            let _ = wv.set_visible(true);
            if activate {
                if let Some(h) = &self.host {
                    h.activate();
                }
                let _ = wv.focus();
            }
        }
    }

    pub fn hide(&self) {
        if let Some(wv) = &self.webview {
            // Marks the page hidden (panel.js closes menus/dialogs) and lets the engine throttle.
            let _ = wv.set_visible(false);
        }
        if let Some(h) = &self.host {
            h.hide();
        }
    }

    pub fn is_visible(&self) -> bool {
        self.host.as_ref().is_some_and(|h| h.is_visible())
    }

    pub fn window(&self) -> WindowId {
        self.host.as_ref().map(|h| h.window()).unwrap_or(WindowId(0))
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

    pub(crate) fn eval(&self, js: &str) {
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
        // Drop the web view while its window still exists (on Windows wry detaches its parent
        // subclass with a SendMessage), then destroy the window.
        self.webview.take();
        if let Some(h) = self.host.take() {
            h.destroy();
        }
    }
}

/// Turn off the fraudulent-website warning, which would send the page's address to a lookup
/// service; the panel only ever shows its own embedded page.
#[cfg(target_os = "macos")]
fn no_fraud_lookups(webview: &WebView) {
    use wry::WebViewExtMacOS;
    let wk = webview.webview();
    // SAFETY: `configuration` and `preferences` return the live configuration objects of the web
    // view just built, on the main thread that owns it.
    let still_on = unsafe {
        wk.configuration().preferences().setFraudulentWebsiteWarningEnabled(false);
        wk.configuration().preferences().isFraudulentWebsiteWarningEnabled()
    };
    if still_on {
        log::warn("panel: the fraudulent-website warning could not be turned off");
    }
}

// ---------------------------------------------------------------- PNG (smoke captures)

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

    fn r(l: i32, t: i32, rr: i32, b: i32) -> Rect {
        Rect { x: l, y: t, w: rr - l, h: b - t }
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
            "restoreLinkHandler",
            "removeChanges",
            "openSystemSettings",
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
    }

    #[cfg(windows)]
    #[test]
    fn origin_is_the_https_mapping() {
        assert_eq!(origin_prefix(), "https://sessionmux.app/");
        assert!("https://sessionmux.app/panel.html".starts_with(&origin_prefix()));
        assert!(!"https://sessionmux.app.example.com/".starts_with(&origin_prefix()));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn origin_is_the_custom_protocol() {
        assert_eq!(origin_prefix(), "sessionmux://app/");
        assert!(panel_url().starts_with(&origin_prefix()));
        assert!(!"sessionmux://app.example.com/".starts_with(&origin_prefix()));
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
        assert_eq!((p.x, p.x + p.w), (1828 - 10 - 380, 1828 - 10));
        assert_eq!(p.y + p.h, 1040, "clamped to the work area bottom");
        // Bubble at the left edge → right of it.
        let b = r(12, 400, 92, 480);
        let p = rect_beside(b, area, "auto", 380, 580, 10);
        assert_eq!((p.x, p.y), (102, 440 - 290));
        // Forced side, clamped back on screen.
        let p = rect_beside(b, area, "left", 380, 580, 10);
        assert_eq!(p.x, 0);
        // Tiny screen: neither fits → right, clamped.
        let p = rect_beside(r(100, 0, 180, 80), r(0, 0, 400, 600), "auto", 380, 580, 10);
        assert_eq!((p.x, p.y), (20, 0));
        assert_eq!(scaled(PANEL_W, 1.5), 570);
    }

    #[test]
    fn geometry_near_point() {
        let area = r(0, 0, 1920, 1040);
        let p = rect_near_point((1800, 1060), area, 380, 580, 10);
        assert_eq!((p.x, p.y, p.x + p.w, p.y + p.h), (1540, 460, 1920, 1040));
        // Taskbar on top: no room above → below the point.
        let p = rect_near_point((500, 20), area, 380, 580, 10);
        assert_eq!((p.x, p.y), (310, 30));
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
}

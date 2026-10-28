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

//! The panel's window on macOS: a borderless, non-activating `NSPanel` of `PANEL_W`×`PANEL_H`
//! points at the status-window level, on every Space and beside full-screen apps, with rounded
//! corners (#101114) and a shadow. WKWebView (through wry) fills it.
//!
//! The panel takes the keyboard without activating Sessionmux, so the app the user came from
//! stays active behind it. When the panel stops being the key window while it is visible
//! (another app or window was clicked), the app receives the synthetic request `__blur` and
//! decides whether to hide it (it stays while pinned). Hiding gives the activation back to the
//! previous app when Sessionmux had become active.
//!
//! Placement: beside the floating button; below the menu bar item (an anchor inside the menu
//! bar); above or below a point, or the mouse pointer when there is no anchor.

use std::ffi::c_void;
use std::ptr::NonNull;
use std::sync::Arc;

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{define_class, msg_send, sel, DefinedClass, MainThreadMarker, MainThreadOnly, Message};
use objc2_app_kit::{
    NSApplication, NSBackingStoreType, NSColor, NSEvent, NSPanel, NSScreen, NSStatusWindowLevel, NSView, NSWindow,
    NSWindowCollectionBehavior, NSWindowStyleMask,
};
use objc2_core_graphics::CGColor;
use objc2_foundation::{NSInteger, NSObjectProtocol, NSPoint, NSRect, NSSize};
use objc2_web_kit::WKWebView;
use raw_window_handle::{AppKitWindowHandle, HandleError, HasWindowHandle, RawWindowHandle, WindowHandle};
use wry::{WebView, WebViewBuilder};

use crate::platform::{Desktop, EventLoop, Os, PanelHost, Rect, WindowId};
use crate::ui::ipc::{IpcHandler, IpcRequest};
use crate::ui::panel::{rect_beside, rect_near_point, BACKGROUND, PANEL_GAP, PANEL_H, PANEL_W};
use crate::util::log;

/// Corner radius of the panel, in points.
const CORNER_RADIUS: f64 = 10.0;
/// `WKInactiveSchedulingPolicyNone`: the page keeps running while macOS considers it hidden.
const SCHEDULING_NONE: NSInteger = 2;
/// The primary display's height assumed when no display can be read.
const NOMINAL_PRIMARY_HEIGHT: f64 = 900.0;

/// What the panel window carries for its callbacks.
struct PanelIvars {
    ipc: Arc<dyn IpcHandler>,
}

define_class!(
    // SAFETY: NSPanel has no subclassing requirements; `KeyPanel` implements no `Drop`, and its
    // overrides only answer questions or call the inherited implementation first.
    #[unsafe(super(NSPanel, NSWindow))]
    #[thread_kind = MainThreadOnly]
    #[name = "SessionmuxPanelWindow"]
    #[ivars = PanelIvars]
    struct KeyPanel;

    impl KeyPanel {
        /// A borderless window only takes the keyboard when it says it can.
        #[unsafe(method(canBecomeKeyWindow))]
        fn can_become_key_window(&self) -> bool {
            true
        }

        #[unsafe(method(canBecomeMainWindow))]
        fn can_become_main_window(&self) -> bool {
            false
        }

        /// The panel lost the keyboard: tell the app, once the current event is done, if the
        /// panel is still showing then.
        #[unsafe(method(resignKeyWindow))]
        fn resign_key_window(&self) {
            // SAFETY: runs NSWindow's own `resignKeyWindow`, which takes no arguments.
            unsafe {
                let _: () = msg_send![super(self), resignKeyWindow];
            }
            if !self.isVisible() {
                return;
            }
            let ipc = self.ivars().ipc.clone();
            let number = self.windowNumber();
            <Os as EventLoop>::post(Box::new(move || {
                if window_visible(number) {
                    ipc.handle(IpcRequest { id: 0, name: "__blur".to_string(), args: Vec::new() });
                }
            }));
        }
    }

    unsafe impl NSObjectProtocol for KeyPanel {}
);

/// True when Sessionmux's window `number` exists and is on screen. Main thread only.
fn window_visible(number: NSInteger) -> bool {
    let Some(mtm) = MainThreadMarker::new() else { return false };
    NSApplication::sharedApplication(mtm).windowWithWindowNumber(number).is_some_and(|w| w.isVisible())
}

/// Height of the primary display (the one with the menu bar), in points.
fn primary_height(mtm: MainThreadMarker) -> f64 {
    NSScreen::screens(mtm).firstObject().map(|s| s.frame().size.height).unwrap_or(NOMINAL_PRIMARY_HEIGHT)
}

/// `r` (top-left origin, y downwards) as a Cocoa frame (bottom-left origin, y upwards).
fn to_cocoa(r: Rect, primary_h: f64) -> NSRect {
    NSRect::new(
        NSPoint::new(f64::from(r.x), primary_h - f64::from(r.y + r.h)),
        NSSize::new(f64::from(r.w), f64::from(r.h)),
    )
}

/// Where the panel goes for `anchor` and `side` (see [`PanelHost::show_near`]); `cursor` is the
/// mouse pointer and `work_area` gives the usable area of the display nearest to a rectangle.
fn target_rect(anchor: Rect, side: &str, cursor: (i32, i32), work_area: impl Fn(&Rect) -> Rect) -> Rect {
    if anchor.w > 0 && anchor.h > 0 {
        let area = work_area(&anchor);
        if anchor.y < area.y {
            // The menu bar item: centred below it.
            let pt = (anchor.x + anchor.w / 2, anchor.y + anchor.h);
            return rect_near_point(pt, area, PANEL_W, PANEL_H, PANEL_GAP);
        }
        return rect_beside(anchor, area, side, PANEL_W, PANEL_H, PANEL_GAP);
    }
    let pt = if anchor.x == 0 && anchor.y == 0 { cursor } else { (anchor.x, anchor.y) };
    let area = work_area(&Rect { x: pt.0, y: pt.1, w: 1, h: 1 });
    rect_near_point(pt, area, PANEL_W, PANEL_H, PANEL_GAP)
}

/// The mouse pointer in top-left desktop points.
fn cursor(mtm: MainThreadMarker) -> (i32, i32) {
    let p = NSEvent::mouseLocation();
    (p.x.round() as i32, (primary_height(mtm) - p.y).round() as i32)
}

/// Give the window's content view the panel's rounded, opaque background.
fn round_content(window: &NSWindow) {
    let Some(view) = window.contentView() else { return };
    view.setWantsLayer(true);
    let Some(layer) = view.layer() else { return };
    let (r, g, b, a) = BACKGROUND;
    let color =
        CGColor::new_srgb(f64::from(r) / 255.0, f64::from(g) / 255.0, f64::from(b) / 255.0, f64::from(a) / 255.0);
    layer.setBackgroundColor(Some(&color));
    layer.setCornerRadius(CORNER_RADIUS);
    layer.setMasksToBounds(true);
    window.invalidateShadow();
}

/// The web view inside `view`, searching its subviews.
pub(super) fn find_web_view(view: &NSView) -> Option<Retained<WKWebView>> {
    match view.retain().downcast::<WKWebView>() {
        Ok(wk) => Some(wk),
        Err(_) => view.subviews().iter().find_map(|v| find_web_view(&v)),
    }
}

/// Keep the page running and drawing while its window is off every display: macOS would
/// otherwise treat it as hidden and pause it.
fn keep_drawing_off_screen(window: &NSWindow) {
    let Some(wk) = window.contentView().and_then(|v| find_web_view(&v)) else { return };
    let occlusion = sel!(_setWindowOcclusionDetectionEnabled:);
    if wk.respondsToSelector(occlusion) {
        // SAFETY: the selector exists on this web view (checked above) and takes one BOOL.
        unsafe {
            let _: () = msg_send![&*wk, _setWindowOcclusionDetectionEnabled: false];
        }
    }
    // SAFETY: `configuration` and `preferences` return the web view's live configuration objects.
    let prefs = unsafe { wk.configuration().preferences() };
    let scheduling = sel!(setInactiveSchedulingPolicy:);
    if prefs.respondsToSelector(scheduling) {
        // SAFETY: the selector exists (macOS 14 and later, checked above) and takes an NSInteger
        // enumeration value; `SCHEDULING_NONE` is one of its values.
        unsafe {
            let _: () = msg_send![&*prefs, setInactiveSchedulingPolicy: SCHEDULING_NONE];
        }
    }
}

/// Move `window` fully onto the primary display, in its bottom-left corner above the Dock.
pub(super) fn bring_on_screen(window: &NSWindow) {
    let Some(mtm) = MainThreadMarker::new() else { return };
    let Some(screen) = NSScreen::screens(mtm).firstObject() else { return };
    let visible = screen.visibleFrame();
    let size = window.frame().size;
    window.setFrame_display(NSRect::new(visible.origin, size), true);
    window.orderFrontRegardless();
}

// ---------------------------------------------------------------- PanelWindow

/// The panel's window. Not `Send`: AppKit windows live on the main thread.
pub struct PanelWindow {
    window: Retained<KeyPanel>,
}

impl PanelWindow {
    /// Move the window to `r` (top-left desktop points) and make it fully visible and clickable.
    fn place(&self, r: Rect) {
        let mtm = self.window.mtm();
        self.window.setFrame_display(to_cocoa(r, primary_height(mtm)), true);
        self.window.setAlphaValue(1.0);
        self.window.setIgnoresMouseEvents(false);
    }

    fn target(&self, anchor: Rect, side: &str) -> Rect {
        target_rect(anchor, side, cursor(self.window.mtm()), <Os as Desktop>::work_area)
    }
}

/// The window's content view for wry, which builds WKWebView into its window.
impl HasWindowHandle for PanelWindow {
    fn window_handle(&self) -> Result<WindowHandle<'_>, HandleError> {
        let view = self.window.contentView().ok_or(HandleError::Unavailable)?;
        let ptr = NonNull::from(&*view).cast::<c_void>();
        // SAFETY: the content view is retained by the window, which this PanelWindow owns and
        // which outlives the borrow; wry only uses the handle during `build`.
        Ok(unsafe { WindowHandle::borrow_raw(RawWindowHandle::AppKit(AppKitWindowHandle::new(ptr))) })
    }
}

impl PanelHost for PanelWindow {
    const ENGINE: &'static str = "WKWebView";

    fn create(ipc: Arc<dyn IpcHandler>, near: Option<(Rect, &str)>) -> Result<Self, String> {
        let mtm =
            MainThreadMarker::new().ok_or_else(|| "the panel can only be created on the main thread".to_string())?;
        let frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(f64::from(PANEL_W), f64::from(PANEL_H)));
        let style = NSWindowStyleMask::Borderless | NSWindowStyleMask::NonactivatingPanel;
        let this = KeyPanel::alloc(mtm).set_ivars(PanelIvars { ipc });
        // SAFETY: NSWindow's designated initialiser with a valid frame, style and backing; the
        // window is created now (no deferral), so it has a window number at once.
        let window: Retained<KeyPanel> = unsafe {
            msg_send![
                super(this),
                initWithContentRect: frame,
                styleMask: style,
                backing: NSBackingStoreType::Buffered,
                defer: false
            ]
        };
        // SAFETY: the window is owned through `Retained`, so closing it must not release it a
        // second time.
        unsafe { window.setReleasedWhenClosed(false) };
        window.setFloatingPanel(true);
        window.setLevel(NSStatusWindowLevel);
        window.setCollectionBehavior(
            NSWindowCollectionBehavior::CanJoinAllSpaces
                | NSWindowCollectionBehavior::FullScreenAuxiliary
                | NSWindowCollectionBehavior::Stationary
                | NSWindowCollectionBehavior::IgnoresCycle,
        );
        window.setHidesOnDeactivate(false);
        window.setBecomesKeyOnlyIfNeeded(false);
        window.setOpaque(false);
        window.setBackgroundColor(Some(&NSColor::clearColor()));
        window.setHasShadow(true);
        round_content(&window);

        let host = PanelWindow { window };
        if let Some((anchor, side)) = near {
            host.place(host.target(anchor, side));
            host.window.orderFrontRegardless();
        }
        Ok(host)
    }

    fn configure(builder: WebViewBuilder<'_>) -> WebViewBuilder<'_> {
        builder
    }

    /// wry replaced the content view with its own: round that one too.
    fn after_build(&self, _webview: &WebView) {
        round_content(&self.window);
    }

    fn show_near(&self, anchor: Rect, side: &str) {
        self.place(self.target(anchor, side));
        self.window.orderFrontRegardless();
    }

    fn show_inactive_at(&self, x: i32, y: i32) {
        self.place(Rect { x, y, w: PANEL_W, h: PANEL_H });
        keep_drawing_off_screen(&self.window);
        self.window.orderFrontRegardless();
    }

    fn activate(&self) {
        self.window.makeKeyAndOrderFront(None::<&AnyObject>);
    }

    fn hide(&self) {
        self.window.orderOut(None::<&AnyObject>);
        let app = NSApplication::sharedApplication(self.window.mtm());
        if app.isActive() {
            // Opening the panel can activate Sessionmux; hand the activation back so the
            // keyboard returns to the app the user was in.
            app.deactivate();
            log::debug("panel: hidden; activation handed back");
        }
    }

    fn is_visible(&self) -> bool {
        self.window.isVisible()
    }

    fn window(&self) -> WindowId {
        WindowId(self.window.windowNumber() as usize)
    }

    fn destroy(self) {
        self.window.orderOut(None::<&AnyObject>);
        self.window.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::ipc::IpcRequest;

    struct Nothing;

    impl IpcHandler for Nothing {
        fn handle(&self, _req: IpcRequest) {}
    }

    /// A 1440x900 display with a 25 pt menu bar and a 70 pt Dock, and a second one to its right.
    fn area(r: &Rect) -> Rect {
        if r.x >= 1440 {
            Rect { x: 1440, y: 0, w: 1920, h: 1080 }
        } else {
            Rect { x: 0, y: 25, w: 1440, h: 805 }
        }
    }

    #[test]
    fn frames_flip_to_cocoa() {
        let f = to_cocoa(Rect { x: 10, y: 25, w: 380, h: 580 }, 900.0);
        assert_eq!((f.origin.x, f.origin.y, f.size.width, f.size.height), (10.0, 295.0, 380.0, 580.0));
        // Off every display (smoke captures): kept where asked.
        let f = to_cocoa(Rect { x: -10_000, y: 100, w: 380, h: 580 }, 900.0);
        assert_eq!((f.origin.x, f.origin.y), (-10_000.0, 220.0));
    }

    #[test]
    fn beside_the_floating_button() {
        let bubble = Rect { x: 1300, y: 600, w: 64, h: 64 };
        let r = target_rect(bubble, "auto", (0, 0), area);
        assert_eq!((r.x, r.w, r.h), (1300 - PANEL_GAP - PANEL_W, PANEL_W, PANEL_H));
        assert!(r.y >= 25 && r.y + r.h <= 830);
        let r = target_rect(Rect { x: 40, y: 300, w: 64, h: 64 }, "auto", (0, 0), area);
        assert_eq!(r.x, 40 + 64 + PANEL_GAP);
        // On the second display.
        let r = target_rect(Rect { x: 1500, y: 300, w: 64, h: 64 }, "left", (0, 0), area);
        assert_eq!(r.x, 1440);
    }

    #[test]
    fn below_the_menu_bar_item() {
        let item = Rect { x: 1100, y: 0, w: 24, h: 24 };
        let r = target_rect(item, "auto", (0, 0), area);
        assert_eq!(r.y, 24 + PANEL_GAP);
        assert_eq!(r.x, 1112 - PANEL_W / 2);
        // Near the right edge it stays on the display.
        let r = target_rect(Rect { x: 1420, y: 0, w: 20, h: 24 }, "auto", (0, 0), area);
        assert_eq!(r.x + r.w, 1440);
    }

    #[test]
    fn at_a_point_or_the_pointer() {
        let r = target_rect(Rect { x: 700, y: 800, w: 0, h: 0 }, "auto", (5, 5), area);
        assert_eq!((r.x, r.y), (700 - PANEL_W / 2, 800 - PANEL_H - PANEL_GAP));
        let r = target_rect(Rect::default(), "auto", (700, 30), area);
        assert_eq!((r.x, r.y), (700 - PANEL_W / 2, 30 + PANEL_GAP));
    }

    #[test]
    fn off_the_main_thread_nothing_is_created() {
        // Tests normally run on worker threads; on the main thread there is nothing to check.
        if MainThreadMarker::new().is_some() {
            return;
        }
        let e = PanelWindow::create(Arc::new(Nothing), None).err().unwrap_or_default();
        assert!(e.contains("main thread"));
        assert!(!window_visible(1));
    }
}

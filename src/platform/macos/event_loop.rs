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

//! The UI event loop and the display geometry, on AppKit.
//!
//! Sessionmux runs as an accessory app: no Dock icon and no visible menu bar. A hidden main menu
//! carries the standard Edit commands so text fields in the panel answer ⌘C, ⌘V and the rest.
//! Work from other threads reaches the main thread through the main dispatch queue; timers are
//! run-loop timers in the common modes, so they also fire while a menu is open.
//!
//! Links handed to Sessionmux by the system (`claude://` through Launch Services) arrive as a
//! get-URL Apple Event. The handler is installed before launching finishes, so the link that
//! launched the app is kept; links that arrive before the app has finished starting are queued
//! and delivered once it has.
//!
//! Quitting from the menu stops the loop and returns from [`run_app`], which then shuts the app
//! down. A quit the system asks for (logout, restart, shutdown) runs the same quit and shutdown
//! before AppKit ends the process. Neither path changes which app opens `claude://` links.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::ffi::CStr;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use block2::RcBlock;
use dispatch2::DispatchQueue;
use objc2::rc::Retained;
use objc2::runtime::{ProtocolObject, Sel};
use objc2::{define_class, msg_send, sel, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSApplicationDelegate, NSApplicationTerminateReply, NSEvent,
    NSEventMask, NSEventModifierFlags, NSEventType, NSMenu, NSMenuItem, NSScreen, NSWorkspace,
    NSWorkspaceDidActivateApplicationNotification, NSWorkspaceWillPowerOffNotification,
};
use objc2_core_foundation::{kCFRunLoopCommonModes, CFAbsoluteTimeGetCurrent, CFRetained, CFRunLoop, CFRunLoopTimer};
use objc2_foundation::{
    NSAppleEventDescriptor, NSAppleEventManager, NSArray, NSDate, NSDefaultRunLoopMode, NSNotification, NSObject,
    NSObjectProtocol, NSPoint, NSRect, NSString, NSURL,
};

use crate::core::relay;
use crate::platform::{Desktop, EventLoop, Os, PlatformEvent, Rect, TimerId, WindowId};
use crate::util::log;

/// The area assumed for layout while no display can be read: a common laptop screen in points,
/// below the menu bar.
const NOMINAL_WORK_AREA: Rect = Rect { x: 0, y: 25, w: 1440, h: 875 };

/// Links kept while the app is still starting; later ones are dropped (logged).
const MAX_QUEUED_URLS: usize = 16;

/// A four-character Apple Event code as the number the system uses.
const fn fourcc(c: &[u8; 4]) -> u32 {
    u32::from_be_bytes(*c)
}

/// `kInternetEventClass` and `kAEGetURL`: "open this link".
const INTERNET_EVENT_CLASS: u32 = fourcc(b"GURL");
const AE_GET_URL: u32 = fourcc(b"GURL");
/// `keyDirectObject`: the link itself.
const KEY_DIRECT_OBJECT: u32 = fourcc(b"----");
/// `kAEOpenApplication`, `keyAEPropData` and `keyAELaunchedAsLogInItem`: the launch event of an
/// app the system opened as a login item.
const AE_OPEN_APPLICATION: u32 = fourcc(b"oapp");
const KEY_AE_PROP_DATA: u32 = fourcc(b"prdt");
const AE_LAUNCHED_AS_LOGIN_ITEM: u32 = fourcc(b"lgit");

/// The app's event handler; None before `init` and after `shutdown`.
static HANDLER: Mutex<Option<fn(PlatformEvent)>> = Mutex::new(None);
/// True between `init` and `shutdown`: posted work and events are delivered.
static ALIVE: AtomicBool = AtomicBool::new(false);
/// True once the app finished starting (links are delivered instead of queued).
static LAUNCHED: AtomicBool = AtomicBool::new(false);
/// Links received before the app finished starting.
static QUEUED_URLS: Mutex<Vec<String>> = Mutex::new(Vec::new());
/// Set when the loop was asked to stop.
static STOPPING: AtomicBool = AtomicBool::new(false);
/// Set once a quit was sent to the app, so repeated requests from the system send one.
static QUIT_SENT: AtomicBool = AtomicBool::new(false);
/// The exit code [`EventLoop::run`] returns.
static EXIT_CODE: AtomicI32 = AtomicI32::new(0);

/// Starts the app once AppKit finished launching; the flag says the system opened Sessionmux as a
/// login item.
type Launch = Box<dyn FnOnce(bool) -> Result<(), String>>;

thread_local! {
    static DELEGATE: RefCell<Option<Retained<Delegate>>> = const { RefCell::new(None) };
    static TIMERS: RefCell<HashMap<TimerId, CFRetained<CFRunLoopTimer>>> = RefCell::new(HashMap::new());
    static LAUNCH: RefCell<Option<Launch>> = const { RefCell::new(None) };
    static FINISH: Cell<Option<fn()>> = const { Cell::new(None) };
    /// The launch was a login-item launch (read while launching).
    static LOGIN_ITEM: Cell<bool> = const { Cell::new(false) };
    /// The app started without error, so it is shut down at the end.
    static STARTED: Cell<bool> = const { Cell::new(false) };
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements and `Delegate` implements no `Drop`.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "SessionmuxAppDelegate"]
    struct Delegate;

    impl Delegate {
        /// The get-URL Apple Event: one link the system hands to Sessionmux.
        #[unsafe(method(handleGetURLEvent:withReplyEvent:))]
        fn handle_get_url(&self, event: &NSAppleEventDescriptor, _reply: &NSAppleEventDescriptor) {
            match direct_string(event) {
                Some(s) => url_received(&s),
                None => log::warn("event loop: a link event carried no link"),
            }
        }

        /// The system is shutting down or restarting.
        #[unsafe(method(willPowerOff:))]
        fn will_power_off(&self, _n: &NSNotification) {
            log::info("event loop: the system is powering off");
            send_quit();
        }

        /// Another app (or Sessionmux) became the active one.
        #[unsafe(method(didActivateApp:))]
        fn did_activate_app(&self, _n: &NSNotification) {
            // Read now, when the notification arrives: the app just activated is the one in front.
            let front = NSWorkspace::sharedWorkspace().frontmostApplication().map(|a| a.processIdentifier());
            if let Some(pid) = front.and_then(|p| u32::try_from(p).ok()).filter(|&p| p != 0 && p != std::process::id()) {
                post_event(PlatformEvent::FrontChanged(pid));
            }
        }
    }

    unsafe impl NSObjectProtocol for Delegate {}

    unsafe impl NSApplicationDelegate for Delegate {
        #[unsafe(method(applicationWillFinishLaunching:))]
        fn will_finish_launching(&self, _n: &NSNotification) {
            // Installed before launching finishes so the link that launched the app arrives here.
            install_url_handler(self);
            LOGIN_ITEM.set(LOGIN_ITEM.get() || launched_as_login_item());
        }

        #[unsafe(method(applicationDidFinishLaunching:))]
        fn did_finish_launching(&self, _n: &NSNotification) {
            LOGIN_ITEM.set(LOGIN_ITEM.get() || launched_as_login_item());
            finish_launching();
        }

        /// Opened again from Finder, the Dock or Spotlight while running: show the panel.
        #[unsafe(method(applicationShouldHandleReopen:hasVisibleWindows:))]
        fn should_handle_reopen(&self, _sender: &NSApplication, _visible: bool) -> bool {
            post_event(PlatformEvent::ShowPanel);
            false
        }

        /// Links delivered by AppKit itself (when no get-URL handler is installed).
        #[unsafe(method(application:openURLs:))]
        fn open_urls(&self, _application: &NSApplication, urls: &NSArray<NSURL>) {
            for u in urls.iter() {
                if let Some(s) = u.absoluteString() {
                    url_received(&s.to_string());
                }
            }
        }

        /// A quit the system asks for (logout, restart, a quit request from another app): run the
        /// app's quit and shutdown now, then let AppKit end the process.
        #[unsafe(method(applicationShouldTerminate:))]
        fn should_terminate(&self, _sender: &NSApplication) -> NSApplicationTerminateReply {
            log::info("event loop: the system asked Sessionmux to quit");
            send_quit();
            run_finish();
            NSApplicationTerminateReply::TerminateNow
        }
    }
);

impl Delegate {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(());
        // SAFETY: plain NSObject initialiser on a freshly allocated instance.
        unsafe { msg_send![super(this), init] }
    }
}

/// The string of an Apple Event's direct parameter (a get-URL event's link).
fn direct_string(event: &NSAppleEventDescriptor) -> Option<String> {
    // SAFETY: `paramDescriptorForKeyword:` takes an AEKeyword (a 32-bit code) and returns an
    // autoreleased descriptor or nil.
    let d: Option<Retained<NSAppleEventDescriptor>> =
        unsafe { msg_send![event, paramDescriptorForKeyword: KEY_DIRECT_OBJECT] };
    d.and_then(|d| d.stringValue()).map(|s| s.to_string())
}

/// Deliver `ev` to the app's handler now, on the main thread. Dropped before `init` and after
/// `shutdown`.
fn dispatch(ev: PlatformEvent) {
    if !ALIVE.load(Ordering::Acquire) {
        return;
    }
    let handler = HANDLER.lock().ok().and_then(|g| *g);
    if let Some(h) = handler {
        h(ev);
    }
}

/// Deliver `ev` from the top of the loop, after the current callback returned.
pub(crate) fn post_event(ev: PlatformEvent) {
    <Os as EventLoop>::post(Box::new(move || dispatch(ev)));
}

/// Ask the app to quit, once.
fn send_quit() {
    if !QUIT_SENT.swap(true, Ordering::AcqRel) {
        dispatch(PlatformEvent::QuitRequested);
    }
}

/// Run the app's shutdown once (at the end of [`run_app`], or for a quit the system asked for).
fn run_finish() {
    if !STARTED.get() {
        return;
    }
    if let Some(f) = FINISH.take() {
        f();
    }
}

/// A link of at most `relay::MAX_URL_LEN + 1` characters: anything longer is cut there, so it is
/// still refused as too long instead of being forwarded shortened.
fn cap_url(s: &str) -> String {
    match s.char_indices().nth(relay::MAX_URL_LEN + 1) {
        Some((at, _)) => s[..at].to_string(),
        None => s.to_string(),
    }
}

/// A link from the system: delivered as `OpenUrl` once the app has started, queued before.
fn url_received(raw: &str) {
    let url = cap_url(raw);
    if LAUNCHED.load(Ordering::Acquire) {
        post_event(PlatformEvent::OpenUrl(url));
        return;
    }
    let Ok(mut q) = QUEUED_URLS.lock() else { return };
    if q.len() < MAX_QUEUED_URLS {
        q.push(url);
    } else {
        log::warn("event loop: too many links before start; one was dropped");
    }
}

/// Mark the app as started and deliver the links that arrived before.
fn deliver_queued_urls() {
    LAUNCHED.store(true, Ordering::Release);
    let queued = QUEUED_URLS.lock().map(|mut q| std::mem::take(&mut *q)).unwrap_or_default();
    for url in queued {
        post_event(PlatformEvent::OpenUrl(url));
    }
}

/// AppKit finished launching: start the app (when [`run_app`] gave a start), then deliver the
/// queued links. A start that fails ends the loop with exit code 1.
fn finish_launching() {
    let Some(launch) = LAUNCH.with(|l| l.borrow_mut().take()) else {
        deliver_queued_urls();
        return;
    };
    match launch(LOGIN_ITEM.get()) {
        Ok(()) => {
            STARTED.set(true);
            deliver_queued_urls();
        }
        Err(_) => {
            EXIT_CODE.store(1, Ordering::Release);
            stop_loop();
        }
    }
}

/// True when the event being handled is the launch event of a login-item start.
fn launched_as_login_item() -> bool {
    let mgr = NSAppleEventManager::sharedAppleEventManager();
    // SAFETY: `currentAppleEvent` returns the event being handled, or nil.
    let ev: Option<Retained<NSAppleEventDescriptor>> = unsafe { msg_send![&*mgr, currentAppleEvent] };
    let Some(ev) = ev else { return false };
    // SAFETY: `eventID` returns the event's 32-bit id code.
    let id: u32 = unsafe { msg_send![&*ev, eventID] };
    if id != AE_OPEN_APPLICATION {
        return false;
    }
    // SAFETY: as in `direct_string`.
    let prop: Option<Retained<NSAppleEventDescriptor>> =
        unsafe { msg_send![&*ev, paramDescriptorForKeyword: KEY_AE_PROP_DATA] };
    prop.is_some_and(|p| {
        // SAFETY: `enumCodeValue` returns a 32-bit code (0 when the value is no enumeration).
        let code: u32 = unsafe { msg_send![&*p, enumCodeValue] };
        code == AE_LAUNCHED_AS_LOGIN_ITEM
    })
}

/// Route get-URL Apple Events to `delegate`.
fn install_url_handler(delegate: &Delegate) {
    let mgr = NSAppleEventManager::sharedAppleEventManager();
    // SAFETY: the delegate implements `handleGetURLEvent:withReplyEvent:` with the signature the
    // manager calls; it stays alive in `DELEGATE` until the handler is removed.
    unsafe {
        let _: () = msg_send![
            &*mgr,
            setEventHandler: delegate,
            andSelector: sel!(handleGetURLEvent:withReplyEvent:),
            forEventClass: INTERNET_EVENT_CLASS,
            andEventID: AE_GET_URL
        ];
    }
}

fn remove_url_handler() {
    let mgr = NSAppleEventManager::sharedAppleEventManager();
    // SAFETY: removing a handler that may or may not be installed is harmless.
    unsafe {
        let _: () = msg_send![&*mgr, removeEventHandlerForEventClass: INTERNET_EVENT_CLASS, andEventID: AE_GET_URL];
    }
}

/// The Edit commands of the hidden main menu: title, action, key equivalent, with Shift.
const EDIT_ITEMS: [(&str, &CStr, &str, bool); 6] = [
    ("Undo", c"undo:", "z", false),
    ("Redo", c"redo:", "z", true),
    ("Cut", c"cut:", "x", false),
    ("Copy", c"copy:", "c", false),
    ("Paste", c"paste:", "v", false),
    ("Select All", c"selectAll:", "a", false),
];

/// The main menu: an empty application menu (AppKit treats the first item as that) and Edit,
/// whose commands go to the focused text field (nil target).
fn main_menu(mtm: MainThreadMarker) -> Retained<NSMenu> {
    let menu = NSMenu::new(mtm);
    let app_item = NSMenuItem::new(mtm);
    app_item.setSubmenu(Some(&NSMenu::new(mtm)));
    menu.addItem(&app_item);

    let title = NSString::from_str("Edit");
    let edit = NSMenu::initWithTitle(NSMenu::alloc(mtm), &title);
    for (i, (label, action, key, shift)) in EDIT_ITEMS.into_iter().enumerate() {
        if i == 2 {
            edit.addItem(&NSMenuItem::separatorItem(mtm));
        }
        // SAFETY: `action` is a valid selector name; with no target the action travels along the
        // responder chain.
        let item = unsafe {
            NSMenuItem::initWithTitle_action_keyEquivalent(
                NSMenuItem::alloc(mtm),
                &NSString::from_str(label),
                Some(Sel::register(action)),
                &NSString::from_str(key),
            )
        };
        if shift {
            item.setKeyEquivalentModifierMask(NSEventModifierFlags::Command | NSEventModifierFlags::Shift);
        }
        edit.addItem(&item);
    }
    let edit_item = NSMenuItem::new(mtm);
    edit_item.setTitle(&title);
    edit_item.setSubmenu(Some(&edit));
    menu.addItem(&edit_item);
    menu
}

/// Create the shared application as an accessory app with its delegate and hidden main menu,
/// once. Main thread only.
pub fn prepare(mtm: MainThreadMarker) {
    if DELEGATE.with(|d| d.borrow().is_some()) {
        return;
    }
    let app = NSApplication::sharedApplication(mtm);
    if !app.setActivationPolicy(NSApplicationActivationPolicy::Accessory) {
        log::warn("event loop: the accessory activation policy was refused");
    }
    let delegate = Delegate::new(mtm);
    app.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    app.setMainMenu(Some(&main_menu(mtm)));
    // Also installed here, so a link is caught when launching finished before the delegate was set.
    install_url_handler(&delegate);
    let center = NSWorkspace::sharedWorkspace().notificationCenter();
    // SAFETY: the delegate implements `willPowerOff:` and `didActivateApp:`, each taking the
    // notification; it stays alive in `DELEGATE` and is removed as an observer in `shutdown`.
    unsafe {
        center.addObserver_selector_name_object(
            &delegate,
            sel!(willPowerOff:),
            Some(NSWorkspaceWillPowerOffNotification),
            None,
        );
        center.addObserver_selector_name_object(
            &delegate,
            sel!(didActivateApp:),
            Some(NSWorkspaceDidActivateApplicationNotification),
            None,
        );
    }
    DELEGATE.with(|d| *d.borrow_mut() = Some(delegate));
}

/// Wake the loop with an empty event so a requested stop takes effect at once.
fn wake(app: &NSApplication) {
    let ev = NSEvent::otherEventWithType_location_modifierFlags_timestamp_windowNumber_context_subtype_data1_data2(
        NSEventType::ApplicationDefined,
        NSPoint::new(0.0, 0.0),
        NSEventModifierFlags::empty(),
        0.0,
        0,
        None,
        0,
        0,
        0,
    );
    if let Some(ev) = ev {
        app.postEvent_atStart(&ev, false);
    }
}

/// End `NSApplication::run` after the current event.
fn stop_loop() {
    STOPPING.store(true, Ordering::Release);
    let Some(mtm) = MainThreadMarker::new() else {
        <Os as EventLoop>::post(Box::new(stop_loop));
        return;
    };
    let app = NSApplication::sharedApplication(mtm);
    app.stop(None);
    wake(&app);
}

/// Run AppKit's loop: `launch` starts the app once AppKit finished launching (its flag says the
/// system opened Sessionmux as a login item) and `finish` shuts it down at the end, also when the
/// system ends the process for a logout. Returns the exit code. Main thread only.
pub fn run_app(launch: impl FnOnce(bool) -> Result<(), String> + 'static, finish: fn()) -> i32 {
    let Some(mtm) = MainThreadMarker::new() else {
        log::error("event loop: not on the main thread");
        return 1;
    };
    prepare(mtm);
    LAUNCH.with(|l| *l.borrow_mut() = Some(Box::new(launch)));
    FINISH.set(Some(finish));
    let code = <Os as EventLoop>::run();
    if STARTED.get() {
        run_finish();
    } else {
        <Os as EventLoop>::shutdown();
    }
    code
}

/// Handle events for `ms` milliseconds without running the full loop (the self-check drives the
/// UI this way). Sleeps when called off the main thread.
#[allow(dead_code)]
pub fn pump(ms: u64) {
    let Some(mtm) = MainThreadMarker::new() else {
        std::thread::sleep(Duration::from_millis(ms));
        return;
    };
    let app = NSApplication::sharedApplication(mtm);
    let end = Instant::now() + Duration::from_millis(ms);
    loop {
        let left = end.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }
        let until = NSDate::dateWithTimeIntervalSinceNow(left.as_secs_f64());
        // SAFETY: reading an immutable extern string constant of Foundation.
        let mode = unsafe { NSDefaultRunLoopMode };
        if let Some(ev) = app.nextEventMatchingMask_untilDate_inMode_dequeue(NSEventMask::Any, Some(&until), mode, true)
        {
            app.sendEvent(&ev);
        }
        app.updateWindows();
    }
}

/// Run `f` now when on the main thread, else post it there.
fn on_main(f: impl FnOnce() + Send + 'static) {
    if MainThreadMarker::new().is_some() {
        f();
    } else {
        <Os as EventLoop>::post(Box::new(f));
    }
}

/// Timer interval in seconds; at least 1 ms.
fn interval_secs(ms: u32) -> f64 {
    f64::from(ms.max(1)) / 1000.0
}

fn remove_timer(id: TimerId) {
    if let Some(t) = TIMERS.with(|m| m.borrow_mut().remove(&id)) {
        t.invalidate();
    }
}

fn add_timer(id: TimerId, ms: u32) {
    remove_timer(id);
    let secs = interval_secs(ms);
    let block = RcBlock::new(move |_t: *mut CFRunLoopTimer| dispatch(PlatformEvent::Timer(id)));
    // SAFETY: the default allocator; the timer copies the block, which only captures `id`.
    let timer =
        unsafe { CFRunLoopTimer::with_handler(None, CFAbsoluteTimeGetCurrent() + secs, secs, 0, 0, Some(&block)) };
    let (Some(timer), Some(rl)) = (timer, CFRunLoop::main()) else {
        log::warn(&format!("event loop: timer {id:?} could not be created"));
        return;
    };
    // SAFETY: reading an immutable extern constant of CoreFoundation.
    rl.add_timer(Some(&timer), unsafe { kCFRunLoopCommonModes });
    TIMERS.with(|m| m.borrow_mut().insert(id, timer));
}

impl EventLoop for Os {
    fn init(on_event: fn(PlatformEvent)) -> Result<(), String> {
        let mtm = MainThreadMarker::new().ok_or_else(|| "the event loop must start on the main thread".to_string())?;
        prepare(mtm);
        if let Ok(mut g) = HANDLER.lock() {
            *g = Some(on_event);
        }
        STOPPING.store(false, Ordering::Release);
        QUIT_SENT.store(false, Ordering::Release);
        ALIVE.store(true, Ordering::Release);
        Ok(())
    }

    fn run() -> i32 {
        let Some(mtm) = MainThreadMarker::new() else {
            log::error("event loop: run off the main thread");
            return 1;
        };
        if !STOPPING.load(Ordering::Acquire) {
            NSApplication::sharedApplication(mtm).run();
        }
        EXIT_CODE.load(Ordering::Acquire)
    }

    fn post(f: Box<dyn FnOnce() + Send>) {
        if !ALIVE.load(Ordering::Acquire) {
            log::debug("event loop: a task was dropped (no event loop)");
            return;
        }
        DispatchQueue::main().exec_async(move || {
            if ALIVE.load(Ordering::Acquire) {
                f();
            } else {
                log::debug("event loop: a task was dropped (event loop ended)");
            }
        });
    }

    fn set_timer(id: TimerId, ms: u32) {
        on_main(move || add_timer(id, ms));
    }

    fn kill_timer(id: TimerId) {
        on_main(move || remove_timer(id));
    }

    fn quit() {
        QUIT_SENT.store(true, Ordering::Release);
        stop_loop();
    }

    fn shutdown() {
        ALIVE.store(false, Ordering::Release);
        if let Ok(mut g) = HANDLER.lock() {
            *g = None;
        }
        if MainThreadMarker::new().is_none() {
            return;
        }
        let timers: Vec<_> = TIMERS.with(|m| m.borrow_mut().drain().map(|(_, t)| t).collect());
        for t in timers {
            t.invalidate();
        }
        if let Some(d) = DELEGATE.with(|d| d.borrow().clone()) {
            remove_url_handler();
            let center = NSWorkspace::sharedWorkspace().notificationCenter();
            // SAFETY: the delegate was added as an observer in `prepare`.
            unsafe { center.removeObserver(&d) };
        }
    }
}

/// A Cocoa rectangle (origin at the bottom-left of the primary display, y upwards) in the
/// top-left coordinates of [`Rect`]; `primary_h` is the primary display's height.
fn to_top_left(r: NSRect, primary_h: f64) -> Rect {
    let y = primary_h - (r.origin.y + r.size.height);
    Rect {
        x: r.origin.x.round() as i32,
        y: y.round() as i32,
        w: r.size.width.round() as i32,
        h: r.size.height.round() as i32,
    }
}

/// Area two rectangles share.
fn overlap(a: &Rect, b: &Rect) -> i64 {
    let w = (a.x + a.w).min(b.x + b.w) - a.x.max(b.x);
    let h = (a.y + a.h).min(b.y + b.h) - a.y.max(b.y);
    if w > 0 && h > 0 {
        i64::from(w) * i64::from(h)
    } else {
        0
    }
}

/// Squared distance from a point to a rectangle (0 inside).
fn distance2(s: &Rect, x: i32, y: i32) -> i64 {
    let dx = i64::from((s.x - x).max(x - (s.x + s.w)).max(0));
    let dy = i64::from((s.y - y).max(y - (s.y + s.h)).max(0));
    dx * dx + dy * dy
}

/// The display for `r`: the one holding its centre, else the one it overlaps most, else the one
/// nearest to its centre. None without displays.
fn nearest_screen(screens: &[Rect], r: &Rect) -> Option<usize> {
    let (cx, cy) = (r.x + r.w / 2, r.y + r.h / 2);
    if let Some(i) = screens.iter().position(|s| cx >= s.x && cx < s.x + s.w && cy >= s.y && cy < s.y + s.h) {
        return Some(i);
    }
    let most = (0..screens.len()).max_by_key(|&i| overlap(&screens[i], r));
    if let Some(i) = most.filter(|&i| overlap(&screens[i], r) > 0) {
        return Some(i);
    }
    (0..screens.len()).min_by_key(|&i| distance2(&screens[i], cx, cy))
}

impl Desktop for Os {
    /// The visible frame (without menu bar and Dock) of the display nearest to `r`.
    fn work_area(r: &Rect) -> Rect {
        let Some(mtm) = MainThreadMarker::new() else { return NOMINAL_WORK_AREA };
        let screens: Vec<Retained<NSScreen>> = NSScreen::screens(mtm).iter().collect();
        let Some(primary) = screens.first() else { return NOMINAL_WORK_AREA };
        let primary_h = primary.frame().size.height;
        let frames: Vec<Rect> = screens.iter().map(|s| to_top_left(s.frame(), primary_h)).collect();
        match nearest_screen(&frames, r) {
            Some(i) => to_top_left(screens[i].visibleFrame(), primary_h),
            None => NOMINAL_WORK_AREA,
        }
    }

    /// AppKit lays out in points already.
    fn units_per_dip(_r: &Rect) -> f32 {
        1.0
    }

    /// True when `w` (the window number of one of Sessionmux's windows) is the key window.
    fn is_foreground(w: WindowId) -> bool {
        let Some(mtm) = MainThreadMarker::new() else { return false };
        NSApplication::sharedApplication(mtm).keyWindow().is_some_and(|k| k.windowNumber() as usize == w.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use objc2_foundation::NSSize;

    #[test]
    fn four_character_codes() {
        assert_eq!(INTERNET_EVENT_CLASS, 0x4755_524C);
        assert_eq!(AE_GET_URL, INTERNET_EVENT_CLASS);
        assert_eq!(KEY_DIRECT_OBJECT, 0x2D2D_2D2D);
        assert_eq!(AE_OPEN_APPLICATION, 0x6F61_7070);
        assert_eq!(KEY_AE_PROP_DATA, 0x7072_6474);
        assert_eq!(AE_LAUNCHED_AS_LOGIN_ITEM, 0x6C67_6974);
    }

    #[test]
    fn links_are_capped_so_long_ones_stay_refused() {
        assert_eq!(cap_url("claude://a"), "claude://a");
        let long = format!("claude://{}", "é".repeat(relay::MAX_URL_LEN * 2));
        let capped = cap_url(&long);
        assert_eq!(capped.chars().count(), relay::MAX_URL_LEN + 1);
        assert!(relay::validate_url(&capped).is_err());
        let max = format!("claude://{}", "a".repeat(relay::MAX_URL_LEN - 9));
        assert_eq!(cap_url(&max), max);
        assert!(relay::validate_url(&cap_url(&max)).is_ok());
    }

    #[test]
    fn links_before_start_are_queued_and_bounded() {
        QUEUED_URLS.lock().unwrap().clear();
        let launched = LAUNCHED.swap(false, Ordering::AcqRel);
        for i in 0..MAX_QUEUED_URLS + 3 {
            url_received(&format!("claude://x/{i}"));
        }
        let q = std::mem::take(&mut *QUEUED_URLS.lock().unwrap());
        LAUNCHED.store(launched, Ordering::Release);
        assert_eq!(q.len(), MAX_QUEUED_URLS);
        assert_eq!(q[0], "claude://x/0");
    }

    #[test]
    fn edit_menu_has_the_standard_commands() {
        let actions: Vec<&str> = EDIT_ITEMS.iter().map(|(_, a, _, _)| a.to_str().unwrap()).collect();
        assert_eq!(actions, ["undo:", "redo:", "cut:", "copy:", "paste:", "selectAll:"]);
        let keys: Vec<(&str, bool)> = EDIT_ITEMS.iter().map(|(_, _, k, s)| (*k, *s)).collect();
        assert_eq!(keys, [("z", false), ("z", true), ("x", false), ("c", false), ("v", false), ("a", false)]);
    }

    #[test]
    fn intervals_are_seconds() {
        assert_eq!(interval_secs(1500), 1.5);
        assert_eq!(interval_secs(0), 0.001);
    }

    fn ns(x: f64, y: f64, w: f64, h: f64) -> NSRect {
        NSRect::new(NSPoint::new(x, y), NSSize::new(w, h))
    }

    #[test]
    fn cocoa_rects_flip_to_top_left() {
        // Primary display 1440x900 with a 25 pt menu bar and a 70 pt Dock at the bottom.
        assert_eq!(to_top_left(ns(0.0, 70.0, 1440.0, 805.0), 900.0), Rect { x: 0, y: 25, w: 1440, h: 805 });
        // A display above the primary one has a negative top.
        assert_eq!(to_top_left(ns(0.0, 900.0, 1920.0, 1080.0), 900.0), Rect { x: 0, y: -1080, w: 1920, h: 1080 });
    }

    #[test]
    fn nearest_screen_rules() {
        let a = Rect { x: 0, y: 0, w: 1440, h: 900 };
        let b = Rect { x: 1440, y: 0, w: 1920, h: 1080 };
        let s = [a, b];
        assert_eq!(nearest_screen(&s, &Rect { x: 10, y: 10, w: 50, h: 50 }), Some(0));
        assert_eq!(nearest_screen(&s, &Rect { x: 1500, y: 10, w: 50, h: 50 }), Some(1));
        // Centre on no display: the one overlapped most.
        assert_eq!(nearest_screen(&s, &Rect { x: 1400, y: 880, w: 100, h: 100 }), Some(1));
        // Overlapping none: the nearest.
        assert_eq!(nearest_screen(&s, &Rect { x: -500, y: 10, w: 50, h: 50 }), Some(0));
        assert_eq!(nearest_screen(&[], &a), None);
    }

    #[test]
    fn off_the_main_thread_geometry_is_nominal() {
        // Tests normally run on worker threads; on the main thread there is nothing to check.
        if MainThreadMarker::new().is_some() {
            return;
        }
        let r = Rect { x: 10, y: 10, w: 1, h: 1 };
        assert_eq!(Os::units_per_dip(&r), 1.0);
        assert_eq!(Os::work_area(&r), NOMINAL_WORK_AREA);
        assert!(!Os::is_foreground(WindowId(1)));
    }

    #[test]
    fn off_the_main_thread_init_fails() {
        if MainThreadMarker::new().is_some() {
            return;
        }
        assert!(Os::init(|_| {}).is_err());
    }
}

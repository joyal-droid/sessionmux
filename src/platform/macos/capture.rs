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

//! What the smoke run needs on macOS: a UI pump, panel captures and memory figures.
//!
//! The panel is captured with WKWebView's own snapshot, which needs no Screen Recording
//! permission and works while the window is off every display. If no snapshot arrives there,
//! the window is moved onto the primary display and the snapshot is taken again.
//!
//! Memory: resident size and physical footprint (`proc_pid_rusage`) of Sessionmux, and with the
//! web engine counted in, of the WebKit processes macOS runs on Sessionmux's behalf (those whose
//! responsible process is Sessionmux).

use std::cell::{Cell, RefCell};
use std::ffi::{c_int, c_void, CStr};
use std::path::Path;
use std::ptr::NonNull;
use std::rc::Rc;
use std::time::{Duration, Instant};

use block2::RcBlock;
use objc2::msg_send;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObjectProtocol, ProtocolObject};
use objc2::MainThreadMarker;
use objc2_app_kit::{NSApplication, NSImage};
use objc2_core_foundation::CFRetained;
use objc2_core_graphics::{
    CGBitmapContextCreate, CGColorSpace, CGContext, CGImage, CGImageAlphaInfo, CGImageByteOrderInfo,
};
use objc2_foundation::{NSActivityOptions, NSError, NSNumber, NSPoint, NSProcessInfo, NSRect, NSSize, NSString};
use objc2_web_kit::{WKSnapshotConfiguration, WKWebView};

use super::event_loop;
use super::panel::{bring_on_screen, find_web_view};
use crate::platform::{Capture, Footprint, Os, WindowId};
use crate::ui::panel::encode_png;
use crate::util::log;

/// How long one snapshot may take before it counts as failed.
const SNAPSHOT_TIMEOUT: Duration = Duration::from_secs(5);
/// Settling time after the window was moved onto a display.
const ON_SCREEN_SETTLE_MS: u64 = 400;
/// `RTLD_DEFAULT`: look a symbol up in every image loaded into the process.
const RTLD_DEFAULT: *mut c_void = -2isize as *mut c_void;

thread_local! {
    /// Keeps the process out of App Nap for the self-check (held until the process ends).
    static ACTIVITY: RefCell<Option<Retained<ProtocolObject<dyn NSObjectProtocol>>>> = const { RefCell::new(None) };
    /// Set once the panel was moved onto a display because no snapshot arrived off it.
    static MOVED_ON_SCREEN: Cell<bool> = const { Cell::new(false) };
}

/// Prepare AppKit for the self-check, which drives the UI with [`Capture::pump`] instead of the
/// app's run loop: finish launching (so windows and the web view behave as in a normal start)
/// and keep timers running at full rate. Main thread only; once.
pub fn begin_self_check() {
    let Some(mtm) = MainThreadMarker::new() else { return };
    let app = NSApplication::sharedApplication(mtm);
    if !app.isRunning() {
        app.finishLaunching();
    }
    ACTIVITY.with(|a| {
        if a.borrow().is_none() {
            let reason = NSString::from_str("Sessionmux self-check");
            let token =
                NSProcessInfo::processInfo().beginActivityWithOptions_reason(NSActivityOptions::UserInitiated, &reason);
            *a.borrow_mut() = Some(token);
        }
    });
}

/// What a snapshot request ends with, once it has.
type SnapshotSlot = Rc<RefCell<Option<Result<Retained<NSImage>, String>>>>;

/// Ask `wk` for a snapshot of what it shows and wait for it while running UI events.
fn snapshot(wk: &WKWebView, mtm: MainThreadMarker) -> Result<Retained<NSImage>, String> {
    let result: SnapshotSlot = Rc::new(RefCell::new(None));
    let slot = result.clone();
    let block = RcBlock::new(move |image: *mut NSImage, error: *mut NSError| {
        // SAFETY: WebKit passes a valid image or nil, and a valid error or nil, both alive for
        // the duration of this call; retaining keeps the image afterwards.
        let image = unsafe { Retained::retain(image) };
        let outcome = match image {
            Some(i) => Ok(i),
            None => {
                // SAFETY: as above, `error` is nil or a valid NSError.
                let why = unsafe { error.as_ref() }.map(|e| e.localizedDescription().to_string());
                Err(why.unwrap_or_else(|| "no image".to_string()))
            }
        };
        *slot.borrow_mut() = Some(outcome);
    });
    let width = wk.bounds().size.width;
    // SAFETY: a new snapshot configuration, then plain property setters on it; this function
    // owns it.
    let config = unsafe {
        let config = WKSnapshotConfiguration::new(mtm);
        config.setSnapshotWidth(Some(&NSNumber::new_f64(width)));
        config.setAfterScreenUpdates(false);
        config
    };
    // SAFETY: `config` and `block` are valid for the call; WebKit copies the block and calls it
    // once on the main thread.
    unsafe { wk.takeSnapshotWithConfiguration_completionHandler(Some(&config), &block) };
    let deadline = Instant::now() + SNAPSHOT_TIMEOUT;
    while result.borrow().is_none() && Instant::now() < deadline {
        event_loop::pump(10);
    }
    let outcome = result.borrow_mut().take();
    outcome.unwrap_or_else(|| Err(format!("no snapshot within {} s", SNAPSHOT_TIMEOUT.as_secs())))
}

/// Premultiplied BGRA (as CoreGraphics draws it) to straight BGRA, in place.
fn unpremultiply(bgra: &mut [u8]) {
    for px in bgra.as_chunks_mut::<4>().0 {
        let a = u32::from(px[3]);
        if a == 0 || a == 255 {
            continue;
        }
        for c in &mut px[..3] {
            *c = ((u32::from(*c) * 255 + a / 2) / a).min(255) as u8;
        }
    }
}

/// The pixels of `image`: top-down straight BGRA and the size.
fn image_pixels(image: &CGImage) -> Result<(Vec<u8>, u32, u32), String> {
    let (w, h) = (CGImage::width(Some(image)), CGImage::height(Some(image)));
    if w == 0 || h == 0 {
        return Err("the snapshot is empty".into());
    }
    let mut buf = vec![0u8; w * h * 4];
    let space = CGColorSpace::new_device_rgb().ok_or("no RGB colour space")?;
    let info = CGImageAlphaInfo::PremultipliedFirst.0 | CGImageByteOrderInfo::Order32Little.0;
    // SAFETY: `buf` holds `w * h * 4` bytes, matching the width, height and row length given;
    // it outlives the context, which is dropped at the end of this function.
    let ctx = unsafe { CGBitmapContextCreate(buf.as_mut_ptr().cast(), w, h, 8, w * 4, Some(&space), info) }
        .ok_or("could not create a bitmap")?;
    let rect = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(w as f64, h as f64));
    CGContext::draw_image(Some(&ctx), rect, Some(image));
    drop(ctx);
    unpremultiply(&mut buf);
    Ok((buf, w as u32, h as u32))
}

/// The bitmap behind `image`, at its best resolution.
fn cg_image(image: &NSImage) -> Option<CFRetained<CGImage>> {
    // SAFETY: `CGImageForProposedRect:context:hints:` takes a rectangle pointer, a graphics
    // context and a hints dictionary, all of which may be null (null asks for the image's best
    // representation), and returns a CGImage owned by the image, or null.
    let raw: *mut CGImage = unsafe {
        msg_send![
            image,
            CGImageForProposedRect: std::ptr::null_mut::<NSRect>(),
            context: std::ptr::null::<AnyObject>(),
            hints: std::ptr::null::<AnyObject>()
        ]
    };
    // SAFETY: a non-null result is a valid CGImage; retaining it keeps it alive past the image.
    NonNull::new(raw).map(|p| unsafe { CFRetained::retain(p) })
}

/// Snapshot Sessionmux's window `w` (the panel) and write it as PNG.
fn panel_capture(w: WindowId, path: &Path) -> Result<(u32, u32), String> {
    let mtm = MainThreadMarker::new().ok_or("captures run on the main thread")?;
    let window = NSApplication::sharedApplication(mtm)
        .windowWithWindowNumber(w.0 as isize)
        .ok_or_else(|| format!("no window {}", w.0))?;
    let wk = window.contentView().and_then(|v| find_web_view(&v)).ok_or("the window holds no web view")?;
    let image = match snapshot(&wk, mtm) {
        Ok(i) => i,
        Err(e) if !MOVED_ON_SCREEN.get() => {
            log::warn(&format!("capture: no snapshot off-screen ({e}); moving the panel onto the display"));
            MOVED_ON_SCREEN.set(true);
            bring_on_screen(&window);
            event_loop::pump(ON_SCREEN_SETTLE_MS);
            snapshot(&wk, mtm)?
        }
        Err(e) => return Err(format!("snapshot: {e}")),
    };
    let cg = cg_image(&image).ok_or("the snapshot has no bitmap")?;
    let (pixels, pw, ph) = image_pixels(&cg)?;
    std::fs::write(path, encode_png(pw, ph, &pixels)).map_err(|e| format!("write {}: {e}", path.display()))?;
    Ok((pw, ph))
}

/// (resident size, physical footprint) of `pid` in bytes.
fn usage(pid: libc::pid_t) -> Option<(u64, u64)> {
    // SAFETY: an all-zero rusage_info_v4 is a valid value of this plain C struct.
    let mut info: libc::rusage_info_v4 = unsafe { std::mem::zeroed() };
    let out = (&mut info as *mut libc::rusage_info_v4).cast::<libc::rusage_info_t>();
    // SAFETY: `out` points to a rusage_info_v4, the struct the V4 flavour fills.
    let r = unsafe { libc::proc_pid_rusage(pid, libc::RUSAGE_INFO_V4, out) };
    (r == 0).then_some((info.ri_resident_size, info.ri_phys_footprint))
}

/// Every process id on the system.
fn all_pids() -> Vec<libc::pid_t> {
    // SAFETY: a null buffer asks for the number of processes.
    let n = unsafe { libc::proc_listallpids(std::ptr::null_mut(), 0) };
    if n <= 0 {
        return Vec::new();
    }
    let mut pids = vec![0 as libc::pid_t; n as usize + 64];
    let bytes = (pids.len() * std::mem::size_of::<libc::pid_t>()) as c_int;
    // SAFETY: `pids` has room for `bytes` bytes of pids.
    let got = unsafe { libc::proc_listallpids(pids.as_mut_ptr().cast(), bytes) };
    pids.truncate(got.max(0) as usize);
    pids
}

/// The executable path of `pid`.
fn exe_path(pid: libc::pid_t) -> Option<String> {
    let mut buf = vec![0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
    // SAFETY: `buf` has room for the size given.
    let n = unsafe { libc::proc_pidpath(pid, buf.as_mut_ptr().cast(), buf.len() as u32) };
    (n > 0).then(|| String::from_utf8_lossy(&buf[..n as usize]).into_owned())
}

/// True for a WebKit helper process (web content, networking, GPU).
fn is_web_engine(path: &str) -> bool {
    path.rsplit('/').next().is_some_and(|name| name.starts_with("com.apple.WebKit."))
}

type ResponsibleFn = unsafe extern "C" fn(libc::pid_t) -> libc::pid_t;

/// The system's "responsible process" lookup, when this macOS has it.
fn responsible_lookup() -> Option<ResponsibleFn> {
    const NAME: &CStr = c"responsibility_get_pid_responsible_for_pid";
    // SAFETY: dlsym with RTLD_DEFAULT only looks a name up; it returns null when absent.
    let sym = unsafe { libc::dlsym(RTLD_DEFAULT, NAME.as_ptr()) };
    if sym.is_null() {
        return None;
    }
    // SAFETY: the system function has the C signature `pid_t f(pid_t)`.
    Some(unsafe { std::mem::transmute::<*mut c_void, ResponsibleFn>(sym) })
}

/// The WebKit processes working for Sessionmux.
fn web_engine_pids() -> Vec<libc::pid_t> {
    let Some(responsible) = responsible_lookup() else { return Vec::new() };
    let me = std::process::id() as libc::pid_t;
    all_pids()
        .into_iter()
        .filter(|&p| p > 0 && p != me)
        // SAFETY: the lookup takes any pid and returns -1 or a pid.
        .filter(|&p| unsafe { responsible(p) } == me)
        .filter(|&p| exe_path(p).is_some_and(|path| is_web_engine(&path)))
        .collect()
}

impl Capture for Os {
    /// A macOS process started from a terminal already writes to it.
    fn attach_console() {}

    /// Run UI events, posted work and timers for `ms` milliseconds.
    fn pump(ms: u64) {
        event_loop::pump(ms);
    }

    fn panel_png(w: WindowId, path: &Path) -> Result<(u32, u32), String> {
        panel_capture(w, path)
    }

    fn footprint(helpers: bool) -> Footprint {
        let (mut resident, mut private) = usage(std::process::id() as libc::pid_t).unwrap_or((0, 0));
        if !helpers {
            return Footprint { resident, private, helpers: 0 };
        }
        let engines = web_engine_pids();
        for pid in &engines {
            let (r, p) = usage(*pid).unwrap_or((0, 0));
            resident += r;
            private += p;
        }
        Footprint { resident, private, helpers: engines.len() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn footprint_of_this_process() {
        let own = <Os as Capture>::footprint(false);
        assert!(own.resident > 0 && own.private > 0);
        assert_eq!(own.helpers, 0);
        let all = <Os as Capture>::footprint(true);
        assert!(all.resident >= own.resident / 2);
        assert!(all_pids().contains(&(std::process::id() as libc::pid_t)));
        assert!(exe_path(std::process::id() as libc::pid_t).is_some());
    }

    #[test]
    fn web_engine_processes_by_name() {
        let base = "/System/Library/Frameworks/WebKit.framework/Versions/A/XPCServices";
        assert!(is_web_engine(&format!(
            "{base}/com.apple.WebKit.WebContent.xpc/Contents/MacOS/com.apple.WebKit.WebContent"
        )));
        assert!(is_web_engine("/x/com.apple.WebKit.Networking"));
        assert!(is_web_engine("/x/com.apple.WebKit.GPU"));
        assert!(!is_web_engine("/Applications/Sessionmux.app/Contents/MacOS/Sessionmux"));
        assert!(!is_web_engine("/x/com.apple.WebKit.GPU/other"));
    }

    #[test]
    fn colours_are_unpremultiplied() {
        let mut px = [0x10, 0x11, 0x14, 0xFF, 0, 0, 0, 0, 0x40, 0x20, 0x00, 0x80];
        unpremultiply(&mut px);
        assert_eq!(px, [0x10, 0x11, 0x14, 0xFF, 0, 0, 0, 0, 0x80, 0x40, 0x00, 0x80]);
    }

    #[test]
    fn captures_need_the_main_thread() {
        // Tests normally run on worker threads; on the main thread there is nothing to check.
        if MainThreadMarker::new().is_some() {
            return;
        }
        Os::attach_console();
        Os::pump(1);
        let e = Os::panel_png(WindowId(1), Path::new("x.png")).unwrap_err();
        assert!(e.contains("main thread"));
        begin_self_check();
    }
}

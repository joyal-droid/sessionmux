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

//! Other apps' windows, read from the window server's list of on-screen windows. Only the owner,
//! layer, size and opacity of each window are read, never its title, so no permission is needed.
//! The app in front is read on the main thread; other threads ask it there and wait briefly.

use std::collections::{HashMap, HashSet};
use std::sync::mpsc;
use std::time::Duration;

use objc2::MainThreadMarker;
use objc2_app_kit::{NSRunningApplication, NSWorkspace};
use objc2_core_foundation::{CFArray, CFDictionary, CFNumber, CFRetained, CFString, CFType, CGRect};
use objc2_core_graphics::{
    kCGNullWindowID, kCGWindowAlpha, kCGWindowBounds, kCGWindowLayer, kCGWindowNumber, kCGWindowOwnerPID,
    CGRectMakeWithDictionaryRepresentation, CGWindowListCopyWindowInfo, CGWindowListOption,
};

use crate::platform::{EventLoop, Os, Pid, WindowId, Windows};

/// Windows smaller than this (points, either side) are not an app's main window.
const MIN_SIDE: f64 = 100.0;
/// How long another thread waits for the main thread to read the app in front.
const FRONT_WAIT: Duration = Duration::from_millis(200);

/// One on-screen window as the window server reports it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct WinInfo {
    pub(crate) id: u32,
    pub(crate) pid: Pid,
    pub(crate) layer: i64,
    pub(crate) alpha: f64,
    pub(crate) w: f64,
    pub(crate) h: f64,
}

impl WinInfo {
    /// A normal app window that can be seen: the document layer, not transparent, not tiny.
    fn is_main_candidate(&self) -> bool {
        self.layer == 0 && self.alpha > 0.0 && self.w >= MIN_SIDE && self.h >= MIN_SIDE
    }
}

/// The largest visible normal window of each pid in `pids`.
pub(crate) fn pick_main_windows(list: &[WinInfo], pids: &HashSet<Pid>) -> HashMap<Pid, WindowId> {
    let mut best: HashMap<Pid, (f64, u32)> = HashMap::new();
    for w in list.iter().filter(|w| w.is_main_candidate() && pids.contains(&w.pid)) {
        let area = w.w * w.h;
        let e = best.entry(w.pid).or_insert((area, w.id));
        if area > e.0 {
            *e = (area, w.id);
        }
    }
    best.into_iter().map(|(pid, (_, id))| (pid, WindowId(id as usize))).collect()
}

/// The owner of the frontmost visible normal window (the list runs front to back).
pub(crate) fn pick_front_owner(list: &[WinInfo]) -> Option<Pid> {
    list.iter().find(|w| w.is_main_candidate()).map(|w| w.pid)
}

fn number(d: &CFDictionary<CFString, CFType>, key: &CFString) -> Option<CFRetained<CFNumber>> {
    d.get(key)?.downcast::<CFNumber>().ok()
}

/// On-screen windows, front to back, desktop elements left out.
pub(crate) fn on_screen() -> Vec<WinInfo> {
    window_list(CGWindowListOption::OptionOnScreenOnly | CGWindowListOption::ExcludeDesktopElements)
}

/// True when `pid` has a normal window that is not on screen now: minimised, on another Space or
/// ordered out.
pub(crate) fn has_off_screen_window(pid: Pid) -> bool {
    window_list(CGWindowListOption::OptionAll | CGWindowListOption::ExcludeDesktopElements)
        .iter()
        .any(|w| w.pid == pid && w.layer == 0 && w.w >= MIN_SIDE && w.h >= MIN_SIDE)
}

/// True when the app `pid` is hidden (its windows are then off screen until it is shown again).
pub(crate) fn app_hidden(pid: Pid) -> bool {
    let Ok(p) = libc::pid_t::try_from(pid) else { return false };
    if p <= 0 {
        return false;
    }
    on_main_wait(FRONT_WAIT, move |_| {
        NSRunningApplication::runningApplicationWithProcessIdentifier(p).is_some_and(|a| a.isHidden())
    })
    .unwrap_or(false)
}

/// The windows the window server lists for `opts`.
fn window_list(opts: CGWindowListOption) -> Vec<WinInfo> {
    let Some(list) = CGWindowListCopyWindowInfo(opts, kCGNullWindowID) else { return Vec::new() };
    // SAFETY: the window server documents this list as an array of dictionaries with string keys;
    // values are read with checked downcasts only.
    let list: CFRetained<CFArray<CFDictionary<CFString, CFType>>> = unsafe { CFRetained::cast_unchecked(list) };
    let mut out = Vec::with_capacity(list.len());
    for d in list.iter() {
        // SAFETY: the keys are constant strings exported by CoreGraphics, valid for the program's
        // lifetime.
        let (k_id, k_pid, k_layer, k_alpha, k_bounds) =
            unsafe { (kCGWindowNumber, kCGWindowOwnerPID, kCGWindowLayer, kCGWindowAlpha, kCGWindowBounds) };
        let (Some(id), Some(pid)) = (
            number(&d, k_id).and_then(|n| n.as_i64()).and_then(|n| u32::try_from(n).ok()),
            number(&d, k_pid).and_then(|n| n.as_i64()).and_then(|n| Pid::try_from(n).ok()),
        ) else {
            continue;
        };
        let layer = number(&d, k_layer).and_then(|n| n.as_i64()).unwrap_or(i64::MAX);
        let alpha = number(&d, k_alpha).and_then(|n| n.as_f64()).unwrap_or(0.0);
        let mut rect = CGRect::default();
        let bounds = d.get(k_bounds).and_then(|b| b.downcast::<CFDictionary>().ok());
        // SAFETY: `rect` is a live, writable rectangle; the dictionary is the window's bounds.
        let has_bounds = unsafe { CGRectMakeWithDictionaryRepresentation(bounds.as_deref(), &mut rect) };
        if !has_bounds {
            continue;
        }
        out.push(WinInfo { id, pid, layer, alpha, w: rect.size.width, h: rect.size.height });
    }
    out
}

/// The main window of each of `pids` that has one on screen, in one read of the window list.
pub(crate) fn main_windows(pids: &HashSet<Pid>) -> HashMap<Pid, WindowId> {
    if pids.is_empty() {
        return HashMap::new();
    }
    pick_main_windows(&on_screen(), pids)
}

/// The owner of the frontmost normal window, read from the window list (any thread, always
/// current).
pub(crate) fn front_window_owner() -> Option<Pid> {
    pick_front_owner(&on_screen())
}

/// Run `f` on the main thread and wait up to `wait` for its result: directly when called there,
/// else posted to it. None when the main thread does not answer in time or no event loop runs.
pub(crate) fn on_main_wait<R: Send + 'static>(
    wait: Duration,
    f: impl FnOnce(MainThreadMarker) -> R + Send + 'static,
) -> Option<R> {
    if let Some(mtm) = MainThreadMarker::new() {
        return Some(f(mtm));
    }
    let (tx, rx) = mpsc::sync_channel(1);
    <Os as EventLoop>::post(Box::new(move || {
        if let Some(mtm) = MainThreadMarker::new() {
            let _ = tx.send(f(mtm));
        }
    }));
    rx.recv_timeout(wait).ok()
}

impl Windows for Os {
    /// The largest visible normal window of `pid`.
    fn main_window(pid: Pid) -> Option<WindowId> {
        main_windows(&HashSet::from([pid])).remove(&pid)
    }

    /// The app the system has in front, as AppKit reports it on the main thread.
    fn foreground_pid() -> Option<Pid> {
        on_main_wait(FRONT_WAIT, |_| {
            let app = NSWorkspace::sharedWorkspace().frontmostApplication()?;
            Pid::try_from(app.processIdentifier()).ok().filter(|&p| p != 0)
        })
        .flatten()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(id: u32, pid: Pid, layer: i64, alpha: f64, wd: f64, h: f64) -> WinInfo {
        WinInfo { id, pid, layer, alpha, w: wd, h }
    }

    #[test]
    fn largest_visible_normal_window_wins() {
        let list = [
            w(1, 10, 25, 1.0, 800.0, 600.0), // a floating panel
            w(2, 10, 0, 1.0, 400.0, 300.0),
            w(3, 10, 0, 1.0, 900.0, 700.0),
            w(4, 10, 0, 0.0, 2000.0, 2000.0), // transparent
            w(5, 11, 0, 1.0, 99.0, 500.0),    // too narrow
            w(6, 12, 0, 1.0, 300.0, 300.0),
        ];
        let got = pick_main_windows(&list, &HashSet::from([10, 11, 13]));
        assert_eq!(got.get(&10), Some(&WindowId(3)));
        assert_eq!(got.get(&11), None);
        assert_eq!(got.get(&12), None, "only the asked pids are reported");
        assert_eq!(got.len(), 1);
    }

    #[test]
    fn front_owner_is_the_first_normal_window() {
        let list = [w(1, 7, 25, 1.0, 500.0, 500.0), w(2, 8, 0, 0.0, 500.0, 500.0), w(3, 9, 0, 1.0, 500.0, 500.0)];
        assert_eq!(pick_front_owner(&list), Some(9));
        assert_eq!(pick_front_owner(&[]), None);
    }

    #[test]
    fn reading_the_window_list_is_safe() {
        // A runner may have no window server session; the read must still work.
        let list = on_screen();
        for x in &list {
            assert!(x.id != 0);
        }
        assert_eq!(Os::main_window(u32::MAX), None);
        assert!(!app_hidden(0));
        assert!(!app_hidden(u32::MAX));
        assert_eq!(main_windows(&HashSet::new()).len(), 0);
    }

    #[test]
    fn front_app_needs_a_running_main_thread() {
        // Tests run without an event loop: the answer is "unknown", quickly, never a hang.
        let t = std::time::Instant::now();
        let _ = Os::foreground_pid();
        assert!(t.elapsed() < Duration::from_secs(2));
    }
}

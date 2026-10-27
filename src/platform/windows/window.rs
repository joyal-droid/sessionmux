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

//! Other programs' top-level windows: the main window of a process and the foreground process.

use std::collections::{HashMap, HashSet};
use std::ffi::c_void;

use windows::Win32::Foundation::{BOOL, HWND, LPARAM, RECT};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetForegroundWindow, GetWindow, GetWindowLongPtrW, GetWindowRect, GetWindowThreadProcessId,
    IsWindowVisible, GWL_EXSTYLE, GW_OWNER, WS_EX_TOOLWINDOW,
};

use crate::platform::{Os, Pid, WindowId, Windows};

/// pid → first visible, unowned top-level window (z-order), for the given pids (all if empty).
pub(super) fn main_windows(pids: &HashSet<u32>) -> HashMap<u32, isize> {
    struct Ctx<'a> {
        pids: &'a HashSet<u32>,
        found: HashMap<u32, isize>,
    }
    unsafe extern "system" fn cb(hwnd: HWND, lp: LPARAM) -> BOOL {
        // SAFETY: lp is the &mut Ctx passed to EnumWindows below, alive for the whole enumeration
        // and only accessed from this (synchronous) callback.
        let ctx = &mut *(lp.0 as *mut Ctx);
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid != 0 && (ctx.pids.is_empty() || ctx.pids.contains(&pid)) && !ctx.found.contains_key(&pid) {
            let owned = matches!(GetWindow(hwnd, GW_OWNER), Ok(o) if !o.0.is_null());
            let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;
            let mut r = RECT::default();
            let sized = GetWindowRect(hwnd, &mut r).is_ok() && r.right > r.left && r.bottom > r.top;
            if !owned && IsWindowVisible(hwnd).as_bool() && ex & WS_EX_TOOLWINDOW.0 == 0 && sized {
                ctx.found.insert(pid, hwnd.0 as isize);
            }
        }
        BOOL(1)
    }
    let mut ctx = Ctx { pids, found: HashMap::new() };
    // SAFETY: the callback only dereferences the LPARAM as the Ctx that outlives this call.
    let _ = unsafe { EnumWindows(Some(cb), LPARAM(&mut ctx as *mut Ctx as isize)) };
    ctx.found
}

pub(super) fn main_window(pid: u32) -> isize {
    let set: HashSet<u32> = [pid].into_iter().collect();
    main_windows(&set).get(&pid).copied().unwrap_or(0)
}

pub(super) fn hwnd_of(v: isize) -> HWND {
    HWND(v as *mut c_void)
}

impl Windows for Os {
    fn main_window(pid: Pid) -> Option<WindowId> {
        match main_window(pid) {
            0 => None,
            h => Some(WindowId(h as usize)),
        }
    }

    fn foreground_pid() -> Option<Pid> {
        foreground_pid()
    }
}

/// PID owning the current foreground window.
fn foreground_pid() -> Option<u32> {
    // SAFETY: GetForegroundWindow has no preconditions.
    let fg = unsafe { GetForegroundWindow() };
    if fg.0.is_null() {
        return None;
    }
    let mut pid = 0u32;
    // SAFETY: fg may be stale; the API tolerates invalid handles (returns 0).
    unsafe { GetWindowThreadProcessId(fg, Some(&mut pid)) };
    if pid == 0 {
        None
    } else {
        Some(pid)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_window_for_a_missing_process() {
        assert_eq!(Os::main_window(0), None);
        assert_eq!(hwnd_of(0), HWND(std::ptr::null_mut()));
    }
}

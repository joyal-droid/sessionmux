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

//! Single instance through the named mutex `brand::MUTEX` (`Local\Sessionmux.SingleInstance`).
//! A second launch finds the first instance's message window and posts
//! `WM_SESSIONMUX_SHOW_PANEL` to it. Message-only windows are invisible to `FindWindowW`, so the
//! window is looked up with `FindWindowExW` under `HWND_MESSAGE`.

use super::event_loop::WM_SESSIONMUX_SHOW_PANEL;
use super::win;
use crate::brand;
use crate::platform::SingleInstance;
use crate::util::log;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{CloseHandle, GetLastError, ERROR_ALREADY_EXISTS, HANDLE, HWND, LPARAM, WPARAM};
use windows::Win32::System::Threading::{CreateMutexW, OpenMutexW, ReleaseMutex, SYNCHRONIZATION_SYNCHRONIZE};
use windows::Win32::UI::WindowsAndMessaging::{
    AllowSetForegroundWindow, FindWindowExW, GetWindowThreadProcessId, PostMessageW, HWND_MESSAGE,
};

/// The mutex backend.
pub struct Os;

/// The owned single-instance mutex, released and closed on drop. Empty when the mutex could not
/// be created (the app then runs unguarded).
pub struct Guard(Option<HANDLE>);

impl Drop for Guard {
    fn drop(&mut self) {
        if let Some(h) = self.0.take() {
            // SAFETY: `h` is the mutex this thread created and owns; it is released, then closed
            // exactly once.
            unsafe {
                let _ = ReleaseMutex(h);
                let _ = CloseHandle(h);
            }
        }
    }
}

/// Find another instance's message window.
pub fn find_msg_window() -> Option<HWND> {
    let class = win::wide(brand::CLASS_MSG);
    // SAFETY: `class` outlives the call; FindWindowExW only reads it.
    let found = unsafe { FindWindowExW(HWND_MESSAGE, None, PCWSTR(class.as_ptr()), PCWSTR::null()) };
    found.ok().filter(|h| !h.is_invalid())
}

impl SingleInstance for Os {
    type Guard = Guard;

    fn acquire() -> Option<Guard> {
        let name = win::wide(brand::MUTEX);
        // SAFETY: creates (or opens) a named mutex; `name` is NUL-terminated and outlives the call.
        let mutex = unsafe { CreateMutexW(None, true, PCWSTR(name.as_ptr())) };
        // SAFETY: GetLastError reads this thread's last-error value set by CreateMutexW.
        let already = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
        let mutex = match mutex {
            Ok(h) => Some(h),
            Err(e) => {
                log::warn(&format!("single-instance mutex: {e}"));
                None
            }
        };
        if already {
            if let Some(h) = mutex {
                // SAFETY: `h` was returned by CreateMutexW above; another instance owns the
                // mutex, so it is only closed, exactly once.
                let _ = unsafe { CloseHandle(h) };
            }
            return None;
        }
        Some(Guard(mutex))
    }

    fn notify_existing() -> bool {
        let Some(h) = find_msg_window() else { return false };
        // We were just started by the user, so we may hand our foreground right to the first
        // instance; without it the panel would show inactive and never blur-hide.
        let mut pid = 0u32;
        // SAFETY: h is a window handle (possibly stale; the API then returns 0); pid is a
        // valid out-pointer. AllowSetForegroundWindow has no memory preconditions.
        unsafe {
            GetWindowThreadProcessId(h, Some(&mut pid));
            if pid != 0 {
                let _ = AllowSetForegroundWindow(pid);
            }
        }
        // SAFETY: posting a private message to another process's window; integers only.
        unsafe { PostMessageW(h, WM_SESSIONMUX_SHOW_PANEL, WPARAM(0), LPARAM(0)) }.is_ok()
    }

    fn running() -> bool {
        let name = win::wide(brand::MUTEX);
        // SAFETY: opens (never creates) a named mutex; `name` is NUL-terminated and outlives the call.
        match unsafe { OpenMutexW(SYNCHRONIZATION_SYNCHRONIZE, false, PCWSTR(name.as_ptr())) } {
            Ok(h) => {
                // SAFETY: `h` was just returned by OpenMutexW and is closed exactly once.
                let _ = unsafe { CloseHandle(h) };
                true
            }
            Err(_) => false,
        }
    }
}

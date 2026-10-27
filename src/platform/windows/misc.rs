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

//! Small Windows facts: the local UTC offset, symlinks, sharing violations, the working-set
//! trim, exe path comparison, the UI thread's COM / DPI / taskbar identity set-up and the
//! debugger check.

use std::io;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use super::win;
use crate::brand;
use crate::platform::{Debugger, Misc, Os};
use crate::util::log;

/// COM was initialised by `init_ui_thread` and still needs its CoUninitialize.
static COM_STARTED: AtomicBool = AtomicBool::new(false);

impl Misc for Os {
    /// FileTimeToLocalFileTime of `now`; 0 when the conversion fails.
    fn local_offset_secs(now: u64) -> i64 {
        use windows::Win32::Foundation::FILETIME;
        use windows::Win32::Storage::FileSystem::FileTimeToLocalFileTime;
        const EPOCH_DIFF_SECS: u64 = 11_644_473_600;
        let ticks = (now + EPOCH_DIFF_SECS) * 10_000_000;
        let utc = FILETIME { dwLowDateTime: ticks as u32, dwHighDateTime: (ticks >> 32) as u32 };
        let mut local = FILETIME::default();
        // SAFETY: both pointers refer to live FILETIME values for the duration of the call.
        if unsafe { FileTimeToLocalFileTime(&utc, &mut local) }.is_err() {
            return 0;
        }
        let local_ticks = (u64::from(local.dwHighDateTime) << 32) | u64::from(local.dwLowDateTime);
        (local_ticks as i64 - ticks as i64) / 10_000_000
    }

    /// A directory or file symlink (needs Developer Mode or the symlink privilege).
    fn symlink(target: &Path, link: &Path, is_dir: bool) -> io::Result<()> {
        if is_dir {
            std::os::windows::fs::symlink_dir(target, link)
        } else {
            std::os::windows::fs::symlink_file(target, link)
        }
    }

    /// ERROR_SHARING_VIOLATION / ERROR_LOCK_VIOLATION.
    fn is_sharing_violation(e: &io::Error) -> bool {
        matches!(e.raw_os_error(), Some(32) | Some(33))
    }

    /// EmptyWorkingSet: the pages come back on demand.
    fn trim_memory() {
        use windows::Win32::System::ProcessStatus::EmptyWorkingSet;
        use windows::Win32::System::Threading::GetCurrentProcess;
        // SAFETY: GetCurrentProcess returns a pseudo-handle valid for this call; no memory is freed.
        if let Err(e) = unsafe { EmptyWorkingSet(GetCurrentProcess()) } {
            log::debug(&format!("EmptyWorkingSet: {e}"));
        }
    }

    /// Case-insensitive, either separator, quotes ignored.
    fn same_exe(a: &str, b: &str) -> bool {
        let norm = |s: &str| s.trim().trim_matches('"').replace('/', "\\").to_lowercase();
        let (a, b) = (norm(a), norm(b));
        !a.is_empty() && a == b
    }

    /// Windows never moves a running copy aside.
    fn translocated() -> bool {
        false
    }

    /// COM in an STA, per-monitor DPI awareness and the taskbar identity `brand::AUMID`.
    fn init_ui_thread() -> bool {
        use windows::core::PCWSTR;
        use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};
        use windows::Win32::UI::HiDpi::{SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2};
        use windows::Win32::UI::Shell::SetCurrentProcessExplicitAppUserModelID;
        // SAFETY: called once on the UI thread before any COM use; balanced by uninit_ui_thread.
        let com = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
        // SAFETY: no preconditions; fails harmlessly when the manifest already set the awareness.
        let _ = unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
        let aumid = win::wide(brand::AUMID);
        // SAFETY: `aumid` is a NUL-terminated UTF-16 buffer that outlives the call; failure is harmless.
        let _ = unsafe { SetCurrentProcessExplicitAppUserModelID(PCWSTR(aumid.as_ptr())) };
        COM_STARTED.store(com.is_ok(), Ordering::Release);
        com.is_ok()
    }

    fn uninit_ui_thread() {
        use windows::Win32::System::Com::CoUninitialize;
        if COM_STARTED.swap(false, Ordering::AcqRel) {
            // SAFETY: balances the successful CoInitializeEx of init_ui_thread on the same thread.
            unsafe { CoUninitialize() };
        }
    }
}

impl Debugger for Os {
    /// A local or a remote debugger.
    fn present() -> bool {
        use windows::Win32::Foundation::BOOL;
        use windows::Win32::System::Diagnostics::Debug::{CheckRemoteDebuggerPresent, IsDebuggerPresent};
        use windows::Win32::System::Threading::GetCurrentProcess;
        // SAFETY: both calls only read process state; `remote` is a valid out-pointer and the
        // pseudo-handle from GetCurrentProcess needs no closing.
        unsafe {
            if IsDebuggerPresent().as_bool() {
                return true;
            }
            let mut remote = BOOL(0);
            CheckRemoteDebuggerPresent(GetCurrentProcess(), &mut remote).is_ok() && remote.as_bool()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_exe_rules() {
        assert!(Os::same_exe(
            r#""C:\Program Files\Sessionmux\Sessionmux.exe""#,
            "c:/program files/sessionmux/SESSIONMUX.EXE"
        ));
        assert!(!Os::same_exe(r"C:\a\Sessionmux.exe", r"C:\b\Sessionmux.exe"));
        assert!(!Os::same_exe("", ""));
    }

    #[test]
    fn sharing_violation_codes() {
        assert!(Os::is_sharing_violation(&io::Error::from_raw_os_error(32)));
        assert!(Os::is_sharing_violation(&io::Error::from_raw_os_error(33)));
        assert!(!Os::is_sharing_violation(&io::Error::from_raw_os_error(5)));
        assert!(!Os::is_sharing_violation(&io::Error::other("x")));
    }

    #[test]
    fn local_offset_is_sane() {
        let o = Os::local_offset_secs(1_790_000_000);
        assert!((-14 * 3600..=14 * 3600).contains(&o));
    }

    #[test]
    fn a_windows_copy_is_never_moved_aside() {
        assert!(!Os::translocated());
    }

    #[test]
    fn debugger_check_runs() {
        // Only checks that the call works; a debugger may be attached while developing.
        let _ = Os::present();
    }
}

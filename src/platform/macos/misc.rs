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

//! Small system facts: the local time offset, symbolic links, file-sharing errors, memory
//! trimming, executable comparison, where this copy runs from, the UI thread's preparation and
//! the debugger check.

use std::io;
use std::path::Path;
use std::sync::OnceLock;

use objc2::MainThreadMarker;
use objc2_foundation::NSBundle;

use crate::platform::{Debugger, Misc, Os, Paths};
use crate::util::log;

extern "C" {
    /// Hands free memory of a malloc zone (all zones for null) back to the system; returns the
    /// number of bytes released.
    fn malloc_zone_pressure_relief(zone: *mut libc::c_void, goal: libc::size_t) -> libc::size_t;
}

/// `P_TRACED` in `extern_proc.p_flag`: a debugger is attached.
const P_TRACED: i32 = 0x0000_0800;
/// Offset of `p_flag` in `struct kinfo_proc` (its first member `kp_proc` starts with a 16-byte
/// union and two pointers) on 64-bit macOS.
const P_FLAG_OFFSET: usize = 32;
/// `sizeof(struct kinfo_proc)` on 64-bit macOS.
const KINFO_PROC_SIZE: usize = 648;

/// True when a `kinfo_proc` record's process flags say a debugger is attached.
fn traced(kinfo: &[u8]) -> bool {
    let Some(b) = kinfo.get(P_FLAG_OFFSET..P_FLAG_OFFSET + 4) else { return false };
    let flag = i32::from_ne_bytes([b[0], b[1], b[2], b[3]]);
    flag & P_TRACED != 0
}

/// True for a bundle that runs from a place it cannot stay in: a copy the system moved aside
/// before its first open (App Translocation) or one opened straight from a mounted disk image.
/// A program outside a bundle (a development build) never counts.
fn runs_from_temporary_place(bundle_path: &str) -> bool {
    let p = bundle_path.trim_end_matches('/');
    let is_bundle = p.len() > 4 && p[p.len() - 4..].eq_ignore_ascii_case(".app");
    is_bundle && (p.contains("/AppTranslocation/") || p.starts_with("/Volumes/"))
}

impl Misc for Os {
    /// `tm_gmtoff` of `now` in local time; 0 when the conversion fails.
    fn local_offset_secs(now: u64) -> i64 {
        let Ok(t) = libc::time_t::try_from(now) else { return 0 };
        // SAFETY: `tm` is plain old data; an all-zero value is valid and fully overwritten.
        let mut tm: libc::tm = unsafe { std::mem::zeroed() };
        // SAFETY: both pointers refer to live values of this frame for the duration of the call.
        if unsafe { libc::localtime_r(&t, &mut tm) }.is_null() {
            return 0;
        }
        tm.tm_gmtoff
    }

    /// One kind of symbolic link serves files and folders alike.
    fn symlink(target: &Path, link: &Path, _is_dir: bool) -> io::Result<()> {
        std::os::unix::fs::symlink(target, link)
    }

    /// macOS never refuses a file because another process holds it open.
    fn is_sharing_violation(_e: &io::Error) -> bool {
        false
    }

    /// Return the allocator's free pages to the system (macOS has no working-set call).
    fn trim_memory() {
        // SAFETY: a null zone means every zone; the call only releases memory no allocation uses.
        let released = unsafe { malloc_zone_pressure_relief(std::ptr::null_mut(), 0) };
        log::debug(&format!("memory: released {} KB", released / 1024));
    }

    /// The same path after normalisation (case kept), or the same file.
    fn same_exe(a: &str, b: &str) -> bool {
        let (a, b) = (a.trim(), b.trim());
        Os::same_path(a, b)
    }

    /// The app bundle runs translocated or from a disk image (read once).
    fn translocated() -> bool {
        static AT: OnceLock<bool> = OnceLock::new();
        *AT.get_or_init(|| {
            let path = NSBundle::mainBundle().bundlePath().to_string();
            let t = runs_from_temporary_place(&path);
            if t {
                log::warn("this copy runs from a disk image or a temporary location; recording its path is off");
            }
            t
        })
    }

    /// Create the shared application as an accessory app (no Dock icon) before any window
    /// exists. Nothing to undo afterwards.
    fn init_ui_thread() -> bool {
        match MainThreadMarker::new() {
            Some(mtm) => super::event_loop::prepare(mtm),
            None => log::warn("the UI is not starting on the main thread"),
        }
        false
    }

    fn uninit_ui_thread() {}
}

impl Debugger for Os {
    /// The process flags of this process (`sysctl` `KERN_PROC_PID`) carry `P_TRACED`.
    fn present() -> bool {
        // SAFETY: getpid has no preconditions.
        let pid = unsafe { libc::getpid() };
        let mut mib = [libc::CTL_KERN, libc::KERN_PROC, libc::KERN_PROC_PID, pid];
        // Eight-byte aligned, as the record holds pointers.
        let mut buf = [0u64; KINFO_PROC_SIZE / 8];
        let mut len: libc::size_t = KINFO_PROC_SIZE;
        // SAFETY: `mib` names four valid levels, `buf` is writable for `len` bytes and `len` is
        // updated with the size written; no new value is set.
        let rc = unsafe {
            libc::sysctl(
                mib.as_mut_ptr(),
                mib.len() as libc::c_uint,
                buf.as_mut_ptr().cast(),
                &mut len,
                std::ptr::null_mut(),
                0,
            )
        };
        if rc != 0 {
            return false;
        }
        // SAFETY: `buf` is `KINFO_PROC_SIZE` initialised bytes and `len` is at most that.
        let bytes = unsafe { std::slice::from_raw_parts(buf.as_ptr().cast::<u8>(), len.min(KINFO_PROC_SIZE)) };
        traced(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_exe_rules() {
        let exe = "/Applications/Sessionmux.app/Contents/MacOS/Sessionmux";
        assert!(Os::same_exe(exe, &format!("\"{exe}\"")));
        assert!(!Os::same_exe(exe, &exe.to_lowercase()));
        assert!(!Os::same_exe("", ""));
    }

    #[test]
    fn nothing_is_a_sharing_violation() {
        assert!(!Os::is_sharing_violation(&io::Error::from_raw_os_error(libc::EBUSY)));
        assert!(!Os::is_sharing_violation(&io::Error::other("x")));
    }

    #[test]
    fn local_offset_is_sane() {
        let o = Os::local_offset_secs(1_790_000_000);
        assert!((-14 * 3600..=14 * 3600).contains(&o));
    }

    #[test]
    fn symlinks_work_for_files_and_folders() {
        let d = tempfile::tempdir().unwrap();
        let target = d.path().join("t");
        std::fs::create_dir(&target).unwrap();
        Os::symlink(&target, &d.path().join("l"), true).unwrap();
        assert!(d.path().join("l").is_dir());
    }

    #[test]
    fn traced_flag_is_read_from_the_record() {
        let mut rec = vec![0u8; KINFO_PROC_SIZE];
        assert!(!traced(&rec));
        rec[P_FLAG_OFFSET..P_FLAG_OFFSET + 4].copy_from_slice(&(P_TRACED | 0x4).to_ne_bytes());
        assert!(traced(&rec));
        rec[P_FLAG_OFFSET..P_FLAG_OFFSET + 4].copy_from_slice(&0x4i32.to_ne_bytes());
        assert!(!traced(&rec));
        assert!(!traced(&rec[..20]));
        assert!(!traced(&[]));
    }

    #[test]
    fn no_debugger_under_the_test_runner() {
        // `cargo test` does not trace its test binaries.
        assert!(!Os::present());
    }

    #[test]
    fn temporary_places() {
        assert!(runs_from_temporary_place("/private/var/folders/xy/T/AppTranslocation/1A2B-3C/d/Sessionmux.app"));
        assert!(runs_from_temporary_place("/Volumes/Sessionmux/Sessionmux.app"));
        assert!(runs_from_temporary_place("/Volumes/Sessionmux/Sessionmux.APP/"));
        assert!(!runs_from_temporary_place("/Applications/Sessionmux.app"));
        assert!(!runs_from_temporary_place("/Users/me/Applications/Sessionmux.app"));
        // Development builds are not bundles.
        assert!(!runs_from_temporary_place("/Volumes/Work/sessionmux/target/release"));
        assert!(!runs_from_temporary_place(""));
    }

    #[test]
    fn the_rest_is_safe_to_call() {
        Os::trim_memory();
        // The test binary is not inside a bundle.
        assert!(!Os::translocated());
        // Nothing is left to undo, whichever thread prepares the UI.
        if objc2::MainThreadMarker::new().is_none() {
            assert!(!Os::init_ui_thread());
            Os::uninit_ui_thread();
        }
    }
}

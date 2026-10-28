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
//! trimming, executable comparison and the debugger check.

use std::io;
use std::path::Path;

use crate::platform::{Debugger, Misc, Os, Paths};

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

    /// Not done yet: memory is left to the system.
    fn trim_memory() {}

    /// The same path after normalisation (case kept).
    fn same_exe(a: &str, b: &str) -> bool {
        let (a, b) = (a.trim(), b.trim());
        Os::same_path(a, b)
    }

    /// Not checked yet: the copy is treated as installed where it runs.
    fn translocated() -> bool {
        false
    }

    /// Nothing to prepare yet.
    fn init_ui_thread() -> bool {
        false
    }

    fn uninit_ui_thread() {}
}

impl Debugger for Os {
    /// Not checked yet: reports no debugger.
    fn present() -> bool {
        false
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
    fn the_rest_is_inert() {
        Os::trim_memory();
        assert!(!Os::translocated());
        assert!(!Os::init_ui_thread());
        Os::uninit_ui_thread();
        assert!(!Os::present());
    }
}

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

//! Folder roots and path comparison. Settings, data and Claude's profile folders all live in
//! `~/Library/Application Support`; paths compare case-sensitively, as written.

use std::ffi::{CStr, OsStr};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use crate::brand;
use crate::platform::{Os, Paths};

/// The home folder from the user database, for when `$HOME` is unset or not absolute.
fn home_from_passwd() -> Option<PathBuf> {
    // SAFETY: `passwd` is plain old data; an all-zero value is a valid "empty" record.
    let mut pwd: libc::passwd = unsafe { std::mem::zeroed() };
    let mut buf: Vec<libc::c_char> = vec![0; 4096];
    let mut out: *mut libc::passwd = std::ptr::null_mut();
    // SAFETY: every pointer refers to live memory owned by this frame and `buf.len()` is its real
    // size; on success `out` points at `pwd`, whose strings point into `buf`.
    let rc = unsafe { libc::getpwuid_r(libc::geteuid(), &mut pwd, buf.as_mut_ptr(), buf.len(), &mut out) };
    if rc != 0 || out.is_null() || pwd.pw_dir.is_null() {
        return None;
    }
    // SAFETY: `pw_dir` is a NUL-terminated string inside `buf`, which is still alive.
    let dir = unsafe { CStr::from_ptr(pwd.pw_dir) };
    let p = PathBuf::from(OsStr::from_bytes(dir.to_bytes()));
    p.is_absolute().then_some(p)
}

impl Paths for Os {
    /// `$HOME` when it is absolute, else the home folder of the user database, else `/`.
    fn home() -> PathBuf {
        std::env::var_os("HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .or_else(home_from_passwd)
            .unwrap_or_else(|| PathBuf::from("/"))
    }

    /// `~/Library/Application Support`.
    fn config_root() -> PathBuf {
        Self::home().join("Library").join("Application Support")
    }

    /// The same folder as [`Paths::config_root`]: macOS keeps no separate local root.
    fn local_root() -> PathBuf {
        Self::config_root()
    }

    /// Claude keeps `Claude-<name>` profiles next to its own `Claude` folder.
    fn profiles_root() -> PathBuf {
        Self::config_root()
    }

    fn current_exe() -> String {
        std::env::current_exe().map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|_| brand::NAME.to_string())
    }

    /// Strip surrounding quotes, make absolute, drop `.` parts, repeated and trailing `/` (the
    /// root stays `/`); case is kept.
    fn norm_path(p: &str) -> String {
        let t = p.trim_matches('"');
        if t.is_empty() {
            return String::new();
        }
        let abs = std::path::absolute(Path::new(t)).unwrap_or_else(|_| PathBuf::from(t));
        let clean: PathBuf = abs.components().collect();
        clean.to_string_lossy().into_owned()
    }

    /// Equal after [`Paths::norm_path`], or the same file when both exist (so `/var/x` and
    /// `/private/var/x` match).
    fn same_path(a: &str, b: &str) -> bool {
        let (na, nb) = (Self::norm_path(a), Self::norm_path(b));
        if na.is_empty() || nb.is_empty() {
            return false;
        }
        if na == nb {
            return true;
        }
        match (std::fs::metadata(&na), std::fs::metadata(&nb)) {
            (Ok(x), Ok(y)) => x.dev() == y.dev() && x.ino() == y.ino(),
            _ => false,
        }
    }

    /// Normalised; `child` is `dir` itself or lies below it.
    fn is_under(child: &str, dir: &str) -> bool {
        let (c, d) = (Self::norm_path(child), Self::norm_path(dir));
        if c.is_empty() || d.is_empty() {
            return false;
        }
        c == d || d == "/" || (c.starts_with(&d) && c.as_bytes().get(d.len()) == Some(&b'/'))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn norm_path_rules() {
        assert_eq!(Os::norm_path("/a//b/./c/"), "/a/b/c");
        assert_eq!(Os::norm_path(r#""/a/B""#), "/a/B");
        assert_eq!(Os::norm_path("/"), "/");
        assert_eq!(Os::norm_path(""), "");
        assert!(Os::norm_path("rel").starts_with('/'));
    }

    #[test]
    fn same_path_rules() {
        assert!(Os::same_path("/x/y/", "/x/y"));
        assert!(!Os::same_path("/x/y", "/x/Y"));
        assert!(!Os::same_path("/x", "/xy"));
        assert!(!Os::same_path("", ""));
        assert!(!Os::same_path("/x", ""));
        let d = std::env::temp_dir().canonicalize().unwrap();
        let s = d.to_string_lossy();
        if let Some(short) = s.strip_prefix("/private") {
            assert!(Os::same_path(&s, short));
        }
    }

    #[test]
    fn is_under_rules() {
        assert!(Os::is_under("/a/b/c", "/a/b/"));
        assert!(Os::is_under("/a/b", "/a/b"));
        assert!(Os::is_under("/a", "/"));
        assert!(!Os::is_under("/a/bc", "/a/b"));
        assert!(!Os::is_under("/a/B/c", "/a/b"));
        assert!(!Os::is_under("", "/"));
    }

    #[test]
    fn folders_are_absolute() {
        assert!(Os::home().is_absolute());
        assert!(Os::config_root().ends_with("Library/Application Support"));
        assert_eq!(Os::local_root(), Os::config_root());
        assert_eq!(Os::profiles_root(), Os::config_root());
        assert!(Path::new(&Os::current_exe()).is_absolute());
        assert!(home_from_passwd().is_some_and(|p| p.is_absolute()));
    }
}

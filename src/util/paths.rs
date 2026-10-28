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

//! Well-known folders and path helpers.
//!
//! API:
//! `app_data() -> PathBuf; local_app_data() -> PathBuf; home() -> PathBuf;`
//! `user_data() -> PathBuf` (%APPDATA%\Sessionmux, overridable via `set_user_data()` for smoke);
//! `set_folders(Option<(home, roaming, local)>)` (smoke demo fixture: replaces the three folders);
//! `slug(cwd) -> String` (every char not [A-Za-z0-9] -> '-');
//! `same_path(a, b) -> bool`, `norm_path(p)`, `is_under(child, dir)` (the system's path rules).
//! The real folders and the path rules come from the platform layer; the overrides live here.

use crate::brand;
use crate::platform::{Os, Paths};
use std::path::PathBuf;
use std::sync::RwLock;

static USER_DATA_OVERRIDE: RwLock<Option<PathBuf>> = RwLock::new(None);

/// Smoke demo fixture: (home, Roaming, Local) used instead of the real folders. Only this
/// process sees it; child processes keep the real environment.
static FOLDERS_OVERRIDE: RwLock<Option<(PathBuf, PathBuf, PathBuf)>> = RwLock::new(None);

/// Override `home()`, `app_data()` and `local_app_data()` (smoke demo fixture). `None` restores
/// the real folders.
pub fn set_folders(f: Option<(PathBuf, PathBuf, PathBuf)>) {
    if let Ok(mut g) = FOLDERS_OVERRIDE.write() {
        *g = f;
    }
}

fn folder_override(pick: fn(&(PathBuf, PathBuf, PathBuf)) -> &PathBuf) -> Option<PathBuf> {
    FOLDERS_OVERRIDE.read().ok().and_then(|g| g.as_ref().map(|f| pick(f).clone()))
}

/// The user's home directory (%USERPROFILE%).
pub fn home() -> PathBuf {
    if let Some(p) = folder_override(|f| &f.0) {
        return p;
    }
    <Os as Paths>::home()
}

/// The per-user settings root (%APPDATA%, Roaming).
pub fn app_data() -> PathBuf {
    if let Some(p) = folder_override(|f| &f.1) {
        return p;
    }
    <Os as Paths>::config_root()
}

/// The per-user local data root (%LOCALAPPDATA%).
pub fn local_app_data() -> PathBuf {
    if let Some(p) = folder_override(|f| &f.2) {
        return p;
    }
    <Os as Paths>::local_root()
}

/// Sessionmux's own data folder: `%APPDATA%\Sessionmux` unless overridden (smoke mode).
pub fn user_data() -> PathBuf {
    if let Ok(g) = USER_DATA_OVERRIDE.read() {
        if let Some(p) = g.as_ref() {
            return p.clone();
        }
    }
    app_data().join(brand::DATA_DIR_NAME)
}

/// Override `user_data()` (smoke mode / tests). `None` restores the default.
pub fn set_user_data(p: Option<PathBuf>) {
    if let Ok(mut g) = USER_DATA_OVERRIDE.write() {
        *g = p;
    }
}

/// True while `set_user_data` overrides the folder (smoke mode): no registry access then.
pub fn user_data_overridden() -> bool {
    USER_DATA_OVERRIDE.read().map(|g| g.is_some()).unwrap_or(false)
}

/// Store file `<user_data>\sessionmux.json`.
#[allow(dead_code)] // used only by the unit tests
pub fn store_file() -> PathBuf {
    user_data().join(brand::STORE_FILE)
}

/// Log file `<user_data>\sessionmux.log`.
pub fn log_file() -> PathBuf {
    user_data().join(brand::LOG_FILE)
}

/// Claude Code project slug: every character not in [A-Za-z0-9] becomes '-'.
/// e.g. `C:\Users\u\x` -> `C--Users-u-x`, `/Users/me/proj` -> `-Users-me-proj`. Works per UTF-16
/// unit (no `u` flag), as Claude Code does, so an astral character such as an emoji becomes two
/// dashes.
pub fn slug(cwd: &str) -> String {
    let mut out = String::with_capacity(cwd.len());
    for c in cwd.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else {
            for _ in 0..c.len_utf16() {
                out.push('-');
            }
        }
    }
    out
}

/// Normalise for comparisons with the system's rules (on Windows: quotes stripped, absolute,
/// '/' -> '\', no trailing separators, lowercase). Empty input gives an empty string.
#[allow(dead_code)] // used only by the unit tests
pub fn norm_path(p: &str) -> String {
    <Os as Paths>::norm_path(p)
}

/// True if two paths refer to the same location (on Windows: case-insensitive, trailing-slash
/// tolerant). Empty paths never match.
pub fn same_path(a: &str, b: &str) -> bool {
    <Os as Paths>::same_path(a, b)
}

/// True if `child` is `dir` itself or lies inside it (normalised).
#[allow(dead_code)] // used only by the unit tests
pub fn is_under(child: &str, dir: &str) -> bool {
    <Os as Paths>::is_under(child, dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slug_matches_claude_code() {
        assert_eq!(slug(r"C:\Users\u\x"), "C--Users-u-x");
        assert_eq!(slug("C:/a b/c_d.e"), "C--a-b-c-d-e");
        assert_eq!(slug(""), "");
        assert_eq!(slug("\u{1F600}"), "--");
        assert_eq!(slug("a\u{e9}\u{1F600}b"), "a---b");
    }

    #[test]
    fn slug_of_a_posix_path() {
        assert_eq!(slug("/Users/me/proj"), "-Users-me-proj");
        assert_eq!(slug("/home/a b/c.d"), "-home-a-b-c-d");
    }

    #[cfg(windows)]
    #[test]
    fn path_rules_follow_the_system() {
        assert!(same_path(r"C:\Users\A\AppData", r"c:\users\a\appdata\"));
        assert!(!same_path("", ""));
        assert!(is_under(r"C:\a\b\c.exe", r"C:\A\b\"));
        assert!(!is_under(r"C:\a\bc", r"C:\a\b"));
        assert_eq!(norm_path(r"C:\X\"), r"c:\x");
    }

    #[cfg(not(windows))]
    #[test]
    fn path_rules_follow_the_system() {
        assert_eq!(norm_path("/Users/A//b/./c/"), "/Users/A/b/c");
        assert_eq!(norm_path("\"/a b/\""), "/a b");
        assert_eq!(norm_path("/"), "/");
        assert!(same_path("/Users/A/b", "/Users/A//b/"));
        assert!(!same_path("/no/such/A", "/no/such/a"));
        assert!(!same_path("", ""));
        assert!(is_under("/a/b/c", "/a/b/"));
        assert!(is_under("/a/b", "/a/b"));
        assert!(!is_under("/a/bc", "/a/b"));
        assert!(!is_under("/no/such/B/c", "/no/such/b"));
        // `/var` is a link to `/private/var`: both spellings name one folder.
        let d = tempfile::tempdir().unwrap();
        let real = d.path().canonicalize().unwrap();
        let real = real.to_string_lossy();
        if let Some(short) = real.strip_prefix("/private") {
            assert!(same_path(&real, short));
            assert!(same_path(short, &format!("{real}/")));
        }
    }

    #[test]
    fn user_data_override() {
        let d = tempfile::tempdir().unwrap();
        set_user_data(Some(d.path().to_path_buf()));
        assert_eq!(user_data(), d.path());
        assert_eq!(store_file(), d.path().join("sessionmux.json"));
        set_user_data(None);
        assert!(user_data().ends_with("Sessionmux"));
    }
}

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

//! Windows folders (`%USERPROFILE%`, `%APPDATA%`, `%LOCALAPPDATA%`) and path rules: comparisons
//! are case-insensitive, accept either separator and ignore trailing separators.

use std::path::{Path, PathBuf};

use crate::brand;
use crate::platform::{Os, Paths};

fn env_dir(name: &str) -> Option<PathBuf> {
    std::env::var_os(name).filter(|v| !v.is_empty()).map(PathBuf::from)
}

impl Paths for Os {
    /// %USERPROFILE%, else %HOMEDRIVE%%HOMEPATH%, else the default profile of the system drive.
    fn home() -> PathBuf {
        env_dir("USERPROFILE")
            .or_else(|| {
                let mut s = std::env::var_os("HOMEDRIVE")?;
                s.push(std::env::var_os("HOMEPATH")?);
                Some(PathBuf::from(s))
            })
            .unwrap_or_else(|| {
                // Last resort: the default profile on the system drive, built at run time.
                let mut drive = std::env::var_os("SystemDrive").unwrap_or_else(|| "C:".into());
                drive.push(std::path::MAIN_SEPARATOR_STR);
                PathBuf::from(drive).join("Users").join("Default")
            })
    }

    /// %APPDATA% (Roaming); falls back to `<home>\AppData\Roaming`.
    fn config_root() -> PathBuf {
        env_dir("APPDATA").unwrap_or_else(|| Self::home().join("AppData").join("Roaming"))
    }

    /// %LOCALAPPDATA%; falls back to `<home>\AppData\Local`.
    fn local_root() -> PathBuf {
        env_dir("LOCALAPPDATA").unwrap_or_else(|| Self::home().join("AppData").join("Local"))
    }

    /// Claude keeps `Claude-<name>` profiles in Roaming, next to its own `Claude` folder.
    fn profiles_root() -> PathBuf {
        Self::config_root()
    }

    /// GetModuleFileNameW; falls back to `std::env::current_exe`.
    fn current_exe() -> String {
        use windows::Win32::System::LibraryLoader::GetModuleFileNameW;
        let mut buf = vec![0u16; 512];
        loop {
            // SAFETY: a None module selects the current exe; buf is a valid writable slice.
            let n = unsafe { GetModuleFileNameW(None, &mut buf) } as usize;
            if n == 0 {
                break;
            }
            if n < buf.len() {
                return String::from_utf16_lossy(&buf[..n]);
            }
            if buf.len() >= 32 * 1024 {
                break;
            }
            let len = buf.len() * 2;
            buf.resize(len, 0);
        }
        std::env::current_exe().map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|_| brand::EXE.to_string())
    }

    /// Strip surrounding quotes, make absolute, '/' -> '\', drop trailing separators, lowercase.
    fn norm_path(p: &str) -> String {
        let t = p.trim_matches('"');
        if t.is_empty() {
            return String::new();
        }
        let t = t.replace('/', "\\");
        let abs = std::path::absolute(Path::new(&t)).unwrap_or_else(|_| PathBuf::from(&t));
        let mut s = abs.to_string_lossy().into_owned();
        while s.len() > 1 && s.ends_with('\\') {
            s.pop();
        }
        s.to_lowercase()
    }

    /// Case-insensitive, trailing-slash tolerant.
    fn same_path(a: &str, b: &str) -> bool {
        if a.is_empty() || b.is_empty() {
            return false;
        }
        let (na, nb) = (Self::norm_path(a), Self::norm_path(b));
        !na.is_empty() && na == nb
    }

    /// Normalised, case-insensitive.
    fn is_under(child: &str, dir: &str) -> bool {
        let (c, d) = (Self::norm_path(child), Self::norm_path(dir));
        if c.is_empty() || d.is_empty() {
            return false;
        }
        c == d || (c.starts_with(&d) && c.as_bytes().get(d.len()) == Some(&b'\\'))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_path_rules() {
        assert!(Os::same_path(r"C:\Users\A\AppData", r"c:\users\a\appdata\"));
        assert!(Os::same_path("C:/x/y/", r"C:\X\Y"));
        assert!(Os::same_path(r#""C:\x""#, r"C:\x"));
        assert!(!Os::same_path(r"C:\x", r"C:\xy"));
        assert!(!Os::same_path("", ""));
        assert!(!Os::same_path(r"C:\x", ""));
    }

    #[test]
    fn is_under_rules() {
        assert!(Os::is_under(r"C:\a\b\c.exe", r"C:\A\b\"));
        assert!(Os::is_under(r"C:\a\b", r"C:\a\b"));
        assert!(!Os::is_under(r"C:\a\bc", r"C:\a\b"));
    }

    #[test]
    fn folders_are_absolute() {
        assert!(Os::home().is_absolute());
        assert!(Os::config_root().is_absolute());
        assert!(Os::local_root().is_absolute());
        assert_eq!(Os::profiles_root(), Os::config_root());
        assert!(Path::new(&Os::current_exe()).is_absolute());
    }
}

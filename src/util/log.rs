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

//! File logger for `sessionmux.log`.
//!
//! API:
//! `pub fn init(path: &Path); pub fn info(&str); pub fn warn(&str); pub fn error(&str); pub fn debug(&str);`
//! Lines look like `2026-10-26T04:51:27.186Z [INFO ] msg`. When the file would grow past 1 MB it is
//! renamed to `<file>.1` (one backup kept) and a fresh file is started. Writes are synchronous.
//! Before `init` (or after an unrecoverable write error) messages are dropped silently.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

pub const MAX_BYTES: u64 = 1024 * 1024;

struct State {
    path: PathBuf,
    size: u64,
    broken: bool,
}

static STATE: Mutex<Option<State>> = Mutex::new(None);

/// Start logging to `path` (parent directory is created). Calling again switches files.
pub fn init(path: &Path) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    if let Ok(mut g) = STATE.lock() {
        *g = Some(State { path: path.to_path_buf(), size, broken: false });
    }
}

pub fn debug(msg: &str) {
    write("DEBUG", msg);
}
pub fn info(msg: &str) {
    write("INFO", msg);
}
pub fn warn(msg: &str) {
    write("WARN", msg);
}
pub fn error(msg: &str) {
    write("ERROR", msg);
}

fn write(level: &str, msg: &str) {
    let line = format!("{} [{:<5}] {}\n", iso_now(), level, msg);
    let Ok(mut g) = STATE.lock() else { return };
    let Some(st) = g.as_mut() else { return };
    if st.broken {
        return;
    }
    if st.size + line.len() as u64 > MAX_BYTES {
        let mut backup = st.path.clone().into_os_string();
        backup.push(".1");
        let _ = std::fs::remove_file(&backup);
        let _ = std::fs::rename(&st.path, &backup); // on failure we just keep appending
        st.size = 0;
    }
    match OpenOptions::new().create(true).append(true).open(&st.path) {
        Ok(mut f) => {
            if f.write_all(line.as_bytes()).is_ok() {
                st.size += line.len() as u64;
            } else {
                st.broken = true;
            }
        }
        Err(_) => st.broken = true,
    }
}

/// Current UTC time as `YYYY-MM-DDTHH:MM:SS.mmmZ` (same as JS `Date.toISOString()`).
pub fn iso_now() -> String {
    iso_from_millis(now_millis())
}

/// Milliseconds since the Unix epoch (JS `Date.now()`).
pub fn now_millis() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// Format milliseconds since the Unix epoch as an ISO-8601 UTC timestamp.
pub fn iso_from_millis(ms: u64) -> String {
    let secs = ms / 1000;
    let millis = ms % 1000;
    let days = (secs / 86_400) as i64;
    let sod = secs % 86_400;
    let (y, m, d) = civil_from_days(days);
    format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z", y, m, d, sod / 3600, (sod / 60) % 60, sod % 60, millis)
}

/// Howard Hinnant's days-to-civil algorithm (proleptic Gregorian, UTC).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iso_format() {
        assert_eq!(iso_from_millis(0), "1970-01-01T00:00:00.000Z");
        assert_eq!(iso_from_millis(951_782_400_000), "2000-02-29T00:00:00.000Z");
        assert_eq!(iso_from_millis(1_792_990_287_186), "2026-10-26T04:51:27.186Z");
    }

    #[test]
    fn line_shape_and_rotation() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("sessionmux.log");
        init(&p);
        info("hello");
        warn("w");
        let text = std::fs::read_to_string(&p).unwrap();
        assert!(text.contains("Z [INFO ] hello\n"), "{text}");
        assert!(text.contains("Z [WARN ] w\n"));
        let big = "x".repeat(600 * 1024);
        info(&big);
        info(&big);
        let mut backup = p.clone().into_os_string();
        backup.push(".1");
        assert!(Path::new(&backup).exists());
        assert!(std::fs::metadata(&p).unwrap().len() <= MAX_BYTES);
    }
}

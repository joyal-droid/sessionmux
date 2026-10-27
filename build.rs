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

//! Embeds the exe icon (resource id 1), the tray icon (resource id 2), the application
//! manifest (PerMonitorV2 DPI, Common-Controls v6, asInvoker) and version strings, and sets
//! `SESSIONMUX_BUILD_DATE` (YYYY-MM-DD, UTC) for `brand::build_date()` (shown in the About card).
//!
//! The date is taken when this script runs (it reruns when build.rs, assets/ or web/ change);
//! a release build can pin it with the environment variable `SESSIONMUX_BUILD_DATE=YYYY-MM-DD`.

use std::time::{SystemTime, UNIX_EPOCH};

/// (year, month, day) of a day count since 1970-01-01 (H. Hinnant's civil_from_days).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// A real calendar date YYYY-MM-DD, years 1970..=9999 (the same rules as `util::date::parse_ymd`).
fn is_ymd(s: &str) -> bool {
    let b = s.as_bytes();
    if !(b.len() == 10
        && b[4] == b'-'
        && b[7] == b'-'
        && b.iter().enumerate().all(|(i, c)| i == 4 || i == 7 || c.is_ascii_digit()))
    {
        return false;
    }
    let (Ok(y), Ok(m), Ok(d)) = (s[..4].parse::<i64>(), s[5..7].parse::<u32>(), s[8..].parse::<u32>()) else {
        return false;
    };
    let leap = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
    let dim = match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => return false,
    };
    (1970..=9999).contains(&y) && (1..=dim).contains(&d)
}

fn build_date() -> String {
    if let Ok(v) = std::env::var("SESSIONMUX_BUILD_DATE") {
        if is_ymd(v.trim()) {
            return v.trim().to_string();
        }
        // Fail the build: a release must not ship with a malformed build date.
        panic!("SESSIONMUX_BUILD_DATE={v:?} is not a valid YYYY-MM-DD date");
    }
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
    let (y, m, d) = civil_from_days(secs.div_euclid(86_400));
    format!("{y:04}-{m:02}-{d:02}")
}

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=assets/icon.ico");
    println!("cargo:rerun-if-changed=assets/tray.ico");
    println!("cargo:rerun-if-changed=assets/sessionmux.manifest");
    println!("cargo:rerun-if-changed=web");
    println!("cargo:rerun-if-env-changed=SESSIONMUX_BUILD_DATE");
    println!("cargo:rustc-env=SESSIONMUX_BUILD_DATE={}", build_date());

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let mut res = winresource::WindowsResource::new();
    res.set_icon("assets/icon.ico") // id 1 = application icon
        .set_icon_with_id("assets/tray.ico", "2") // id 2 = tray icon (util::win::IDI_TRAY)
        .set_manifest_file("assets/sessionmux.manifest")
        .set("ProductName", "Sessionmux")
        .set("FileDescription", "Sessionmux account switcher")
        .set("LegalCopyright", "Copyright (c) 2026 joyal-droid. CPAL-1.0.")
        .set(
            "LegalTrademarks",
            "Claude is a trademark of Anthropic, PBC. Sessionmux is unofficial and not affiliated with Anthropic.",
        )
        .set("CompanyName", "Sessionmux")
        .set("OriginalFilename", "Sessionmux.exe")
        .set("InternalName", "Sessionmux");
    if let Err(e) = res.compile() {
        // Fail the build loudly: an exe without its manifest would run DPI-unaware.
        panic!("winresource failed: {e}");
    }
}

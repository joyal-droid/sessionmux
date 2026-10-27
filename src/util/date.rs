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

//! Calendar-date helpers for ISO-8601 UTC timestamps. No chrono: civil-date arithmetic after
//! H. Hinnant; the clock comes from [`crate::util::log::iso_now`].
//!
//! Dates are `YYYY-MM-DD` strings (UTC); timestamps are ISO-8601 UTC as written by `iso_now`.

use crate::util::log;

/// Current UTC time, ISO-8601 with milliseconds (`2026-10-26T08:00:00.000Z`).
pub fn iso_now() -> String {
    log::iso_now()
}

/// `YYYY-MM-DD` → (year, month, day). Exactly 10 chars, real calendar dates only
/// (`2026-02-30` → None), years 1970..=9999.
pub fn parse_ymd(s: &str) -> Option<(i32, u32, u32)> {
    let b = s.as_bytes();
    if b.len() != 10 || b[4] != b'-' || b[7] != b'-' {
        return None;
    }
    let digits = |r: std::ops::Range<usize>| -> Option<u32> {
        b[r].iter().try_fold(0u32, |acc, c| c.is_ascii_digit().then(|| acc * 10 + u32::from(c - b'0')))
    };
    let (y, m, d) = (digits(0..4)? as i32, digits(5..7)?, digits(8..10)?);
    if !(1970..=9999).contains(&y) || !(1..=12).contains(&m) || d < 1 || d > days_in_month(y, m) {
        return None;
    }
    Some((y, m, d))
}

/// True for an ISO-8601 UTC timestamp as written by `iso_now` (`YYYY-MM-DDTHH:MM:SS[.mmm]Z`):
/// a valid date, `T`, and a trailing `Z`. Timestamps of this shape compare correctly as strings.
pub fn is_iso_utc(s: &str) -> bool {
    s.len() >= 20
        && s.is_ascii()
        && parse_ymd(&s[..10]).is_some()
        && s.as_bytes()[10] == b'T'
        && s.ends_with('Z')
        && s[11..s.len() - 1].bytes().all(|c| c.is_ascii_digit() || c == b':' || c == b'.')
}

fn days_in_month(y: i32, m: u32) -> u32 {
    match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        _ if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 => 29,
        _ => 28,
    }
}

/// Days since 1970-01-01 of a proleptic-Gregorian (year, month, day).
pub fn days_from_civil(y: i32, m: u32, d: u32) -> i64 {
    let y = i64::from(if m <= 2 { y - 1 } else { y });
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let m = i64::from(m);
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + i64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_days() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(2000, 3, 1), 11_017);
        assert_eq!(days_from_civil(2026, 10, 26), 20_752);
        assert_eq!(days_from_civil(2028, 3, 1) - days_from_civil(2028, 2, 28), 2);
    }

    #[test]
    fn parse() {
        assert_eq!(parse_ymd("2026-10-26"), Some((2026, 10, 26)));
        assert_eq!(parse_ymd("2028-02-29"), Some((2028, 2, 29)));
        for bad in
            ["2026-02-29", "2026-13-01", "2026-00-10", "2026-1-01", "2026/10/26", "+026-10-26", "2026-10-26 ", ""]
        {
            assert_eq!(parse_ymd(bad), None, "{bad}");
        }
        assert!(parse_ymd(&iso_now()[..10]).is_some());
    }

    #[test]
    fn iso_timestamps() {
        assert!(is_iso_utc(&iso_now()));
        assert!(is_iso_utc("2026-10-26T08:00:00Z"));
        assert!(is_iso_utc("2026-10-26T08:00:00.123Z"));
        for bad in [
            "2026-10-26",
            "2026-10-26 08:00:00Z",
            "2026-13-26T08:00:00Z",
            "2026-10-26T08:00:00+01",
            "2026-10-26T08:00:0xZ",
        ] {
            assert!(!is_iso_utc(bad), "{bad}");
        }
    }
}

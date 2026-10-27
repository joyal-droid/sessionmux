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

//! ShellExecuteW for links and files; Explorer with `/select,` to show a folder. Windows has no
//! privacy settings page for what Sessionmux does.

use std::path::Path;

use windows::core::PCWSTR;
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

use super::win;
use crate::platform::{Os, PrivacyPane, Shell};

/// The `/select,` argument that makes Explorer open the parent and select `path`.
fn select_arg(path: &Path) -> String {
    format!("/select,\"{}\"", path.display())
}

fn shell_open(op: &str, file: &str, params: Option<&str>) -> Result<(), String> {
    let op_w = win::wide(op);
    let file_w = win::wide(file);
    let params_w = params.map(win::wide);
    let params_p = params_w.as_ref().map(|w| PCWSTR(w.as_ptr())).unwrap_or(PCWSTR::null());
    // SAFETY: every string is a NUL-terminated UTF-16 buffer that outlives the call.
    let r = unsafe {
        ShellExecuteW(
            HWND::default(),
            PCWSTR(op_w.as_ptr()),
            PCWSTR(file_w.as_ptr()),
            params_p,
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };
    if (r.0 as isize) <= 32 {
        return Err(format!("ShellExecute {op} {file} failed ({})", r.0 as isize));
    }
    Ok(())
}

impl Shell for Os {
    fn open_url(url: &str) -> Result<(), String> {
        shell_open("open", url, None)
    }

    fn reveal(path: &Path) -> Result<(), String> {
        shell_open("open", "explorer.exe", Some(&select_arg(path)))
    }

    /// Windows asks for neither permission.
    fn open_privacy_settings(pane: PrivacyPane) -> Result<(), String> {
        Err(format!("Windows has no {pane:?} setting for Sessionmux."))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn select_argument_quotes_the_path() {
        assert_eq!(select_arg(Path::new(r"C:\a b\c")), r#"/select,"C:\a b\c""#);
    }
}

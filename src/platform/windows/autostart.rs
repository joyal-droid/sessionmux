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

//! Start with Windows: the `HKCU\Software\Microsoft\Windows\CurrentVersion\Run` value
//! "Sessionmux" = `"<exe>" --hidden`.

use std::ffi::c_void;

use windows::core::PCWSTR;
use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS};
use windows::Win32::System::Registry::{
    RegCloseKey, RegDeleteValueW, RegGetValueW, RegOpenKeyExW, RegSetValueExW, HKEY, HKEY_CURRENT_USER, KEY_SET_VALUE,
    REG_SZ, RRF_RT_REG_SZ,
};

use super::win::wide;
use crate::brand;
use crate::platform::{Autostart, AutostartStatus, Os};

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";

/// The command the Run value holds while login start is on.
fn run_command(exe: &str) -> String {
    format!("\"{}\" --hidden", exe)
}

/// True when the Run value exists (whatever exe it names).
fn run_value_present() -> bool {
    let subkey = wide(RUN_KEY);
    let value_name = wide(brand::RUN_VALUE);
    let mut cb = 0u32;
    // SAFETY: both names are NUL-terminated and live; no data buffer, only its size is asked for.
    let rc = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            PCWSTR(subkey.as_ptr()),
            PCWSTR(value_name.as_ptr()),
            RRF_RT_REG_SZ,
            None,
            None::<*mut c_void>,
            Some(&mut cb),
        )
    };
    rc == ERROR_SUCCESS
}

/// Write (`enabled`) or remove the Run value.
fn write_run_value(enabled: bool) -> Result<(), String> {
    let subkey = wide(RUN_KEY);
    let value_name = wide(brand::RUN_VALUE);
    let mut key = HKEY::default();
    // SAFETY: `subkey` is a NUL-terminated UTF-16 buffer that outlives the call and `key`
    // is a valid out-pointer.
    let rc = unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, PCWSTR(subkey.as_ptr()), 0, KEY_SET_VALUE, &mut key) };
    if rc != ERROR_SUCCESS {
        return Err(format!("could not open the Run key (error {})", rc.0));
    }
    let result = if enabled {
        let exe = std::env::current_exe().map_err(|e| e.to_string());
        match exe {
            Err(e) => Err(e),
            Ok(exe) => {
                let cmd = run_command(&exe.display().to_string());
                let data: Vec<u8> = wide(&cmd).iter().flat_map(|u| u.to_le_bytes()).collect();
                // SAFETY: `key` is an open key handle; `value_name` is NUL-terminated;
                // `data` is a NUL-terminated REG_SZ byte buffer alive for the call.
                let rc = unsafe { RegSetValueExW(key, PCWSTR(value_name.as_ptr()), 0, REG_SZ, Some(&data)) };
                if rc == ERROR_SUCCESS {
                    Ok(())
                } else {
                    Err(format!("could not write the Run value (error {})", rc.0))
                }
            }
        }
    } else {
        // SAFETY: `key` is an open key handle and `value_name` is NUL-terminated.
        let rc = unsafe { RegDeleteValueW(key, PCWSTR(value_name.as_ptr())) };
        if rc == ERROR_SUCCESS || rc == ERROR_FILE_NOT_FOUND {
            Ok(())
        } else {
            Err(format!("could not remove the Run value (error {})", rc.0))
        }
    };
    // SAFETY: `key` was opened above and is closed exactly once.
    let _ = unsafe { RegCloseKey(key) };
    result
}

impl Autostart for Os {
    fn status() -> AutostartStatus {
        if run_value_present() {
            AutostartStatus::On
        } else {
            AutostartStatus::Off
        }
    }

    /// Writes the Run value with the current exe path (also when it is already there, so a moved
    /// install starts from its new place) or removes it.
    fn set(enabled: bool) -> Result<AutostartStatus, String> {
        write_run_value(enabled)?;
        Ok(if enabled { AutostartStatus::On } else { AutostartStatus::Off })
    }

    /// Windows asks for no approval.
    fn open_settings() {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_command_shape() {
        assert_eq!(run_command(r"C:\a b\Sessionmux.exe"), r#""C:\a b\Sessionmux.exe" --hidden"#);
    }

    /// Read-only: the status call works whatever the machine's setting is.
    #[test]
    fn status_reads() {
        assert!(matches!(Os::status(), AutostartStatus::On | AutostartStatus::Off));
    }
}

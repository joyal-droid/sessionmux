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

//! Small HKCU registry helpers, mostly for the `(Default)` REG_SZ of a key (used by core::relay).
//!
//! `read_default(subkey) -> Option<String>`; `write_default(subkey, value) -> Result<(), String>`
//! (creates the key path when missing); `delete_default(subkey) -> Result<(), String>` (missing
//! value/key is Ok); `delete_value(subkey, name) -> Result<bool, String>` (true = it existed);
//! `key_exists(subkey) -> bool`;
//! `delete_tree(subkey)` (best effort).
//! All paths are relative to HKEY_CURRENT_USER. Callers decide which key; tests only ever use
//! throw-away `Software\Classes\sessionmux-test-<uuid>` keys.

use windows::core::PCWSTR;
use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_MORE_DATA, ERROR_SUCCESS};
use windows::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyExW, RegDeleteTreeW, RegDeleteValueW, RegGetValueW, RegOpenKeyExW, RegSetValueExW, HKEY,
    HKEY_CURRENT_USER, KEY_READ, KEY_SET_VALUE, REG_OPTION_NON_VOLATILE, REG_SZ, RRF_RT_REG_SZ,
};

use crate::util::win::wide;

/// `(Default)` of `HKCU\<subkey>` when it exists and is a non-empty REG_SZ.
pub fn read_default(subkey: &str) -> Option<String> {
    let sk = wide(subkey);
    let mut buf = vec![0u16; 512];
    for _ in 0..4 {
        let mut cb = (buf.len() * 2) as u32;
        // SAFETY: sk is NUL-terminated and outlives the call; a null value name selects
        // (Default); buf is writable for cb bytes and cb is a valid in/out pointer.
        let rc = unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                PCWSTR(sk.as_ptr()),
                PCWSTR::null(),
                RRF_RT_REG_SZ,
                None,
                Some(buf.as_mut_ptr().cast()),
                Some(&mut cb),
            )
        };
        if rc == ERROR_MORE_DATA {
            buf = vec![0u16; (cb as usize / 2) + 16];
            continue;
        }
        if rc != ERROR_SUCCESS {
            return None;
        }
        buf.truncate(cb as usize / 2);
        let s = crate::util::win::from_wide(&buf);
        return (!s.is_empty()).then_some(s);
    }
    None
}

/// Create `HKCU\<subkey>` (and every missing parent) and set its `(Default)` = `value` (REG_SZ).
pub fn write_default(subkey: &str, value: &str) -> Result<(), String> {
    let sk = wide(subkey);
    let mut key = HKEY::default();
    // SAFETY: sk is NUL-terminated and outlives the call; `key` is a valid out-pointer.
    let rc = unsafe {
        RegCreateKeyExW(
            HKEY_CURRENT_USER,
            PCWSTR(sk.as_ptr()),
            0,
            PCWSTR::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_SET_VALUE,
            None,
            &mut key,
            None,
        )
    };
    if rc != ERROR_SUCCESS {
        return Err(format!("could not create HKCU\\{subkey} (error {})", rc.0));
    }
    let data: Vec<u8> = wide(value).iter().flat_map(|u| u.to_le_bytes()).collect();
    // SAFETY: `key` is an open key handle; a null value name selects (Default); data is a
    // NUL-terminated REG_SZ byte buffer alive for the call.
    let rc = unsafe { RegSetValueExW(key, PCWSTR::null(), 0, REG_SZ, Some(&data)) };
    // SAFETY: `key` was opened above and is closed exactly once.
    unsafe {
        let _ = RegCloseKey(key);
    }
    if rc == ERROR_SUCCESS {
        Ok(())
    } else {
        Err(format!("could not set HKCU\\{subkey} (error {})", rc.0))
    }
}

/// Remove the `(Default)` value of `HKCU\<subkey>`; a missing key or value is not an error.
pub fn delete_default(subkey: &str) -> Result<(), String> {
    delete_value(subkey, None).map(|_| ())
}

/// Remove the value `name` (None = `(Default)`) of `HKCU\<subkey>`. Ok(true) when it was
/// deleted, Ok(false) when the key or the value does not exist.
pub fn delete_value(subkey: &str, name: Option<&str>) -> Result<bool, String> {
    let sk = wide(subkey);
    let mut key = HKEY::default();
    // SAFETY: sk is NUL-terminated and outlives the call; `key` is a valid out-pointer.
    let rc = unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, PCWSTR(sk.as_ptr()), 0, KEY_SET_VALUE, &mut key) };
    if rc == ERROR_FILE_NOT_FOUND {
        return Ok(false);
    }
    if rc != ERROR_SUCCESS {
        return Err(format!("could not open HKCU\\{subkey} (error {})", rc.0));
    }
    let name_w = name.map(wide);
    let name_p = name_w.as_ref().map(|w| PCWSTR(w.as_ptr())).unwrap_or(PCWSTR::null());
    // SAFETY: `key` is an open key handle; name_p is null ((Default)) or a NUL-terminated
    // buffer that outlives the call.
    let rc = unsafe { RegDeleteValueW(key, name_p) };
    // SAFETY: `key` was opened above and is closed exactly once.
    unsafe {
        let _ = RegCloseKey(key);
    }
    match rc {
        ERROR_SUCCESS => Ok(true),
        ERROR_FILE_NOT_FOUND => Ok(false),
        _ => Err(format!(
            "could not delete the value {} of HKCU\\{subkey} (error {})",
            name.unwrap_or("(Default)"),
            rc.0
        )),
    }
}

/// True when `HKCU\<subkey>` exists.
#[allow(dead_code)] // used by the registry tests only
pub fn key_exists(subkey: &str) -> bool {
    let sk = wide(subkey);
    let mut key = HKEY::default();
    // SAFETY: sk is NUL-terminated and outlives the call; `key` is a valid out-pointer.
    let rc = unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, PCWSTR(sk.as_ptr()), 0, KEY_READ, &mut key) };
    if rc == ERROR_SUCCESS {
        // SAFETY: `key` was opened above and is closed exactly once.
        unsafe {
            let _ = RegCloseKey(key);
        }
    }
    rc == ERROR_SUCCESS
}

/// Delete `HKCU\<subkey>` with everything below it (best effort). Refuses an empty subkey.
#[allow(dead_code)] // used by the registry tests only
pub fn delete_tree(subkey: &str) {
    if subkey.trim().trim_matches('\\').is_empty() {
        return;
    }
    let sk = wide(subkey);
    // SAFETY: sk is NUL-terminated and outlives the call; RegDeleteTreeW with a subkey deletes
    // that key and its descendants.
    let _ = unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, PCWSTR(sk.as_ptr())) };
}

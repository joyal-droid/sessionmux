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

//! Small Win32 helpers.
//!
//! API:
//! `wide(s) -> Vec<u16>` (NUL-terminated UTF-16); `from_wide(&[u16]) -> String` (stops at the first NUL);
//! `hinstance() -> HINSTANCE`; `last_error() -> String`; `dpi_scale(hwnd) -> f32` (1.0 = 96 DPI).
//! Resource ids embedded by build.rs: 1 (exe icon) and `IDI_TRAY` (tray icon).
//! `current_user_sid() -> Option<String>` (`S-1-5-21-…`, looked up once);
//! `process_user_sid(pid) -> Option<String>`; [`ffi`]: the named-pipe, file and SDDL calls the
//! hook pipe needs.

use std::sync::OnceLock;
use windows::core::{PCWSTR, PWSTR};
use windows::Win32::Foundation::{CloseHandle, GetLastError, LocalFree, HANDLE, HINSTANCE, HLOCAL, HWND};
use windows::Win32::Security::{GetTokenInformation, TokenUser, TOKEN_QUERY, TOKEN_USER};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::{
    GetCurrentProcess, OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::HiDpi::{GetDpiForSystem, GetDpiForWindow};

/// Resource id of the tray icon (assets/tray.ico).
pub const IDI_TRAY: u16 = 2;

/// UTF-16, NUL-terminated, for passing to `PCWSTR` parameters.
pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Decode UTF-16 up to the first NUL (or the whole slice). Invalid surrogates become U+FFFD.
pub fn from_wide(w: &[u16]) -> String {
    let end = w.iter().position(|&c| c == 0).unwrap_or(w.len());
    String::from_utf16_lossy(&w[..end])
}

/// `MAKEINTRESOURCEW(id)`.
pub fn make_int_resource(id: u16) -> PCWSTR {
    PCWSTR(id as usize as *const u16)
}

/// Module handle of Sessionmux.exe.
pub fn hinstance() -> HINSTANCE {
    // SAFETY: GetModuleHandleW(NULL) only reads the PEB of the current process; it cannot fail
    // for the exe itself and returns a handle that need not be freed.
    match unsafe { GetModuleHandleW(PCWSTR::null()) } {
        Ok(h) => HINSTANCE::from(h),
        Err(_) => HINSTANCE::default(),
    }
}

/// `GetLastError()` formatted as "message (code N)".
pub fn last_error() -> String {
    // SAFETY: GetLastError reads the calling thread's last-error value; no preconditions.
    let code = unsafe { GetLastError() };
    let err = windows::core::Error::from(code);
    let msg = err.message();
    let msg = msg.trim();
    if msg.is_empty() {
        format!("Win32 error {}", code.0)
    } else {
        format!("{} (code {})", msg, code.0)
    }
}

/// Monitor DPI scale for `hwnd` (1.0 = 96 DPI). A null/invalid hwnd gives the system DPI.
pub fn dpi_scale(hwnd: HWND) -> f32 {
    // SAFETY: GetDpiForWindow tolerates invalid handles (returns 0); GetDpiForSystem has no
    // preconditions.
    let dpi = unsafe {
        let d = if hwnd.is_invalid() { 0 } else { GetDpiForWindow(hwnd) };
        if d == 0 {
            GetDpiForSystem()
        } else {
            d
        }
    };
    if dpi == 0 {
        1.0
    } else {
        dpi as f32 / 96.0
    }
}

/// The SID of the user this process runs as, in string form; `None` when it cannot be read.
pub fn current_user_sid() -> Option<String> {
    static SID: OnceLock<Option<String>> = OnceLock::new();
    SID.get_or_init(|| {
        // SAFETY: GetCurrentProcess returns a pseudo-handle that needs no closing.
        token_user_sid(unsafe { GetCurrentProcess() })
    })
    .clone()
}

/// The SID of the user process `pid` runs as; `None` when the process cannot be queried (for
/// example because it belongs to another user).
pub fn process_user_sid(pid: u32) -> Option<String> {
    // SAFETY: opens a process for limited query only; the handle is closed below.
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?;
    let sid = token_user_sid(process);
    // SAFETY: `process` was returned by OpenProcess above and is closed exactly once.
    let _ = unsafe { CloseHandle(process) };
    sid
}

/// The token user of `process` as a string SID.
fn token_user_sid(process: HANDLE) -> Option<String> {
    let mut token = HANDLE::default();
    // SAFETY: `process` is a valid process handle; `token` is a valid out-pointer.
    unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut token) }.ok()?;
    let sid = token_sid_string(token);
    // SAFETY: `token` was returned by OpenProcessToken above and is closed exactly once.
    let _ = unsafe { CloseHandle(token) };
    sid
}

fn token_sid_string(token: HANDLE) -> Option<String> {
    let mut len = 0u32;
    // SAFETY: a size query with no buffer; it fails with ERROR_INSUFFICIENT_BUFFER and sets `len`.
    let _ = unsafe { GetTokenInformation(token, TokenUser, None, 0, &mut len) };
    if len == 0 || len > 64 * 1024 {
        return None;
    }
    // u64 storage keeps the TOKEN_USER at the front suitably aligned.
    let mut buf = vec![0u64; (len as usize).div_ceil(8)];
    // SAFETY: `buf` holds at least `len` writable bytes; `len` is a valid in/out pointer.
    unsafe { GetTokenInformation(token, TokenUser, Some(buf.as_mut_ptr().cast()), len, &mut len) }.ok()?;
    // SAFETY: on success the buffer starts with a TOKEN_USER whose SID points into the buffer.
    let user = unsafe { &*(buf.as_ptr() as *const TOKEN_USER) };
    let mut text = PWSTR::null();
    // SAFETY: `user.User.Sid` is a valid SID inside `buf`; `text` receives a LocalAlloc'd string.
    if !unsafe { ffi::ConvertSidToStringSidW(user.User.Sid.0, &mut text) }.as_bool() || text.is_null() {
        return None;
    }
    // SAFETY: `text` is a NUL-terminated string returned above.
    let sid = unsafe { text.to_string() }.ok();
    // SAFETY: `text` was allocated by ConvertSidToStringSidW and is freed exactly once.
    let _ = unsafe { LocalFree(HLOCAL(text.0.cast())) };
    sid.filter(|s| s.starts_with("S-1-"))
}

/// Raw bindings for the named-pipe, file and SDDL calls of the hook pipe.
pub mod ffi {
    use windows::core::{PCWSTR, PWSTR};
    use windows::Win32::Foundation::{BOOL, HANDLE};
    use windows::Win32::Security::SECURITY_ATTRIBUTES;

    pub const GENERIC_READ: u32 = 0x8000_0000;
    pub const GENERIC_WRITE: u32 = 0x4000_0000;
    pub const OPEN_EXISTING: u32 = 3;
    pub const PIPE_ACCESS_DUPLEX: u32 = 0x0000_0003;
    pub const FILE_FLAG_FIRST_PIPE_INSTANCE: u32 = 0x0008_0000;
    pub const PIPE_TYPE_MESSAGE: u32 = 0x0000_0004;
    pub const PIPE_READMODE_MESSAGE: u32 = 0x0000_0002;
    pub const PIPE_REJECT_REMOTE_CLIENTS: u32 = 0x0000_0008;
    pub const PIPE_UNLIMITED_INSTANCES: u32 = 255;
    /// Lets the server identify the client but never act as it.
    pub const SECURITY_SQOS_PRESENT: u32 = 0x0010_0000;
    pub const SECURITY_IDENTIFICATION: u32 = 0x0001_0000;
    pub const SDDL_REVISION_1: u32 = 1;
    pub const ERROR_FILE_NOT_FOUND: u32 = 2;
    pub const ERROR_BROKEN_PIPE: u32 = 109;
    pub const ERROR_PIPE_BUSY: u32 = 231;
    pub const ERROR_NO_DATA: u32 = 232;
    pub const ERROR_PIPE_NOT_CONNECTED: u32 = 233;
    pub const ERROR_MORE_DATA: u32 = 234;
    pub const ERROR_PIPE_CONNECTED: u32 = 535;

    #[link(name = "kernel32")]
    extern "system" {
        pub fn CreateNamedPipeW(
            name: PCWSTR,
            open_mode: u32,
            pipe_mode: u32,
            max_instances: u32,
            out_buffer: u32,
            in_buffer: u32,
            default_timeout: u32,
            security: *const SECURITY_ATTRIBUTES,
        ) -> HANDLE;
        pub fn ConnectNamedPipe(pipe: HANDLE, overlapped: *mut core::ffi::c_void) -> BOOL;
        pub fn DisconnectNamedPipe(pipe: HANDLE) -> BOOL;
        pub fn WaitNamedPipeW(name: PCWSTR, timeout_ms: u32) -> BOOL;
        pub fn PeekNamedPipe(
            pipe: HANDLE,
            buffer: *mut core::ffi::c_void,
            buffer_size: u32,
            read: *mut u32,
            available: *mut u32,
            left_in_message: *mut u32,
        ) -> BOOL;
        pub fn GetNamedPipeServerProcessId(pipe: HANDLE, pid: *mut u32) -> BOOL;
        pub fn CreateFileW(
            name: PCWSTR,
            access: u32,
            share: u32,
            security: *const SECURITY_ATTRIBUTES,
            disposition: u32,
            flags: u32,
            template: HANDLE,
        ) -> HANDLE;
        pub fn ReadFile(
            file: HANDLE,
            buffer: *mut core::ffi::c_void,
            len: u32,
            read: *mut u32,
            overlapped: *mut core::ffi::c_void,
        ) -> BOOL;
        pub fn WriteFile(
            file: HANDLE,
            buffer: *const core::ffi::c_void,
            len: u32,
            written: *mut u32,
            overlapped: *mut core::ffi::c_void,
        ) -> BOOL;
        pub fn FlushFileBuffers(file: HANDLE) -> BOOL;
    }

    #[link(name = "advapi32")]
    extern "system" {
        pub fn ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl: PCWSTR,
            revision: u32,
            descriptor: *mut *mut core::ffi::c_void,
            size: *mut u32,
        ) -> BOOL;
        pub fn ConvertSidToStringSidW(sid: *mut core::ffi::c_void, out: *mut PWSTR) -> BOOL;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wide_roundtrip() {
        let w = wide("Sessionmux — ✓");
        assert_eq!(*w.last().unwrap(), 0);
        assert_eq!(from_wide(&w), "Sessionmux — ✓");
        assert_eq!(from_wide(&[0x41, 0x42]), "AB");
        assert_eq!(from_wide(&[0x41, 0, 0x42]), "A");
    }

    #[test]
    fn dpi_default() {
        assert!(dpi_scale(HWND::default()) >= 1.0);
        assert!(!hinstance().is_invalid());
    }
}

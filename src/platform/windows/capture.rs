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

//! What the smoke run needs on Windows: the parent console, a message pump, window captures with
//! `PrintWindow(PW_RENDERFULLCONTENT)` and memory figures (working set and private commit) of
//! Sessionmux and its `msedgewebview2.exe` descendants.

use std::path::Path;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{CloseHandle, BOOL, FILETIME, HWND, RECT};
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GetDC, ReleaseDC, SelectObject, BITMAPINFO,
    BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, HDC, HGDIOBJ,
};
use windows::Win32::System::Console::{AttachConsole, ATTACH_PARENT_PROCESS};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};
use windows::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS};
use windows::Win32::System::Threading::{
    GetCurrentProcessId, GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, GetWindowRect, PeekMessageW, TranslateMessage, MSG, PM_REMOVE, PW_RENDERFULLCONTENT, WM_QUIT,
};

use crate::platform::windows::win;
use crate::platform::{Capture, Footprint, Os, WindowId};
use crate::ui::panel::encode_png;

#[link(name = "user32")]
extern "system" {
    // Declared here because windows-rs puts PrintWindow behind the Win32_Storage_Xps feature.
    fn PrintWindow(hwnd: HWND, hdc: HDC, flags: u32) -> BOOL;
}

/// Capture `hwnd` with `PrintWindow(PW_RENDERFULLCONTENT)` and write it as a PNG.
/// Returns the image size. The window must be shown (it may be off-screen).
fn capture_window_png(hwnd: HWND, path: &Path) -> Result<(u32, u32), String> {
    let mut wr = RECT::default();
    // SAFETY: valid out-pointer.
    unsafe { GetWindowRect(hwnd, &mut wr) }.map_err(|e| format!("GetWindowRect: {e}"))?;
    let (w, h) = (wr.right - wr.left, wr.bottom - wr.top);
    if w <= 0 || h <= 0 {
        return Err("window has no size".into());
    }
    let bmi = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: w,
            biHeight: -h, // top-down
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut pixels: Vec<u8>;
    // SAFETY: standard GDI capture sequence; every handle created here is released below, and
    // `bits` points to w*h*4 bytes owned by the DIB section until DeleteObject.
    unsafe {
        let screen = GetDC(None);
        let mem = CreateCompatibleDC(screen);
        let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
        let dib = CreateDIBSection(mem, &bmi, DIB_RGB_COLORS, &mut bits, None, 0);
        let dib = match dib {
            Ok(d) if !bits.is_null() => d,
            other => {
                if let Ok(d) = other {
                    let _ = DeleteObject(d);
                }
                let _ = DeleteDC(mem);
                ReleaseDC(None, screen);
                return Err(format!("CreateDIBSection: {}", win::last_error()));
            }
        };
        let old = SelectObject(mem, HGDIOBJ(dib.0));
        let ok = PrintWindow(hwnd, mem, PW_RENDERFULLCONTENT).as_bool();
        pixels = std::slice::from_raw_parts(bits as *const u8, (w * h * 4) as usize).to_vec();
        SelectObject(mem, old);
        let _ = DeleteObject(dib);
        let _ = DeleteDC(mem);
        ReleaseDC(None, screen);
        if !ok {
            return Err(format!("PrintWindow: {}", win::last_error()));
        }
    }
    for px in pixels.as_chunks_mut::<4>().0 {
        px[3] = 0xFF; // PrintWindow leaves alpha undefined for opaque windows
    }
    let png = encode_png(w as u32, h as u32, &pixels);
    std::fs::write(path, png).map_err(|e| format!("write {}: {e}", path.display()))?;
    Ok((w as u32, h as u32))
}

fn current_pid() -> u32 {
    // SAFETY: no preconditions.
    unsafe { GetCurrentProcessId() }
}

/// (working set, private commit) of `pid` in bytes.
fn mem(pid: u32) -> Option<(u64, u64)> {
    // SAFETY: OpenProcess with query-limited rights; the handle is closed below.
    let h = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?;
    let mut pmc =
        PROCESS_MEMORY_COUNTERS { cb: std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32, ..Default::default() };
    // SAFETY: `h` is a live process handle and `pmc` a correctly sized out-struct.
    let r = unsafe { GetProcessMemoryInfo(h, &mut pmc, pmc.cb) };
    // SAFETY: closing the handle opened above exactly once.
    let _ = unsafe { CloseHandle(h) };
    r.ok().map(|_| (pmc.WorkingSetSize as u64, pmc.PagefileUsage as u64))
}

fn creation_time(pid: u32) -> Option<u64> {
    // SAFETY: OpenProcess with query-limited rights; the handle is closed below.
    let h = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?;
    let (mut c, mut e, mut k, mut u) =
        (FILETIME::default(), FILETIME::default(), FILETIME::default(), FILETIME::default());
    // SAFETY: valid handle and out-pointers.
    let r = unsafe { GetProcessTimes(h, &mut c, &mut e, &mut k, &mut u) };
    // SAFETY: closing the handle opened above exactly once.
    let _ = unsafe { CloseHandle(h) };
    r.ok().map(|_| ((c.dwHighDateTime as u64) << 32) | c.dwLowDateTime as u64)
}

/// (working set sum, private commit sum, number of WebView2 descendants).
fn tree_mem() -> (u64, u64, usize) {
    let me = current_pid();
    let (mut total, mut private) = mem(me).unwrap_or((0, 0));
    let mut entries: Vec<(u32, u32, String)> = Vec::new();
    // SAFETY: snapshot handle closed below; PROCESSENTRY32W initialised with its size.
    unsafe {
        if let Ok(snap) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) {
            let mut pe =
                PROCESSENTRY32W { dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
            if Process32FirstW(snap, &mut pe).is_ok() {
                loop {
                    let len = pe.szExeFile.iter().position(|&c| c == 0).unwrap_or(pe.szExeFile.len());
                    entries.push((
                        pe.th32ProcessID,
                        pe.th32ParentProcessID,
                        win::from_wide(&pe.szExeFile[..len]).to_ascii_lowercase(),
                    ));
                    if Process32NextW(snap, &mut pe).is_err() {
                        break;
                    }
                }
            }
            let _ = CloseHandle(snap);
        }
    }
    let my_start = creation_time(me).unwrap_or(0);
    let mut parents = vec![me];
    let mut found: Vec<u32> = Vec::new();
    while let Some(p) = parents.pop() {
        for (pid, ppid, name) in &entries {
            if *ppid == p && !found.contains(pid) && *pid != me && name == "msedgewebview2.exe" {
                // Guard against pid reuse: a real child starts after its parent.
                if creation_time(*pid).unwrap_or(0) >= my_start {
                    found.push(*pid);
                    parents.push(*pid);
                }
            }
        }
    }
    for pid in &found {
        let (w, p) = mem(*pid).unwrap_or((0, 0));
        total += w;
        private += p;
    }
    (total, private, found.len())
}

impl Capture for Os {
    fn attach_console() {
        // SAFETY: AttachConsole has no memory-safety preconditions; failure (no parent console,
        // e.g. output already redirected) is fine and ignored.
        let _ = unsafe { AttachConsole(ATTACH_PARENT_PROCESS) };
    }

    /// Dispatch this thread's messages for `ms` milliseconds.
    fn pump(ms: u64) {
        let end = Instant::now() + Duration::from_millis(ms);
        let mut msg = MSG::default();
        while Instant::now() < end {
            // SAFETY: standard non-blocking message pump on the UI thread; `msg` is a valid
            // out-pointer.
            unsafe {
                while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                    if msg.message == WM_QUIT {
                        return;
                    }
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn panel_png(w: WindowId, path: &Path) -> Result<(u32, u32), String> {
        capture_window_png(HWND(w.0 as _), path)
    }

    fn footprint(helpers: bool) -> Footprint {
        if helpers {
            let (resident, private, helpers) = tree_mem();
            Footprint { resident, private, helpers }
        } else {
            let (resident, private) = mem(current_pid()).unwrap_or((0, 0));
            Footprint { resident, private, helpers: 0 }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn footprint_of_this_process() {
        let own = <Os as Capture>::footprint(false);
        assert!(own.resident > 0 && own.private > 0);
        assert_eq!(own.helpers, 0);
        let tree = <Os as Capture>::footprint(true);
        assert!(tree.resident > 0);
        assert!(creation_time(current_pid()).is_some());
    }

    #[test]
    fn capture_needs_a_window() {
        let path = std::env::temp_dir().join("sessionmux-capture-test.png");
        assert!(<Os as Capture>::panel_png(WindowId(0), &path).is_err());
    }
}

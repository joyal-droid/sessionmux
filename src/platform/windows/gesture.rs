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

//! The tamper gesture's window on Windows: a `WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_TOPMOST |
//! WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE` popup of `WIN_PX`² px (click-through, never activated),
//! pushed with `UpdateLayeredWindow(ULW_ALPHA)` like the bubble. A timer on the window steps
//! through `gesture::timeline`; the window is destroyed after the last step.

use std::cell::RefCell;
use std::os::windows::ffi::OsStrExt;
use std::path::Path;

use windows::core::PCWSTR;
use windows::Win32::Foundation::{
    GetLastError, COLORREF, ERROR_CLASS_ALREADY_EXISTS, GENERIC_WRITE, HANDLE, HWND, LPARAM, LRESULT, POINT, RECT,
    SIZE, WPARAM,
};
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GetDC, GetMonitorInfoW, MonitorFromPoint,
    MonitorFromRect, ReleaseDC, SelectObject, AC_SRC_ALPHA, AC_SRC_OVER, BITMAPINFO, BITMAPINFOHEADER, BI_RGB,
    BLENDFUNCTION, DIB_RGB_COLORS, HGDIOBJ, MONITORINFO, MONITOR_DEFAULTTONEAREST, MONITOR_DEFAULTTOPRIMARY,
};
use windows::Win32::Graphics::Imaging::{
    CLSID_WICImagingFactory, GUID_ContainerFormatPng, GUID_WICPixelFormat32bppBGRA, GUID_WICPixelFormat32bppPBGRA,
    IWICImagingFactory, IWICPalette, WICBitmapDitherTypeNone, WICBitmapEncoderNoCache, WICBitmapPaletteTypeCustom,
};
use windows::Win32::System::Com::{CoCreateInstance, StructuredStorage::IPropertyBag2, CLSCTX_INPROC_SERVER};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, KillTimer, RegisterClassExW, SetTimer, SetWindowPos, ShowWindow,
    UpdateLayeredWindow, HTTRANSPARENT, HWND_TOPMOST, MA_NOACTIVATE, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE,
    SW_SHOWNOACTIVATE, ULW_ALPHA, WM_MOUSEACTIVATE, WM_NCHITTEST, WM_TIMER, WNDCLASSEXW, WS_EX_LAYERED,
    WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
};

use crate::platform::windows::win;
use crate::platform::{GestureHost, Os, Rect};
use crate::ui::gesture::{pixel_scale, place, render, timeline, Step, FRAMES, GAP_PX, WIN_PX};
use crate::util::log;

/// Write the held (last) frame at 100 % as PNG (straight alpha) through the WIC encoder, as the
/// bubble does for its captures; no window is created.
fn write_png(path: &Path) -> Result<(), String> {
    let (px, side) = render(FRAMES.len() - 1, 1);
    let wpath: Vec<u16> = path.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
    // SAFETY: COM calls on this thread (initialised by main.rs); `px` and `wpath` outlive every
    // call that reads them.
    unsafe {
        let wic: IWICImagingFactory = CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)
            .map_err(|e| format!("WIC factory: {e}"))?;
        let src = wic
            .CreateBitmapFromMemory(side as u32, side as u32, &GUID_WICPixelFormat32bppPBGRA, (side * 4) as u32, &px)
            .map_err(|e| format!("WIC bitmap: {e}"))?;
        let conv = wic.CreateFormatConverter().map_err(|e| format!("WIC converter: {e}"))?;
        conv.Initialize(
            &src,
            &GUID_WICPixelFormat32bppBGRA,
            WICBitmapDitherTypeNone,
            None::<&IWICPalette>,
            0.0,
            WICBitmapPaletteTypeCustom,
        )
        .map_err(|e| format!("WIC convert: {e}"))?;
        let stream = wic.CreateStream().map_err(|e| format!("WIC stream: {e}"))?;
        stream
            .InitializeFromFilename(PCWSTR(wpath.as_ptr()), GENERIC_WRITE.0)
            .map_err(|e| format!("open {}: {e}", path.display()))?;
        let enc =
            wic.CreateEncoder(&GUID_ContainerFormatPng, std::ptr::null()).map_err(|e| format!("WIC encoder: {e}"))?;
        enc.Initialize(&stream, WICBitmapEncoderNoCache).map_err(|e| format!("encoder init: {e}"))?;
        let mut frame = None;
        let mut bag: Option<IPropertyBag2> = None;
        enc.CreateNewFrame(&mut frame, &mut bag).map_err(|e| format!("new frame: {e}"))?;
        let frame = frame.ok_or("new frame: null")?;
        frame.Initialize(bag.as_ref()).map_err(|e| format!("frame init: {e}"))?;
        frame.SetSize(side as u32, side as u32).map_err(|e| format!("frame size: {e}"))?;
        let mut fmt = GUID_WICPixelFormat32bppBGRA;
        frame.SetPixelFormat(&mut fmt).map_err(|e| format!("frame format: {e}"))?;
        frame.WriteSource(&conv, std::ptr::null()).map_err(|e| format!("write: {e}"))?;
        frame.Commit().map_err(|e| format!("frame commit: {e}"))?;
        enc.Commit().map_err(|e| format!("encoder commit: {e}"))?;
    }
    Ok(())
}

/// Copy premultiplied BGRA `px` (`side`² px) into a DIB and push it at (x, y) with `alpha`.
fn push(hwnd: HWND, px: &[u8], side: i32, x: i32, y: i32, alpha: u8) -> Result<(), String> {
    // SAFETY: standard GDI sequence; every handle created here is released on all paths, the
    // DIB bits pointer is only written while the DIB is alive and holds side*side*4 bytes.
    unsafe {
        let screen = GetDC(None);
        if screen.is_invalid() {
            return Err("GetDC failed".into());
        }
        let mem = CreateCompatibleDC(screen);
        if mem.is_invalid() {
            ReleaseDC(None, screen);
            return Err("CreateCompatibleDC failed".into());
        }
        let bi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: side,
                biHeight: -side, // top-down
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
        let dib = match CreateDIBSection(mem, &bi, DIB_RGB_COLORS, &mut bits, HANDLE::default(), 0) {
            Ok(b) if !bits.is_null() => b,
            Ok(b) => {
                let _ = DeleteObject(HGDIOBJ(b.0));
                let _ = DeleteDC(mem);
                ReleaseDC(None, screen);
                return Err("CreateDIBSection returned no bits".into());
            }
            Err(e) => {
                let _ = DeleteDC(mem);
                ReleaseDC(None, screen);
                return Err(format!("CreateDIBSection: {e}"));
            }
        };
        let len = (side as usize) * (side as usize) * 4;
        std::ptr::copy_nonoverlapping(px.as_ptr(), bits as *mut u8, len.min(px.len()));
        let old = SelectObject(mem, HGDIOBJ(dib.0));
        let blend = BLENDFUNCTION {
            BlendOp: AC_SRC_OVER as u8,
            BlendFlags: 0,
            SourceConstantAlpha: alpha,
            AlphaFormat: AC_SRC_ALPHA as u8,
        };
        let result = UpdateLayeredWindow(
            hwnd,
            screen,
            Some(&POINT { x, y }),
            Some(&SIZE { cx: side, cy: side }),
            mem,
            Some(&POINT { x: 0, y: 0 }),
            COLORREF(0),
            Some(&blend),
            ULW_ALPHA,
        )
        .map_err(|e| format!("UpdateLayeredWindow: {e}"));
        SelectObject(mem, old);
        let _ = DeleteObject(HGDIOBJ(dib.0));
        let _ = DeleteDC(mem);
        ReleaseDC(None, screen);
        result
    }
}

// -------------------------------------------------------------------------------- window

const CLASS_NAME: &str = "SessionmuxGesture";
const TIMER_ID: usize = 1;

struct Live {
    hwnd: HWND,
    steps: Vec<Step>,
    at: usize,
    k: i32,
    pos: (i32, i32),
}

thread_local! {
    static LIVE: RefCell<Option<Live>> = const { RefCell::new(None) };
}

/// Work area and pixel scale (effective DPI / 96) of the monitor the gesture appears on: the
/// bubble's monitor when there is an anchor, else the primary one.
fn work_area(anchor: Option<&Rect>) -> (Rect, f32) {
    let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
    // SAFETY: MonitorFrom* always return a monitor with these flags; `mi` is a sized out-struct.
    let (mon, ok) = unsafe {
        let mon = match anchor {
            Some(r) => MonitorFromRect(&RECT::from(*r), MONITOR_DEFAULTTONEAREST),
            None => MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY),
        };
        (mon, GetMonitorInfoW(mon, &mut mi).as_bool())
    };
    let area = if ok { Rect::from(mi.rcWork) } else { Rect { x: 0, y: 0, w: 1920, h: 1080 } };
    let (mut dx, mut dy) = (0u32, 0u32);
    // SAFETY: valid out-pointers; failure leaves 0 and the system DPI is used instead.
    let scale = match unsafe { GetDpiForMonitor(mon, MDT_EFFECTIVE_DPI, &mut dx, &mut dy) } {
        Ok(()) if dx > 0 => dx as f32 / 96.0,
        _ => win::dpi_scale(HWND::default()),
    };
    (area, scale)
}

fn register_class() -> Result<(), String> {
    let class = win::wide(CLASS_NAME);
    let wc = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        lpfnWndProc: Some(wndproc),
        hInstance: win::hinstance(),
        lpszClassName: PCWSTR(class.as_ptr()),
        ..Default::default()
    };
    // SAFETY: `wc` is fully initialised and `class` outlives the call.
    unsafe {
        if RegisterClassExW(&wc) == 0 && GetLastError() != ERROR_CLASS_ALREADY_EXISTS {
            return Err(format!("RegisterClassExW({CLASS_NAME}): {}", win::last_error()));
        }
    }
    Ok(())
}

/// Show the current step and arm the timer for the next one.
fn show_step(l: &Live) {
    let s = l.steps[l.at];
    let (px, side) = render(s.frame, l.k);
    if let Err(e) = push(l.hwnd, &px, side, l.pos.0, l.pos.1, s.alpha) {
        log::debug(&format!("gesture: {e}"));
    }
    // SAFETY: a timer on our own window, handled in wndproc; re-arming replaces it.
    unsafe { SetTimer(l.hwnd, TIMER_ID, s.ms, None) };
}

fn start(anchor: Option<Rect>) -> Result<(), String> {
    let (area, scale) = work_area(anchor.as_ref());
    let existing = LIVE.with(|l| l.borrow_mut().take().map(|l| l.hwnd));
    let hwnd = match existing {
        Some(h) => h,
        None => {
            register_class()?;
            let class = win::wide(CLASS_NAME);
            // SAFETY: class registered above; `class` outlives the call; no parent or params.
            unsafe {
                CreateWindowExW(
                    WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                    PCWSTR(class.as_ptr()),
                    PCWSTR(class.as_ptr()),
                    WS_POPUP,
                    0,
                    0,
                    WIN_PX,
                    WIN_PX,
                    None,
                    None,
                    win::hinstance(),
                    None,
                )
            }
            .map_err(|e| format!("CreateWindowExW({CLASS_NAME}): {e}"))?
        }
    };
    let k = pixel_scale(scale);
    let pos = place(anchor.as_ref(), &area, WIN_PX * k, GAP_PX * k);
    let live = Live { hwnd, steps: timeline(), at: 0, k, pos };
    show_step(&live);
    // SAFETY: show / z-order our own window without activating it.
    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        let _ = SetWindowPos(hwnd, HWND_TOPMOST, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
    }
    LIVE.with(|l| *l.borrow_mut() = Some(live));
    Ok(())
}

/// Advance one step; destroy the window after the last one.
fn tick() {
    let done = LIVE.with(|cell| {
        let mut g = cell.borrow_mut();
        let l = g.as_mut()?;
        l.at += 1;
        if l.at < l.steps.len() {
            show_step(l);
            None
        } else {
            g.take().map(|l| l.hwnd)
        }
    });
    if let Some(h) = done {
        destroy(h);
    }
}

fn destroy(hwnd: HWND) {
    // SAFETY: our own window, created on this thread; destroyed once (LIVE no longer holds it).
    unsafe {
        let _ = KillTimer(hwnd, TIMER_ID);
        let _ = DestroyWindow(hwnd);
    }
}

/// Whether the animation is currently on screen.
#[cfg(test)]
fn is_playing() -> bool {
    LIVE.with(|l| l.borrow().is_some())
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_TIMER if wp.0 == TIMER_ID => {
            tick();
            LRESULT(0)
        }
        WM_NCHITTEST => LRESULT(HTTRANSPARENT as isize),
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        // SAFETY: default processing for our own window with the original arguments.
        _ => unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
    }
}

impl GestureHost for Os {
    /// Returns at once; the window's timer drives the rest.
    fn play(anchor: Option<Rect>) {
        if let Err(e) = start(anchor) {
            log::debug(&format!("gesture: {e}"));
        }
    }

    fn close() {
        if let Some(l) = LIVE.with(|l| l.borrow_mut().take()) {
            destroy(l.hwnd);
        }
    }

    fn render_to_png(path: &Path) -> Result<(), String> {
        write_png(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::gesture::TOTAL_MS;
    use std::time::{Duration, Instant};

    #[test]
    fn work_area_of_the_primary_display() {
        let (area, scale) = work_area(None);
        assert!(area.w > 0 && area.h > 0);
        assert!(scale >= 1.0);
        let (near, _) = work_area(Some(&Rect { x: area.x + 10, y: area.y + 10, w: 80, h: 80 }));
        assert_eq!(near, area);
    }

    /// Plays the animation on screen for a manual look: `cargo test gesture_live_demo -- --ignored`.
    #[test]
    #[ignore]
    fn gesture_live_demo() {
        use windows::Win32::UI::WindowsAndMessaging::{DispatchMessageW, PeekMessageW, MSG, PM_REMOVE};
        <Os as GestureHost>::play(None);
        assert!(is_playing());
        let end = Instant::now() + Duration::from_millis(TOTAL_MS as u64 + 1000);
        let mut msg = MSG::default();
        while Instant::now() < end && is_playing() {
            // SAFETY: plain message pump on the thread that owns the gesture window.
            unsafe {
                while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                    DispatchMessageW(&msg);
                }
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(!is_playing());
    }
}

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

//! Tamper gesture: a small pixel-art hand that raises its middle finger beside the bubble.
//!
//! Purely cosmetic: it blocks nothing and changes nothing, and it has no setting. Only deliberate
//! tampering reaches it: `app.rs` decides when to play it (debugger attached after
//! [`START_GRACE`], forged task message, a `--relay` command line showing argument injection),
//! gates it with [`armed`], rate-limits it with [`Limiter`] and logs `tamper: <reason>`.
//!
//! Display: a `WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_TOPMOST | WS_EX_TOOLWINDOW |
//! WS_EX_NOACTIVATE` popup of [`WIN_PX`]² px (click-through, never activated), pushed with
//! `UpdateLayeredWindow(ULW_ALPHA)` like the bubble. The art is six `const` grids of
//! [`COLS`]×[`ROWS`] cells, [`CELL_PX`] px per cell, drawn without any anti-aliasing: hand
//! `#E3B58A`, outline `#101114` and a flat 25 % black drop shadow one cell down and right.
//! Timeline ([`timeline`]): five frames of 90 ms, the last frame for 90 + 1500 ms, then six
//! 50 ms fade steps; the window is destroyed afterwards. Playing again while visible restarts.

use std::cell::RefCell;
use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use std::time::{Duration, Instant};

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

use crate::util::{log, win};

/// Cells per row of a frame grid.
pub const COLS: usize = 16;
/// Rows per frame grid.
pub const ROWS: usize = 20;
/// Pixels per cell (at 100 % scaling).
pub const CELL_PX: i32 = 8;
/// Window width and height (at 100 % scaling).
pub const WIN_PX: i32 = 176;
/// Gap between the bubble and the gesture window (at 100 % scaling).
pub const GAP_PX: i32 = 12;
/// Hand colour `#E3B58A`.
pub const HAND: u32 = 0xE3B58A;
/// Outline colour `#101114`.
pub const OUTLINE: u32 = 0x101114;
/// Drop shadow: black at 25 % (alpha 64 of 255).
pub const SHADOW_ALPHA: u8 = 64;

pub const FRAME_MS: u32 = 90;
pub const HOLD_MS: u32 = 1500;
pub const FADE_MS: u32 = 300;
pub const FADE_STEPS: u32 = 6;
/// Whole animation: 6 × 90 + 1500 + 300 = 2340 ms.
pub const TOTAL_MS: u32 = FRAMES.len() as u32 * FRAME_MS + HOLD_MS + FADE_MS;

/// Minimum time between two triggers that are acted on.
pub const RATE_LIMIT: Duration = Duration::from_secs(10);

/// Cell values: `.` transparent, `#` outline, `o` hand.
pub const FRAMES: [[&str; ROWS]; 6] = [
    // 0: closed fist
    [
        "................",
        "................",
        "................",
        "................",
        "................",
        "................",
        "................",
        "......###.......",
        "..####ooo####...",
        ".#ooo#ooo#ooo##.",
        ".#ooo#ooo#ooo#o#",
        "##ooo#ooo#ooo#o#",
        "#o#oooooooooo#o#",
        "#oo#ooooooooooo#",
        "#ooo#oooooooooo#",
        ".#ooo#ooooooooo#",
        "..#oooooooooo##.",
        "...#oooooooo#...",
        "...#oooooooo#...",
        "...##########...",
    ],
    // 1: the finger starts to rise
    [
        "................",
        "................",
        "................",
        "................",
        "................",
        "................",
        "......###.......",
        ".....#ooo#......",
        "..####ooo####...",
        ".#ooo#ooo#ooo##.",
        ".#ooo#ooo#ooo#o#",
        "##ooo#ooo#ooo#o#",
        "#o#oooooooooo#o#",
        "#oo#ooooooooooo#",
        "#ooo#oooooooooo#",
        ".#ooo#ooooooooo#",
        "..#oooooooooo##.",
        "...#oooooooo#...",
        "...#oooooooo#...",
        "...##########...",
    ],
    // 2: half way
    [
        "................",
        "................",
        "................",
        "................",
        "......###.......",
        ".....#ooo#......",
        ".....#ooo#......",
        ".....#ooo#......",
        "..####ooo####...",
        ".#ooo#ooo#ooo##.",
        ".#ooo#ooo#ooo#o#",
        "##ooo#ooo#ooo#o#",
        "#o#oooooooooo#o#",
        "#oo#ooooooooooo#",
        "#ooo#oooooooooo#",
        ".#ooo#ooooooooo#",
        "..#oooooooooo##.",
        "...#oooooooo#...",
        "...#oooooooo#...",
        "...##########...",
    ],
    // 3: nearly up
    [
        "................",
        "................",
        "......###.......",
        ".....#ooo#......",
        ".....#ooo#......",
        ".....#ooo#......",
        ".....#ooo#......",
        ".....#ooo#......",
        "..####ooo####...",
        ".#ooo#ooo#ooo##.",
        ".#ooo#ooo#ooo#o#",
        "##ooo#ooo#ooo#o#",
        "#o#oooooooooo#o#",
        "#oo#ooooooooooo#",
        "#ooo#oooooooooo#",
        ".#ooo#ooooooooo#",
        "..#oooooooooo##.",
        "...#oooooooo#...",
        "...#oooooooo#...",
        "...##########...",
    ],
    // 4: full gesture, stretched one cell
    [
        "......###.......",
        ".....#ooo#......",
        ".....#ooo#......",
        ".....#ooo#......",
        ".....#ooo#......",
        ".....#ooo#......",
        ".....#ooo#......",
        ".....#ooo#......",
        "..####ooo####...",
        ".#ooo#ooo#ooo##.",
        ".#ooo#ooo#ooo#o#",
        "##ooo#ooo#ooo#o#",
        "#o#oooooooooo#o#",
        "#oo#ooooooooooo#",
        "#ooo#oooooooooo#",
        ".#ooo#ooooooooo#",
        "..#oooooooooo##.",
        "...#oooooooo#...",
        "...#oooooooo#...",
        "...##########...",
    ],
    // 5: full gesture, settled (held)
    [
        "................",
        "......###.......",
        ".....#ooo#......",
        ".....#ooo#......",
        ".....#ooo#......",
        ".....#ooo#......",
        ".....#ooo#......",
        ".....#ooo#......",
        "..####ooo####...",
        ".#ooo#ooo#ooo##.",
        ".#ooo#ooo#ooo#o#",
        "##ooo#ooo#ooo#o#",
        "#o#oooooooooo#o#",
        "#oo#ooooooooooo#",
        "#ooo#oooooooooo#",
        ".#ooo#ooooooooo#",
        "..#oooooooooo##.",
        "...#oooooooo#...",
        "...#oooooooo#...",
        "...##########...",
    ],
];

// ------------------------------------------------------------------------------ triggers

/// Why the gesture played (the word after `tamper:` in the log).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reason {
    Debugger,
    ForgedMessage,
    BadLink,
}

impl Reason {
    pub fn as_str(self) -> &'static str {
        match self {
            Reason::Debugger => "debugger",
            Reason::ForgedMessage => "forged-message",
            Reason::BadLink => "bad-link",
        }
    }
}

/// A debugger seen this soon after start is ignored (Windows Error Reporting attaches to a
/// process that is crashing, when no gesture matters).
pub const START_GRACE: Duration = Duration::from_secs(3);

/// Whether a trigger may play at all: never in smoke mode, never while the process is exiting,
/// and a debugger only once [`START_GRACE`] has passed since start.
pub fn armed(reason: Reason, since_start: Duration, smoke: bool, exiting: bool) -> bool {
    if smoke || exiting {
        return false;
    }
    reason != Reason::Debugger || since_start >= START_GRACE
}

/// At most one accepted trigger per [`RATE_LIMIT`].
#[derive(Debug, Default)]
pub struct Limiter {
    last: Option<Instant>,
}

impl Limiter {
    /// True (and remembers `now`) when the previous accepted trigger is at least 10 s old.
    pub fn allow(&mut self, now: Instant) -> bool {
        if self.last.is_some_and(|t| now.saturating_duration_since(t) < RATE_LIMIT) {
            return false;
        }
        self.last = Some(now);
        true
    }
}

/// Edge detector for the debugger check: fires once per attach, re-arms on detach.
#[derive(Debug, Default)]
pub struct DebugWatch {
    attached: bool,
}

impl DebugWatch {
    /// Feed the current state; true exactly when a debugger has just attached.
    pub fn update(&mut self, present: bool) -> bool {
        let fire = present && !self.attached;
        self.attached = present;
        fire
    }
}

/// True while a debugger is attached to this process (local or remote).
pub fn debugger_present() -> bool {
    use windows::Win32::Foundation::BOOL;
    use windows::Win32::System::Diagnostics::Debug::{CheckRemoteDebuggerPresent, IsDebuggerPresent};
    use windows::Win32::System::Threading::GetCurrentProcess;
    // SAFETY: both calls only read process state; `remote` is a valid out-pointer and the
    // pseudo-handle from GetCurrentProcess needs no closing.
    unsafe {
        if IsDebuggerPresent().as_bool() {
            return true;
        }
        let mut remote = BOOL(0);
        CheckRemoteDebuggerPresent(GetCurrentProcess(), &mut remote).is_ok() && remote.as_bool()
    }
}

// ------------------------------------------------------------------------------ timeline

/// One shown state of the animation: which frame, its window alpha, and for how long.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Step {
    pub frame: usize,
    pub alpha: u8,
    pub ms: u32,
}

/// The whole animation; the window is destroyed when the last step's time is up.
pub fn timeline() -> Vec<Step> {
    let last = FRAMES.len() - 1;
    let mut steps: Vec<Step> = (0..FRAMES.len())
        .map(|frame| Step { frame, alpha: 255, ms: if frame == last { FRAME_MS + HOLD_MS } else { FRAME_MS } })
        .collect();
    for i in 1..=FADE_STEPS {
        let alpha = (255 * (FADE_STEPS - i) / FADE_STEPS) as u8;
        steps.push(Step { frame: last, alpha, ms: FADE_MS / FADE_STEPS });
    }
    debug_assert_eq!(steps.iter().map(|s| s.ms).sum::<u32>(), TOTAL_MS);
    steps
}

// ----------------------------------------------------------------------------- rendering

/// Opaque premultiplied BGRA of `0xRRGGBB`.
fn bgra(hex: u32) -> [u8; 4] {
    [(hex & 0xFF) as u8, ((hex >> 8) & 0xFF) as u8, ((hex >> 16) & 0xFF) as u8, 0xFF]
}

/// Integer pixel multiplier for a monitor scale (keeps every cell a crisp square).
pub fn pixel_scale(scale: f32) -> i32 {
    (scale.floor() as i32).max(1)
}

/// Render `frame` as premultiplied top-down BGRA, `WIN_PX * k` px square. Returns (pixels, side).
pub fn render(frame: usize, k: i32) -> (Vec<u8>, i32) {
    let side = WIN_PX * k;
    let cell = CELL_PX * k;
    let mut px = vec![0u8; (side * side * 4) as usize];
    let grid = &FRAMES[frame.min(FRAMES.len() - 1)];
    // Centre the art plus its one-cell shadow offset.
    let ox = (side - COLS as i32 * cell - cell) / 2;
    let oy = (side - ROWS as i32 * cell - cell) / 2;
    let mut fill = |cx: i32, cy: i32, color: [u8; 4]| {
        for y in cy.max(0)..(cy + cell).min(side) {
            for x in cx.max(0)..(cx + cell).min(side) {
                let i = ((y * side + x) * 4) as usize;
                px[i..i + 4].copy_from_slice(&color);
            }
        }
    };
    // Shadow first (the whole silhouette one cell down-right), then the art on top.
    for shadow in [true, false] {
        let off = if shadow { cell } else { 0 };
        for (r, row) in grid.iter().enumerate() {
            for (c, ch) in row.bytes().enumerate() {
                let color = match (ch, shadow) {
                    (b'.', _) => continue,
                    (_, true) => [0, 0, 0, SHADOW_ALPHA],
                    (b'#', false) => bgra(OUTLINE),
                    (_, false) => bgra(HAND),
                };
                fill(ox + c as i32 * cell + off, oy + r as i32 * cell + off, color);
            }
        }
    }
    (px, side)
}

/// Write the held (last) frame at 100 % as PNG (straight alpha) through the WIC encoder, as the
/// bubble does for its captures; no window is created.
pub fn render_to_png(path: &Path) -> Result<(), String> {
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
fn work_area(anchor: Option<&RECT>) -> (RECT, f32) {
    let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
    // SAFETY: MonitorFrom* always return a monitor with these flags; `mi` is a sized out-struct.
    let (mon, ok) = unsafe {
        let mon = match anchor {
            Some(r) => MonitorFromRect(r, MONITOR_DEFAULTTONEAREST),
            None => MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY),
        };
        (mon, GetMonitorInfoW(mon, &mut mi).as_bool())
    };
    let area = if ok { mi.rcWork } else { RECT { left: 0, top: 0, right: 1920, bottom: 1080 } };
    let (mut dx, mut dy) = (0u32, 0u32);
    // SAFETY: valid out-pointers; failure leaves 0 and the system DPI is used instead.
    let scale = match unsafe { GetDpiForMonitor(mon, MDT_EFFECTIVE_DPI, &mut dx, &mut dy) } {
        Ok(()) if dx > 0 => dx as f32 / 96.0,
        _ => win::dpi_scale(HWND::default()),
    };
    (area, scale)
}

/// Top-left of a `side`² window `gap` px beside `anchor` (left of it when there is room, else
/// right), bottom-aligned with it and kept inside `area`; without an anchor, the bottom-right
/// corner of `area`.
pub fn place(anchor: Option<&RECT>, area: &RECT, side: i32, gap: i32) -> (i32, i32) {
    let clamp = |v: i32, lo: i32, hi: i32| if hi < lo { lo } else { v.clamp(lo, hi) };
    let Some(a) = anchor else {
        return (area.right - side - gap, area.bottom - side - gap);
    };
    let left = a.left - gap - side;
    let x = if left >= area.left { left } else { a.right + gap };
    let y = a.bottom - side;
    (clamp(x, area.left, area.right - side), clamp(y, area.top, area.bottom - side))
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

/// Play the animation beside `anchor` (the bubble's rect) or in the bottom-right corner of the
/// primary work area. Restarts it when already playing. Returns at once; a timer drives the rest.
pub fn play(anchor: Option<RECT>) {
    if let Err(e) = start(anchor) {
        log::debug(&format!("gesture: {e}"));
    }
}

fn start(anchor: Option<RECT>) -> Result<(), String> {
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

/// Close the window now if it is showing (shutdown).
pub fn close() {
    if let Some(l) = LIVE.with(|l| l.borrow_mut().take()) {
        destroy(l.hwnd);
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

// ----------------------------------------------------------------------------------- tests

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_grids_are_well_formed() {
        assert_eq!(FRAMES.len(), 6);
        for (i, f) in FRAMES.iter().enumerate() {
            assert_eq!(f.len(), ROWS, "frame {i} rows");
            for (r, row) in f.iter().enumerate() {
                assert_eq!(row.len(), COLS, "frame {i} row {r} has {} cells", row.len());
                assert!(row.bytes().all(|b| matches!(b, b'.' | b'#' | b'o')), "frame {i} row {r}: {row}");
            }
        }
        // The fist is the same in every frame; only the middle finger moves.
        for f in &FRAMES {
            assert_eq!(f[9..], FRAMES[5][9..]);
        }
        // Closed fist, then the finger rises to the top, overshoots one cell and settles.
        let top = |f: &[&str; ROWS]| f.iter().position(|r| r.contains('#')).unwrap();
        let tops: Vec<usize> = FRAMES.iter().map(top).collect();
        assert_eq!(tops, [7, 6, 4, 2, 0, 1]);
    }

    #[test]
    fn timing_table_sums_to_the_documented_duration() {
        let t = timeline();
        assert_eq!(TOTAL_MS, 2340);
        assert_eq!(t.iter().map(|s| s.ms).sum::<u32>(), TOTAL_MS);
        assert_eq!(t.len(), FRAMES.len() + FADE_STEPS as usize);
        assert!(t[..5].iter().enumerate().all(|(i, s)| s.frame == i && s.ms == FRAME_MS && s.alpha == 255));
        assert_eq!(t[5], Step { frame: 5, alpha: 255, ms: FRAME_MS + HOLD_MS });
        let fade: Vec<u8> = t[6..].iter().map(|s| s.alpha).collect();
        assert_eq!(fade, [212, 170, 127, 85, 42, 0]);
        assert!(t[6..].iter().all(|s| s.frame == 5 && s.ms == 50));
    }

    #[test]
    fn render_is_crisp_and_uses_only_the_palette() {
        let (px, side) = render(5, 1);
        assert_eq!(side, WIN_PX);
        let ok = [[0, 0, 0, 0], [0, 0, 0, SHADOW_ALPHA], bgra(HAND), bgra(OUTLINE)];
        for p in px.chunks(4) {
            assert!(ok.contains(&[p[0], p[1], p[2], p[3]]), "unexpected pixel {p:?}");
        }
        assert!(px.chunks(4).any(|p| p == bgra(HAND)));
        assert!(px.chunks(4).any(|p| p == [0, 0, 0, SHADOW_ALPHA]));
        // 2x scaling doubles every pixel exactly.
        let (px2, side2) = render(5, 2);
        assert_eq!(side2, 2 * WIN_PX);
        for y in 0..side {
            for x in 0..side {
                let a = &px[((y * side + x) * 4) as usize..][..4];
                let b = &px2[(((2 * y) * side2 + 2 * x) * 4) as usize..][..4];
                assert_eq!(a, b);
            }
        }
        assert_eq!(pixel_scale(1.0), 1);
        assert_eq!(pixel_scale(1.5), 1);
        assert_eq!(pixel_scale(2.0), 2);
    }

    #[test]
    fn armed_rules() {
        let late = START_GRACE + Duration::from_secs(1);
        for r in [Reason::Debugger, Reason::ForgedMessage, Reason::BadLink] {
            assert!(armed(r, late, false, false), "{r:?}");
            assert!(!armed(r, late, true, false), "{r:?} in smoke mode");
            assert!(!armed(r, late, false, true), "{r:?} while exiting");
        }
        assert!(!armed(Reason::Debugger, Duration::ZERO, false, false));
        assert!(!armed(Reason::Debugger, START_GRACE - Duration::from_millis(1), false, false));
        assert!(armed(Reason::Debugger, START_GRACE, false, false));
        assert!(armed(Reason::ForgedMessage, Duration::ZERO, false, false));
        assert!(armed(Reason::BadLink, Duration::ZERO, false, false));
    }

    #[test]
    fn limiter_and_debug_watch() {
        let t0 = Instant::now();
        let mut l = Limiter::default();
        assert!(l.allow(t0));
        assert!(!l.allow(t0 + Duration::from_secs(9)));
        assert!(l.allow(t0 + Duration::from_secs(10)));
        let mut d = DebugWatch::default();
        assert!(!d.update(false));
        assert!(d.update(true));
        assert!(!d.update(true));
        assert!(!d.update(false));
        assert!(d.update(true));
        assert_eq!(Reason::ForgedMessage.as_str(), "forged-message");
        assert_eq!(Reason::BadLink.as_str(), "bad-link");
        assert_eq!(Reason::Debugger.as_str(), "debugger");
    }

    #[test]
    fn placement() {
        let area = RECT { left: 0, top: 0, right: 1920, bottom: 1040 };
        // Bubble on the right edge: the gesture goes to its left, bottom-aligned.
        let b = RECT { left: 1800, top: 900, right: 1880, bottom: 980 };
        assert_eq!(place(Some(&b), &area, 176, 12), (1800 - 12 - 176, 980 - 176));
        // Bubble on the left edge: to its right, kept inside the work area.
        let b = RECT { left: 10, top: 20, right: 90, bottom: 100 };
        assert_eq!(place(Some(&b), &area, 176, 12), (102, 0));
        // No bubble: bottom-right corner.
        assert_eq!(place(None, &area, 176, 12), (1920 - 188, 1040 - 188));
    }

    /// Plays the animation on screen for a manual look: `cargo test gesture_live_demo -- --ignored`.
    #[test]
    #[ignore]
    fn gesture_live_demo() {
        use windows::Win32::UI::WindowsAndMessaging::{DispatchMessageW, PeekMessageW, MSG, PM_REMOVE};
        play(None);
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

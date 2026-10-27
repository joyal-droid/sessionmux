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

//! The floating bubble's window on Windows — a layered window drawn with Direct2D, no WebView.
//!
//! - `WS_EX_LAYERED|WS_EX_TOPMOST|WS_EX_TOOLWINDOW|WS_EX_NOACTIVATE` popup of class `SessionmuxBubble`.
//!   Every frame is the scene `bubble::model::scene` builds, drawn into a premultiplied 32bpp PBGRA
//!   `IWICBitmap` through a Direct2D WIC render target whose DPI is set to the monitor DPI (so all
//!   drawing is in DIPs), copied into a top-down DIB section and pushed with
//!   `UpdateLayeredWindow(ULW_ALPHA)`. Fully transparent pixels are click-through.
//! - Positions passed in / out (`create`, `set_position`, `DragEnd`) are physical
//!   screen pixels (the process is Per-Monitor-V2 aware).
//! - `create` shows the window (no activation).
//! - COM must already be initialised on the calling (UI) thread (main.rs does it).
//! - The event callback runs after all internal borrows are released, so it may call back into
//!   the bubble (e.g. `set_state`) or even drop it.

use std::cell::{Cell, RefCell};
use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use std::rc::Rc;

use windows::core::{w, Interface, PCWSTR};
use windows::Win32::Foundation::{
    GetLastError, COLORREF, ERROR_CLASS_ALREADY_EXISTS, GENERIC_WRITE, HANDLE, HWND, LPARAM, LRESULT, POINT, RECT,
    SIZE, WPARAM,
};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_PIXEL_FORMAT, D2D_POINT_2F, D2D_RECT_F,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1CreateFactory, ID2D1Factory, ID2D1RenderTarget, ID2D1SolidColorBrush, D2D1_ANTIALIAS_MODE_PER_PRIMITIVE,
    D2D1_DRAW_TEXT_OPTIONS_NONE, D2D1_ELLIPSE, D2D1_FACTORY_TYPE_SINGLE_THREADED, D2D1_FEATURE_LEVEL_DEFAULT,
    D2D1_RENDER_TARGET_PROPERTIES, D2D1_RENDER_TARGET_TYPE_DEFAULT, D2D1_RENDER_TARGET_USAGE_NONE, D2D1_ROUNDED_RECT,
    D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE,
};
use windows::Win32::Graphics::DirectWrite::{
    DWriteCreateFactory, IDWriteFactory, IDWriteFontCollection, IDWriteTextLayout1, DWRITE_FACTORY_TYPE_SHARED,
    DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_PARAGRAPH_ALIGNMENT_CENTER,
    DWRITE_TEXT_ALIGNMENT_CENTER, DWRITE_TEXT_RANGE, DWRITE_WORD_WRAPPING_NO_WRAP,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GetDC, GetMonitorInfoW, MonitorFromPoint,
    MonitorFromRect, ReleaseDC, SelectObject, AC_SRC_ALPHA, AC_SRC_OVER, BITMAPINFO, BITMAPINFOHEADER, BI_RGB,
    BLENDFUNCTION, DIB_RGB_COLORS, HGDIOBJ, MONITORINFO, MONITOR_DEFAULTTONEAREST, MONITOR_DEFAULTTOPRIMARY,
};
use windows::Win32::Graphics::Imaging::{
    CLSID_WICImagingFactory, GUID_ContainerFormatPng, GUID_WICPixelFormat32bppBGRA, GUID_WICPixelFormat32bppPBGRA,
    IWICBitmap, IWICImagingFactory, IWICPalette, WICBitmapCacheOnLoad, WICBitmapDitherTypeNone,
    WICBitmapEncoderNoCache, WICBitmapPaletteTypeCustom,
};
use windows::Win32::System::Com::{CoCreateInstance, StructuredStorage::IPropertyBag2, CLSCTX_INPROC_SERVER};
use windows::Win32::UI::Controls::WM_MOUSELEAVE;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetCapture, ReleaseCapture, SetCapture, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetCursorPos, GetWindowLongPtrW, GetWindowRect, IsWindowVisible,
    LoadCursorW, RegisterClassExW, SetCursor, SetWindowLongPtrW, SetWindowPos, ShowWindow, UpdateLayeredWindow,
    GWLP_USERDATA, HTCLIENT, HWND_TOPMOST, IDC_ARROW, IDC_HAND, MA_NOACTIVATE, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE,
    SWP_NOZORDER, SW_HIDE, SW_SHOWNOACTIVATE, ULW_ALPHA, WM_CAPTURECHANGED, WM_DISPLAYCHANGE, WM_DPICHANGED,
    WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEACTIVATE, WM_MOUSEMOVE, WM_NCDESTROY, WM_RBUTTONUP, WM_SETCURSOR,
    WNDCLASSEXW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};

use crate::brand;
use crate::platform::windows::win;
use crate::platform::{BubbleHost, Rect};
use crate::ui::bubble::model::{
    self, anchored_rect, clamp_to_area, effective_mode, hit_test, px_size, snap_rect, Hit, Visual, DEFAULT_MARGIN_DIP,
    DRAG_THRESHOLD_DIP, EDGE_MARGIN_DIP, MAX_TRAY,
};
use crate::ui::bubble::scene::{Color, Prim, Scene, LETTER_SPACING_EM, TEXT_SHADOW_DY};
use crate::ui::bubble::{BubbleEvent, BubbleMode, BubbleState};
use crate::util::log;

const CLASS_NAME: &str = brand::CLASS_BUBBLE;

/// Work area of the monitor nearest `r`.
fn work_area_for(r: &Rect) -> Rect {
    let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
    let rc = RECT::from(*r);
    // SAFETY: `rc` and `mi` are valid for the duration of the calls; MonitorFromRect with
    // DEFAULTTONEAREST always returns a monitor; GetMonitorInfoW only writes into `mi`.
    let ok = unsafe {
        let mon = MonitorFromRect(&rc, MONITOR_DEFAULTTONEAREST);
        GetMonitorInfoW(mon, &mut mi).as_bool()
    };
    if ok {
        Rect::from(mi.rcWork)
    } else {
        Rect { x: 0, y: 0, w: 1920, h: 1080 }
    }
}

fn primary_work_area() -> Rect {
    let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
    // SAFETY: as in work_area_for; MONITOR_DEFAULTTOPRIMARY always yields a monitor.
    let ok = unsafe {
        let mon = MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY);
        GetMonitorInfoW(mon, &mut mi).as_bool()
    };
    if ok {
        Rect::from(mi.rcWork)
    } else {
        Rect { x: 0, y: 0, w: 1920, h: 1080 }
    }
}

// ------------------------------------------------------------------------------ rendering

/// Device-independent COM factories (created once per bubble).
struct Gfx {
    d2d: ID2D1Factory,
    dwrite: IDWriteFactory,
    wic: IWICImagingFactory,
    /// NUL-terminated family name ("Segoe UI Variable Text" or "Segoe UI").
    family: Vec<u16>,
}

impl Gfx {
    fn new() -> Result<Gfx, String> {
        // SAFETY: plain factory constructors; COM is initialised on this thread by main.rs
        // (CoCreateInstance fails cleanly with CO_E_NOTINITIALIZED otherwise).
        unsafe {
            let d2d: ID2D1Factory = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)
                .map_err(|e| format!("D2D1CreateFactory: {e}"))?;
            let dwrite: IDWriteFactory =
                DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED).map_err(|e| format!("DWriteCreateFactory: {e}"))?;
            let wic: IWICImagingFactory = CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)
                .map_err(|e| format!("WIC factory: {e}"))?;
            let family = pick_family(&dwrite);
            Ok(Gfx { d2d, dwrite, wic, family })
        }
    }
}

fn pick_family(dwrite: &IDWriteFactory) -> Vec<u16> {
    let mut coll: Option<IDWriteFontCollection> = None;
    // SAFETY: out-pointer is a valid Option<IDWriteFontCollection>.
    let got = unsafe { dwrite.GetSystemFontCollection(&mut coll, false) };
    if let (Ok(()), Some(coll)) = (got, coll) {
        for name in ["Segoe UI Variable Text", "Segoe UI"] {
            let wname = win::wide(name);
            let mut idx = 0u32;
            let mut exists = windows::Win32::Foundation::BOOL(0);
            // SAFETY: wname is NUL-terminated and outlives the call; out-pointers are valid.
            let r = unsafe { coll.FindFamilyName(PCWSTR(wname.as_ptr()), &mut idx, &mut exists) };
            if r.is_ok() && exists.as_bool() {
                return wname;
            }
        }
    }
    win::wide("Segoe UI")
}

fn d2d(c: Color) -> D2D1_COLOR_F {
    let (r, g, b) = c.channels();
    D2D1_COLOR_F { r, g, b, a: c.a }
}

struct Painter<'a> {
    gfx: &'a Gfx,
    rt: &'a ID2D1RenderTarget,
    brush: ID2D1SolidColorBrush,
}

impl<'a> Painter<'a> {
    fn circle(&self, cx: f32, cy: f32, r: f32, color: D2D1_COLOR_F) {
        if r <= 0.0 {
            return;
        }
        let e = D2D1_ELLIPSE { point: D2D_POINT_2F { x: cx, y: cy }, radiusX: r, radiusY: r };
        // SAFETY: the brush belongs to this render target; pointers are to locals.
        unsafe {
            self.brush.SetColor(&color);
            self.rt.FillEllipse(&e, &self.brush);
        }
    }

    /// Ring centred on radius `r` with stroke width `w`.
    fn ring(&self, cx: f32, cy: f32, r: f32, w: f32, color: D2D1_COLOR_F) {
        let e = D2D1_ELLIPSE { point: D2D_POINT_2F { x: cx, y: cy }, radiusX: r, radiusY: r };
        // SAFETY: as in circle(); a null stroke style is allowed.
        unsafe {
            self.brush.SetColor(&color);
            self.rt.DrawEllipse(&e, &self.brush, w, None);
        }
    }

    fn rrect(&self, l: f32, t: f32, r: f32, b: f32, radius: f32, color: D2D1_COLOR_F) {
        let rr = D2D1_ROUNDED_RECT {
            rect: D2D_RECT_F { left: l, top: t, right: r, bottom: b },
            radiusX: radius,
            radiusY: radius,
        };
        // SAFETY: as in circle().
        unsafe {
            self.brush.SetColor(&color);
            self.rt.FillRoundedRectangle(&rr, &self.brush);
        }
    }

    fn rrect_stroke(&self, l: f32, t: f32, r: f32, b: f32, radius: f32, w: f32, color: D2D1_COLOR_F) {
        let rr = D2D1_ROUNDED_RECT {
            rect: D2D_RECT_F { left: l, top: t, right: r, bottom: b },
            radiusX: radius,
            radiusY: radius,
        };
        // SAFETY: as in circle().
        unsafe {
            self.brush.SetColor(&color);
            self.rt.DrawRoundedRectangle(&rr, &self.brush, w, None);
        }
    }

    /// Text centred on (cx, cy) with the scene's letter spacing, over its shadow.
    fn text(
        &self,
        text: &str,
        size: f32,
        cx: f32,
        cy: f32,
        color: D2D1_COLOR_F,
        shadow: Option<D2D1_COLOR_F>,
    ) -> windows::core::Result<()> {
        let wtext: Vec<u16> = text.encode_utf16().collect();
        if wtext.is_empty() {
            return Ok(());
        }
        // SAFETY: the family buffer is NUL-terminated and owned by Gfx; all interfaces are
        // used on this thread only; text slices outlive the calls.
        unsafe {
            let fmt = self.gfx.dwrite.CreateTextFormat(
                PCWSTR(self.gfx.family.as_ptr()),
                None::<&IDWriteFontCollection>,
                DWRITE_FONT_WEIGHT_NORMAL,
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                size,
                w!("en-us"),
            )?;
            fmt.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER)?;
            fmt.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
            fmt.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
            let bx = size * 4.0;
            let layout = self.gfx.dwrite.CreateTextLayout(&wtext, &fmt, bx, bx)?;
            let spacing = LETTER_SPACING_EM * size;
            if let Ok(l1) = layout.cast::<IDWriteTextLayout1>() {
                let range = DWRITE_TEXT_RANGE { startPosition: 0, length: wtext.len() as u32 };
                let _ = l1.SetCharacterSpacing(0.0, spacing, 0.0, range);
            }
            // Chromium also adds the letter spacing after the last glyph; keep that offset.
            let ox = cx - bx / 2.0;
            let oy = cy - bx / 2.0;
            if let Some(s) = shadow {
                self.brush.SetColor(&s);
                self.rt.DrawTextLayout(
                    D2D_POINT_2F { x: ox, y: oy + TEXT_SHADOW_DY },
                    &layout,
                    &self.brush,
                    D2D1_DRAW_TEXT_OPTIONS_NONE,
                );
            }
            self.brush.SetColor(&color);
            self.rt.DrawTextLayout(D2D_POINT_2F { x: ox, y: oy }, &layout, &self.brush, D2D1_DRAW_TEXT_OPTIONS_NONE);
        }
        Ok(())
    }

    /// Draw every primitive in order; a text that cannot be laid out stops the frame.
    fn draw(&self, scene: &Scene) -> windows::core::Result<()> {
        for p in &scene.prims {
            match p {
                Prim::Circle { cx, cy, r, color } => self.circle(*cx, *cy, *r, d2d(*color)),
                Prim::Ring { cx, cy, r, w, color } => self.ring(*cx, *cy, *r, *w, d2d(*color)),
                Prim::RoundRect { l, t, r, b, radius, color } => self.rrect(*l, *t, *r, *b, *radius, d2d(*color)),
                Prim::RoundRectStroke { l, t, r, b, radius, w, color } => {
                    self.rrect_stroke(*l, *t, *r, *b, *radius, *w, d2d(*color))
                }
                Prim::Text { text, size, cx, cy, color, shadow } => {
                    self.text(text, *size, *cx, *cy, d2d(*color), shadow.map(d2d))?
                }
            }
        }
        Ok(())
    }
}

/// `ID2D1RenderTarget::CreateSolidColorBrush`. The windows-rs wrapper is gated behind the
/// `Foundation_Numerics` feature (not in our fixed Cargo.toml), so call the COM vtable slot
/// directly: IUnknown(3) + ID2D1Resource::GetFactory(1) + CreateBitmap, CreateBitmapFromWicBitmap,
/// CreateSharedBitmap, CreateBitmapBrush → CreateSolidColorBrush is slot 8 (d2d1.h order).
fn create_solid_brush(rt: &ID2D1RenderTarget, color: &D2D1_COLOR_F) -> windows::core::Result<ID2D1SolidColorBrush> {
    type CreateBrushFn = unsafe extern "system" fn(
        *mut core::ffi::c_void,
        *const D2D1_COLOR_F,
        *const core::ffi::c_void,
        *mut *mut core::ffi::c_void,
    ) -> windows::core::HRESULT;
    // SAFETY: `rt` is a live ID2D1RenderTarget; the first pointer-sized field of every COM object
    // is its vtable, whose layout is fixed by the ABI (slot 8 = CreateSolidColorBrush, verified
    // against the windows-rs ID2D1RenderTarget_Vtbl). A null brush-properties pointer is allowed;
    // on success the out pointer holds an owned reference that from_raw adopts.
    unsafe {
        let this = rt.as_raw();
        let vtbl = *(this as *const *const usize);
        let f: CreateBrushFn = std::mem::transmute(*vtbl.add(8));
        let mut out: *mut core::ffi::c_void = std::ptr::null_mut();
        f(this, color, std::ptr::null(), &mut out).ok()?;
        if out.is_null() {
            return Err(windows::core::Error::from(windows::Win32::Foundation::E_POINTER));
        }
        Ok(ID2D1SolidColorBrush::from_raw(out))
    }
}

/// Draw the scene of `st` and `v` into a new premultiplied BGRA WIC bitmap of `w`×`h` px at
/// `scale`. `bg` paints an opaque background (test previews only).
fn paint(
    gfx: &Gfx,
    st: &BubbleState,
    v: Visual,
    scale: f32,
    bg: Option<u32>,
) -> Result<(IWICBitmap, i32, i32), String> {
    let scene = model::scene(st, v);
    let (w, h) = px_size(effective_mode(st), st.accounts.len(), scale);
    // SAFETY: all interfaces are created and used on this thread; the render target is dropped
    // before the bitmap is read.
    unsafe {
        let bmp = gfx
            .wic
            .CreateBitmap(w as u32, h as u32, &GUID_WICPixelFormat32bppPBGRA, WICBitmapCacheOnLoad)
            .map_err(|e| format!("WIC CreateBitmap: {e}"))?;
        let props = D2D1_RENDER_TARGET_PROPERTIES {
            r#type: D2D1_RENDER_TARGET_TYPE_DEFAULT,
            pixelFormat: D2D1_PIXEL_FORMAT {
                format: DXGI_FORMAT_B8G8R8A8_UNORM,
                alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
            },
            dpiX: 96.0 * scale,
            dpiY: 96.0 * scale,
            usage: D2D1_RENDER_TARGET_USAGE_NONE,
            minLevel: D2D1_FEATURE_LEVEL_DEFAULT,
        };
        let rt = gfx.d2d.CreateWicBitmapRenderTarget(&bmp, &props).map_err(|e| format!("D2D render target: {e}"))?;
        rt.SetAntialiasMode(D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
        rt.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
        let white = D2D1_COLOR_F { r: 1.0, g: 1.0, b: 1.0, a: 1.0 };
        let brush = create_solid_brush(&rt, &white).map_err(|e| format!("D2D brush: {e}"))?;
        rt.BeginDraw();
        let clear = match bg {
            Some(c) => d2d(Color { rgb: c, a: 1.0 }),
            None => D2D1_COLOR_F { r: 0.0, g: 0.0, b: 0.0, a: 0.0 },
        };
        rt.Clear(Some(&clear));
        let p = Painter { gfx, rt: &rt, brush };
        let drawn = p.draw(&scene);
        drop(p);
        let ended = rt.EndDraw(None, None);
        drawn.map_err(|e| format!("draw text: {e}"))?;
        ended.map_err(|e| format!("EndDraw: {e}"))?;
        drop(rt);
        Ok((bmp, w, h))
    }
}

/// Encode a PBGRA WIC bitmap as PNG (straight alpha).
fn write_png(wic: &IWICImagingFactory, src: &IWICBitmap, path: &Path) -> Result<(), String> {
    let wpath: Vec<u16> = path.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
    // SAFETY: COM calls on this thread; wpath is NUL-terminated and outlives the calls.
    unsafe {
        let conv = wic.CreateFormatConverter().map_err(|e| format!("WIC converter: {e}"))?;
        conv.Initialize(
            src,
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
        let (mut w, mut h) = (0u32, 0u32);
        src.GetSize(&mut w, &mut h).map_err(|e| format!("bitmap size: {e}"))?;
        frame.SetSize(w, h).map_err(|e| format!("frame size: {e}"))?;
        let mut fmt = GUID_WICPixelFormat32bppBGRA;
        frame.SetPixelFormat(&mut fmt).map_err(|e| format!("frame format: {e}"))?;
        frame.WriteSource(&conv, std::ptr::null()).map_err(|e| format!("write: {e}"))?;
        frame.Commit().map_err(|e| format!("frame commit: {e}"))?;
        enc.Commit().map_err(|e| format!("encoder commit: {e}"))?;
    }
    Ok(())
}

// -------------------------------------------------------------------------------- window

struct Press {
    /// Cursor position (screen px) at button-down.
    start: POINT,
    /// Window top-left at button-down.
    origin: POINT,
    target: Hit,
    dragging: bool,
}

struct Model {
    state: BubbleState,
    scale: f32,
    visual: Visual,
    tracking: bool,
    press: Option<Press>,
    last: Option<IWICBitmap>,
    /// Current window size (px) as last pushed with UpdateLayeredWindow.
    size: (i32, i32),
}

struct Inner {
    hwnd: Cell<HWND>,
    gfx: Gfx,
    model: RefCell<Model>,
    on_event: Box<dyn Fn(BubbleEvent)>,
}

impl Inner {
    fn window_rect(&self) -> Rect {
        let mut r = RECT::default();
        // SAFETY: GetWindowRect writes into a local RECT; an invalid hwnd just fails.
        if unsafe { GetWindowRect(self.hwnd.get(), &mut r) }.is_err() {
            return Rect::default();
        }
        Rect::from(r)
    }

    /// Render the current model and push it to the window at `pos` (or where it is now).
    fn present(&self, m: &mut Model, pos: Option<(i32, i32)>) {
        let (bmp, w, h) = match paint(&self.gfx, &m.state, m.visual, m.scale, None) {
            Ok(x) => x,
            Err(e) => {
                log::warn(&format!("bubble render: {e}"));
                return;
            }
        };
        let cur = self.window_rect();
        let (x, y) = pos.unwrap_or((cur.x, cur.y));
        if let Err(e) = push_layered(self.hwnd.get(), &bmp, w, h, x, y) {
            log::warn(&format!("bubble present: {e}"));
        }
        m.size = (w, h);
        m.last = Some(bmp);
    }

    /// Re-render keeping the snapped edge when the size changes (mode, account count, DPI).
    fn relayout(&self, m: &mut Model) {
        let mode = effective_mode(&m.state);
        let (w, h) = px_size(mode, m.state.accounts.len(), m.scale);
        let cur = self.window_rect();
        let pos = if (w, h) != m.size || cur.w != w || cur.h != h {
            let area = work_area_for(&cur);
            Some(anchored_rect(&cur, w, h, &area))
        } else {
            None
        };
        self.present(m, pos);
    }

    /// Re-read the monitor DPI and relayout if it changed. A WM_DPICHANGED sent from inside the
    /// drag's SetWindowPos arrives while the model is borrowed and is dropped, so the end of a
    /// drag calls this (with the model not borrowed) to catch up.
    fn sync_dpi(&self) {
        let Ok(mut m) = self.model.try_borrow_mut() else {
            return;
        };
        let s = win::dpi_scale(self.hwnd.get());
        if (s - m.scale).abs() > 0.001 {
            m.scale = s;
            self.relayout(&mut m);
        }
    }

    fn snap(&self) -> (i32, i32) {
        let r = self.window_rect();
        let scale = self.model.try_borrow().map(|m| m.scale).unwrap_or(1.0);
        let area = work_area_for(&r);
        let (x, y) = snap_rect(&r, &area, (EDGE_MARGIN_DIP * scale).round() as i32);
        // SAFETY: plain SetWindowPos on our own window.
        let _ = unsafe { SetWindowPos(self.hwnd.get(), None, x, y, 0, 0, SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE) };
        (x, y)
    }
}

/// Copy `bmp` into a DIB section and hand it to UpdateLayeredWindow at (x, y).
fn push_layered(hwnd: HWND, bmp: &IWICBitmap, w: i32, h: i32, x: i32, y: i32) -> Result<(), String> {
    // SAFETY: standard GDI sequence; every handle created here is released on all paths, the
    // DIB bits pointer is only used while the DIB is alive and is sized w*h*4 bytes.
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
                biWidth: w,
                biHeight: -h, // top-down
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
        let len = (w as usize) * (h as usize) * 4;
        let buf = std::slice::from_raw_parts_mut(bits as *mut u8, len);
        let copied = bmp.CopyPixels(std::ptr::null(), (w * 4) as u32, buf);
        let old = SelectObject(mem, HGDIOBJ(dib.0));
        let result = match copied {
            Err(e) => Err(format!("CopyPixels: {e}")),
            Ok(()) => {
                let blend = BLENDFUNCTION {
                    BlendOp: AC_SRC_OVER as u8,
                    BlendFlags: 0,
                    SourceConstantAlpha: 255,
                    AlphaFormat: AC_SRC_ALPHA as u8,
                };
                let pt = POINT { x, y };
                let sz = SIZE { cx: w, cy: h };
                let src = POINT { x: 0, y: 0 };
                UpdateLayeredWindow(
                    hwnd,
                    screen,
                    Some(&pt),
                    Some(&sz),
                    mem,
                    Some(&src),
                    COLORREF(0),
                    Some(&blend),
                    ULW_ALPHA,
                )
                .map_err(|e| format!("UpdateLayeredWindow: {e}"))
            }
        };
        SelectObject(mem, old);
        let _ = DeleteObject(HGDIOBJ(dib.0));
        let _ = DeleteDC(mem);
        ReleaseDC(None, screen);
        result
    }
}

fn register_class() -> Result<(), String> {
    let class = win::wide(CLASS_NAME);
    // SAFETY: LoadCursorW with a system cursor id; RegisterClassExW reads a fully initialised
    // WNDCLASSEXW whose class-name pointer (`class`) outlives the call.
    unsafe {
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(wndproc),
            hInstance: win::hinstance(),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            lpszClassName: PCWSTR(class.as_ptr()),
            ..Default::default()
        };
        if RegisterClassExW(&wc) == 0 && GetLastError() != ERROR_CLASS_ALREADY_EXISTS {
            return Err(format!("RegisterClassExW({CLASS_NAME}): {}", win::last_error()));
        }
    }
    Ok(())
}

fn lparam_xy(lp: LPARAM) -> (i32, i32) {
    let x = (lp.0 & 0xFFFF) as u16 as i16 as i32;
    let y = ((lp.0 >> 16) & 0xFFFF) as u16 as i16 as i32;
    (x, y)
}

fn cursor_pos() -> POINT {
    let mut p = POINT::default();
    // SAFETY: writes into a local POINT.
    let _ = unsafe { GetCursorPos(&mut p) };
    p
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    // SAFETY: GWLP_USERDATA is either 0 or `Rc::as_ptr` of the Inner owned by a live Bubble
    // (cleared before that Rc is released). We take our own strong reference for the duration
    // of the message so the callback may drop the Bubble safely.
    let raw = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *const Inner;
    if raw.is_null() {
        // SAFETY: default processing for messages before/after our state exists.
        return unsafe { DefWindowProcW(hwnd, msg, wp, lp) };
    }
    // SAFETY: see above — the pointer came from Rc::into_raw-compatible Rc::as_ptr and the
    // owning Rc is still alive, so bumping the count and re-materialising an Rc is sound.
    let inner: Rc<Inner> = unsafe {
        Rc::increment_strong_count(raw);
        Rc::from_raw(raw)
    };
    let (result, event) = handle(&inner, hwnd, msg, wp, lp);
    if let Some(ev) = event {
        (inner.on_event)(ev);
    }
    match result {
        Some(r) => r,
        // SAFETY: default processing.
        None => unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
    }
}

/// Message handling with the model borrowed; returns (result or None for DefWindowProc, event).
fn handle(inner: &Inner, hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> (Option<LRESULT>, Option<BubbleEvent>) {
    match msg {
        WM_MOUSEACTIVATE => return (Some(LRESULT(MA_NOACTIVATE as isize)), None),
        WM_NCDESTROY => {
            // SAFETY: clearing our own user data; the owning Bubble still holds its Rc.
            unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0) };
            return (None, None);
        }
        _ => {}
    }
    // Re-entrant messages (sent while we are inside set_state/present) get default handling.
    let Ok(mut m) = inner.model.try_borrow_mut() else {
        return (None, None);
    };
    let mode = effective_mode(&m.state);
    let n = m.state.accounts.len();
    match msg {
        WM_MOUSEMOVE => {
            let (px, py) = lparam_xy(lp);
            let (x, y) = (px as f32 / m.scale, py as f32 / m.scale);
            let scale = m.scale;
            let size = m.size;
            if let Some(p) = m.press.as_mut() {
                let c = cursor_pos();
                let (dx, dy) = (c.x - p.start.x, c.y - p.start.y);
                let mut changed = false;
                if !p.dragging {
                    let dist = ((dx * dx + dy * dy) as f32).sqrt();
                    if dist >= DRAG_THRESHOLD_DIP * scale {
                        p.dragging = true;
                        changed = true;
                    }
                }
                if p.dragging {
                    let (w, h) = size;
                    let (nx, ny) = (p.origin.x + dx, p.origin.y + dy);
                    let probe = Rect { x: nx, y: ny, w, h };
                    let area = work_area_for(&probe);
                    let (cx, cy) = clamp_to_area(nx, ny, w, h, &area);
                    // SAFETY: moving our own window.
                    let _ =
                        unsafe { SetWindowPos(hwnd, None, cx, cy, 0, 0, SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE) };
                }
                if changed {
                    m.visual.dragging = true;
                    m.visual.hover = Hit::None;
                    inner.present(&mut m, None);
                }
            } else {
                let hit = hit_test(mode, n, x, y);
                if !m.tracking {
                    let mut tme = TRACKMOUSEEVENT {
                        cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                        dwFlags: TME_LEAVE,
                        hwndTrack: hwnd,
                        dwHoverTime: 0,
                    };
                    // SAFETY: tme is a fully initialised local.
                    m.tracking = unsafe { TrackMouseEvent(&mut tme) }.is_ok();
                }
                if hit != m.visual.hover {
                    m.visual.hover = hit;
                    inner.present(&mut m, None);
                }
            }
            (Some(LRESULT(0)), None)
        }
        WM_MOUSELEAVE => {
            m.tracking = false;
            if m.press.is_none() && m.visual.hover != Hit::None {
                m.visual.hover = Hit::None;
                inner.present(&mut m, None);
            }
            (Some(LRESULT(0)), None)
        }
        WM_LBUTTONDOWN => {
            let (px, py) = lparam_xy(lp);
            let hit = hit_test(mode, n, px as f32 / m.scale, py as f32 / m.scale);
            if hit == Hit::None || m.press.is_some() {
                return (Some(LRESULT(0)), None);
            }
            let r = inner.window_rect();
            m.press =
                Some(Press { start: cursor_pos(), origin: POINT { x: r.x, y: r.y }, target: hit, dragging: false });
            m.visual.pressed = hit;
            m.visual.hover = hit;
            // SAFETY: capturing the mouse for our own window on its thread.
            unsafe { SetCapture(hwnd) };
            inner.present(&mut m, None);
            (Some(LRESULT(0)), None)
        }
        WM_LBUTTONUP => {
            let Some(p) = m.press.take() else {
                return (Some(LRESULT(0)), None);
            };
            m.visual.pressed = Hit::None;
            m.visual.dragging = false;
            // Capture cancels TME_LEAVE tracking; re-arm on the next WM_MOUSEMOVE.
            m.tracking = false;
            // press is None now, so the WM_CAPTURECHANGED this triggers is ignored.
            // SAFETY: releasing our own capture.
            if unsafe { GetCapture() } == hwnd {
                let _ = unsafe { ReleaseCapture() };
            }
            let event = if p.dragging {
                drop(m);
                inner.sync_dpi();
                let (x, y) = inner.snap();
                let Ok(mut m) = inner.model.try_borrow_mut() else {
                    return (Some(LRESULT(0)), Some(BubbleEvent::DragEnd(x, y)));
                };
                let (px, py) = lparam_xy(lp);
                m.visual.hover = hit_test(mode, n, px as f32 / m.scale, py as f32 / m.scale);
                inner.present(&mut m, None);
                Some(BubbleEvent::DragEnd(x, y))
            } else {
                let ev = match p.target {
                    Hit::Bubble => Some(BubbleEvent::Click),
                    Hit::Avatar(i) => m.state.accounts.get(i).map(|a| BubbleEvent::AvatarClick(a.id.clone())),
                    Hit::More => Some(BubbleEvent::MoreClick),
                    Hit::Grip | Hit::None => None,
                };
                inner.present(&mut m, None);
                ev
            };
            (Some(LRESULT(0)), event)
        }
        WM_CAPTURECHANGED => {
            // Capture lost while pressed (e.g. window hidden mid-drag): finish without clicking.
            let Some(p) = m.press.take() else {
                return (Some(LRESULT(0)), None);
            };
            m.visual = Visual::default();
            m.tracking = false;
            if p.dragging {
                drop(m);
                inner.sync_dpi();
                let (x, y) = inner.snap();
                if let Ok(mut m) = inner.model.try_borrow_mut() {
                    inner.present(&mut m, None);
                }
                return (Some(LRESULT(0)), Some(BubbleEvent::DragEnd(x, y)));
            }
            inner.present(&mut m, None);
            (Some(LRESULT(0)), None)
        }
        WM_RBUTTONUP => {
            if m.press.is_some() {
                return (Some(LRESULT(0)), None);
            }
            (Some(LRESULT(0)), Some(BubbleEvent::RightClick))
        }
        WM_SETCURSOR => {
            if (lp.0 & 0xFFFF) as u32 != HTCLIENT {
                return (None, None);
            }
            let id = if m.press.as_ref().map(|p| p.dragging).unwrap_or(false) || !m.visual.hover.clickable() {
                IDC_ARROW
            } else {
                IDC_HAND
            };
            // SAFETY: system cursor ids; SetCursor accepts any valid HCURSOR.
            unsafe {
                if let Ok(c) = LoadCursorW(None, id) {
                    SetCursor(c);
                }
            }
            (Some(LRESULT(1)), None)
        }
        WM_DPICHANGED => {
            let dpi = ((wp.0 >> 16) & 0xFFFF) as f32;
            if dpi > 0.0 {
                m.scale = dpi / 96.0;
            }
            inner.relayout(&mut m);
            (Some(LRESULT(0)), None)
        }
        WM_DISPLAYCHANGE => {
            m.scale = win::dpi_scale(hwnd);
            let cur = inner.window_rect();
            let area = work_area_for(&cur);
            let (w, h) = m.size;
            let pos = clamp_to_area(cur.x, cur.y, w, h, &area);
            inner.present(&mut m, Some(pos));
            (None, None)
        }
        _ => (None, None),
    }
}

// ------------------------------------------------------------------------------- public API

pub struct Bubble {
    hwnd: HWND,
    inner: Rc<Inner>,
}

impl BubbleHost for Bubble {
    fn create(pos: Option<(i32, i32)>, on_event: Box<dyn Fn(BubbleEvent)>) -> Result<Bubble, String> {
        let gfx = Gfx::new()?;
        register_class()?;
        let sys = win::dpi_scale(HWND::default());
        let (w0, h0) = px_size(BubbleMode::Icon, 0, sys);
        let (x0, y0) = match pos {
            Some(p) => p,
            None => {
                let a = primary_work_area();
                let m = (DEFAULT_MARGIN_DIP * sys).round() as i32;
                (a.x + a.w - w0 - m, a.y + a.h - h0 - m)
            }
        };
        let class = win::wide(CLASS_NAME);
        let title = win::wide(brand::NAME);
        // SAFETY: class registered above; `class` and `title` outlive the call; no
        // parent/menu/create params.
        let hwnd = unsafe {
            CreateWindowExW(
                WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                PCWSTR(class.as_ptr()),
                PCWSTR(title.as_ptr()),
                WS_POPUP,
                x0,
                y0,
                w0,
                h0,
                None,
                None,
                win::hinstance(),
                None,
            )
        }
        .map_err(|e| format!("CreateWindowExW({CLASS_NAME}): {e}"))?;

        let scale = win::dpi_scale(hwnd);
        let state = BubbleState { mode: BubbleMode::Icon, active: None, accounts: Vec::new(), waiting: 0 };
        let inner = Rc::new(Inner {
            hwnd: Cell::new(hwnd),
            gfx,
            model: RefCell::new(Model {
                state,
                scale,
                visual: Visual::default(),
                tracking: false,
                press: None,
                last: None,
                size: (w0, h0),
            }),
            on_event,
        });
        // SAFETY: stores a pointer to the Rc allocation; it is cleared in Drop/WM_NCDESTROY
        // before the Bubble's Rc is released, and wndproc takes its own strong reference.
        unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, Rc::as_ptr(&inner) as isize) };

        // Initial position at the real DPI, clamped into the work area of its monitor.
        let (w, h) = px_size(BubbleMode::Icon, 0, scale);
        let (x, y) = if pos.is_some() {
            let probe = Rect { x: x0, y: y0, w, h };
            clamp_to_area(x0, y0, w, h, &work_area_for(&probe))
        } else {
            let a = primary_work_area();
            let m = (DEFAULT_MARGIN_DIP * scale).round() as i32;
            (a.x + a.w - w - m, a.y + a.h - h - m)
        };
        {
            let mut m = inner.model.borrow_mut();
            inner.present(&mut m, Some((x, y)));
        }
        let b = Bubble { hwnd, inner };
        b.show(true);
        Ok(b)
    }

    /// Apply a new state: resize (anchored to the snapped edge) and re-render.
    fn set_state(&mut self, s: BubbleState) {
        let mut s = s;
        s.accounts.truncate(MAX_TRAY);
        let Ok(mut m) = self.inner.model.try_borrow_mut() else {
            log::warn("bubble: set_state while busy; ignored");
            return;
        };
        if m.state == s && m.last.is_some() {
            return;
        }
        if effective_mode(&m.state) != effective_mode(&s) || m.state.accounts.len() != s.accounts.len() {
            m.visual.hover = Hit::None;
        }
        m.state = s;
        self.inner.relayout(&mut m);
    }

    fn rect(&self) -> Rect {
        self.inner.window_rect()
    }

    fn show(&self, visible: bool) {
        // SAFETY: plain show/z-order calls on our own window.
        unsafe {
            if visible {
                let _ = ShowWindow(self.hwnd, SW_SHOWNOACTIVATE);
                let _ = SetWindowPos(self.hwnd, HWND_TOPMOST, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
            } else {
                let _ = ShowWindow(self.hwnd, SW_HIDE);
            }
        }
    }

    fn set_position(&self, x: i32, y: i32) {
        // SAFETY: moving our own window.
        let _ = unsafe { SetWindowPos(self.hwnd, None, x, y, 0, 0, SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE) };
    }

    /// Write the last rendered frame as PNG (straight alpha) via the WIC encoder.
    fn render_to_png(&self, path: &Path) -> Result<(), String> {
        let m = self.inner.model.try_borrow().map_err(|_| "bubble busy".to_string())?;
        let bmp = m.last.as_ref().ok_or("bubble: nothing rendered yet")?;
        write_png(&self.inner.gfx.wic, bmp, path)
    }

    /// Whether the window is currently shown.
    fn is_visible(&self) -> bool {
        // SAFETY: read-only query on our own window.
        unsafe { IsWindowVisible(self.hwnd) }.as_bool()
    }
}

impl Drop for Bubble {
    fn drop(&mut self) {
        // SAFETY: detach our state from the window before destroying it; a wndproc running
        // further up the stack holds its own Rc, so the Inner outlives it.
        unsafe {
            SetWindowLongPtrW(self.hwnd, GWLP_USERDATA, 0);
            let _ = DestroyWindow(self.hwnd);
        }
        self.inner.hwnd.set(HWND::default());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::bubble::BubbleAccount;

    fn acct(id: &str, name: &str, color: u32, running: bool, active: bool) -> BubbleAccount {
        BubbleAccount {
            id: id.into(),
            name: name.into(),
            color,
            initials: String::new(),
            running,
            active,
            needs_sign_in: false,
            limited: false,
        }
    }

    fn fake(n: usize) -> Vec<BubbleAccount> {
        let all = [
            acct("a", "Main", 0xF08A5D, true, true),
            acct("b", "Work", 0x5B9CF6, false, false),
            acct("c", "Side project", 0x3DD6A1, true, false),
        ];
        all[..n].to_vec()
    }

    /// Renders every mode/count to PNGs under %TEMP%\sessionmux-bubble-preview for visual review.
    /// Creates (and briefly shows) real bubble windows. Run with
    /// `cargo test -p sessionmux bubble -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn render_previews() {
        use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};
        // SAFETY: test thread COM init; left initialised for the test's lifetime.
        let _ = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
        let dir = std::env::temp_dir().join("sessionmux-bubble-preview");
        std::fs::create_dir_all(&dir).unwrap();

        let mut b = Bubble::create(None, Box::new(|ev| println!("event {ev:?}"))).expect("create");
        b.render_to_png(&dir.join("icon-empty.png")).unwrap();
        for n in 1..=3 {
            let accounts = fake(n);
            let active = accounts.iter().find(|a| a.active).cloned();
            b.set_state(BubbleState {
                mode: BubbleMode::Icon,
                active: active.clone(),
                accounts: accounts.clone(),
                waiting: 0,
            });
            b.render_to_png(&dir.join(format!("icon-{n}.png"))).unwrap();
            b.set_state(BubbleState { mode: BubbleMode::Tray, active, accounts, waiting: 0 });
            let r = b.rect();
            println!("tray n={n} rect {}x{}", r.w, r.h);
            b.render_to_png(&dir.join(format!("tray-{n}.png"))).unwrap();
        }
        drop(b);

        // 3x previews on dark and light backgrounds, plus hover states.
        let gfx = Gfx::new().unwrap();
        let hover = Visual { hover: Hit::Avatar(1), pressed: Hit::None, dragging: false };
        let more = Visual { hover: Hit::More, pressed: Hit::None, dragging: false };
        let icon_hover = Visual { hover: Hit::Bubble, pressed: Hit::None, dragging: false };
        let cases: Vec<(String, BubbleState, Visual)> = vec![
            (
                "x3-icon-empty".into(),
                BubbleState { mode: BubbleMode::Icon, active: None, accounts: vec![], waiting: 0 },
                Visual::default(),
            ),
            (
                "x3-icon".into(),
                BubbleState { mode: BubbleMode::Icon, active: Some(fake(1)[0].clone()), accounts: fake(1), waiting: 0 },
                Visual::default(),
            ),
            (
                "x3-icon-hover".into(),
                BubbleState { mode: BubbleMode::Icon, active: Some(fake(1)[0].clone()), accounts: fake(1), waiting: 0 },
                icon_hover,
            ),
            (
                "x3-tray-1".into(),
                BubbleState { mode: BubbleMode::Tray, active: None, accounts: fake(1), waiting: 0 },
                Visual::default(),
            ),
            (
                "x3-tray-3".into(),
                BubbleState { mode: BubbleMode::Tray, active: None, accounts: fake(3), waiting: 0 },
                Visual::default(),
            ),
            (
                "x3-tray-3-hover".into(),
                BubbleState { mode: BubbleMode::Tray, active: None, accounts: fake(3), waiting: 0 },
                hover,
            ),
            (
                "x3-tray-2-more".into(),
                BubbleState { mode: BubbleMode::Tray, active: None, accounts: fake(2), waiting: 0 },
                more,
            ),
            (
                "x3-icon-waiting".into(),
                BubbleState { mode: BubbleMode::Icon, active: Some(fake(1)[0].clone()), accounts: fake(1), waiting: 2 },
                Visual::default(),
            ),
            (
                "x3-tray-3-waiting-limited".into(),
                BubbleState {
                    mode: BubbleMode::Tray,
                    active: None,
                    accounts: {
                        let mut a = fake(3);
                        a[1].limited = true;
                        a
                    },
                    waiting: 12,
                },
                Visual::default(),
            ),
        ];
        for (name, st, v) in cases {
            for (suffix, bg) in [("dark", 0x2B2D33u32), ("light", 0xE8E9EDu32)] {
                let (bmp, _, _) = paint(&gfx, &st, v, 3.0, Some(bg)).unwrap();
                write_png(&gfx.wic, &bmp, &dir.join(format!("{name}-{suffix}.png"))).unwrap();
            }
        }
        println!("previews in {}", dir.display());
    }
}

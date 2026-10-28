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

//! The floating bubble's window on macOS: a borderless, non-activating panel that floats above
//! other windows on every Space, drawn with Core Graphics and Core Text, no web view.
//!
//! - Every frame is the scene `bubble::model::scene` builds, drawn into a premultiplied BGRA bitmap
//!   at the display's backing scale and shown as the contents of the view's layer. The scene's
//!   shadows are drawn by the scene itself; the window has none.
//! - Positions passed in / out (`create`, `set_position`, `DragEnd`) are points with the origin at
//!   the top-left of the primary display and y growing downwards, like every `Rect`.
//! - A press that moves drags the window; on release it snaps to the nearest left / right edge of
//!   the visible frame (without menu bar and Dock) of the display it is on. A right-click or a
//!   Control-click is `RightClick`.
//! - The window never becomes the key or main window, so a click never takes focus from the app
//!   the user works in, and it reacts to the first click even while Sessionmux is not active.
//! - A move to a display with another backing scale redraws at that scale; a display change keeps
//!   the window inside the visible area.
//! - The event callback runs after all internal borrows are released, so it may call back into
//!   the bubble or drop it.

use std::cell::RefCell;
use std::path::Path;
use std::ptr::NonNull;
use std::rc::{Rc, Weak};

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{define_class, msg_send, sel, AnyThread, DefinedClass, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSApplicationDidChangeScreenParametersNotification, NSBackingStoreType, NSColor, NSCursor, NSEvent,
    NSEventModifierFlags, NSPanel, NSResponder, NSScreen, NSStatusWindowLevel, NSTrackingArea, NSTrackingAreaOptions,
    NSView, NSWindow, NSWindowAnimationBehavior, NSWindowCollectionBehavior, NSWindowStyleMask,
};
use objc2_core_foundation::{CFRange, CFRetained, CFString, CGPoint, CGRect, CGSize};
use objc2_core_graphics::{
    kCGColorSpaceSRGB, CGAffineTransformIdentity, CGBitmapContextCreate, CGBitmapContextCreateImage,
    CGBitmapContextGetBytesPerRow, CGBitmapContextGetData, CGColor, CGColorSpace, CGContext, CGImage, CGImageAlphaInfo,
    CGImageByteOrderInfo, CGPath,
};
use objc2_core_text::{CTFont, CTFontOrientation, CTFontUIFontType};
use objc2_foundation::{NSNotification, NSNotificationCenter, NSObject, NSPoint, NSRect, NSSize};
use objc2_quartz_core::{CALayer, CATransaction};

use crate::platform::{BubbleHost, Desktop, Os, Rect};
use crate::ui::bubble::model::{
    self, anchored_rect, clamp_to_area, dip_size, effective_mode, hit_test, snap_rect, Hit, Visual, DEFAULT_MARGIN_DIP,
    DRAG_THRESHOLD_DIP, EDGE_MARGIN_DIP, MAX_TRAY,
};
use crate::ui::bubble::scene::{Color, Prim, Scene, LETTER_SPACING_EM, TEXT_SHADOW_DY};
use crate::ui::bubble::{BubbleEvent, BubbleMode, BubbleState};
use crate::ui::panel::encode_png;
use crate::util::log;

/// Backing scale of the captures `render_to_png` writes (a Retina display).
const PNG_SCALE: f64 = 2.0;
/// Height of the primary display assumed while no display can be read (a common laptop).
const NOMINAL_PRIMARY_H: f64 = 900.0;

// ---------------------------------------------------------------------------- coordinates

/// Height of the primary display (the one with the menu bar), whose bottom-left corner is the
/// origin of the system's screen coordinates.
pub(crate) fn primary_height(mtm: MainThreadMarker) -> f64 {
    NSScreen::screens(mtm).iter().next().map(|s| s.frame().size.height).unwrap_or(NOMINAL_PRIMARY_H)
}

/// A `Rect` (top-left origin, y down) as a screen frame (bottom-left origin, y up).
pub(crate) fn screen_frame(r: &Rect, primary_h: f64) -> NSRect {
    NSRect::new(
        NSPoint::new(f64::from(r.x), primary_h - f64::from(r.y) - f64::from(r.h)),
        NSSize::new(f64::from(r.w), f64::from(r.h)),
    )
}

/// A screen frame (bottom-left origin, y up) as a `Rect` (top-left origin, y down).
pub(crate) fn top_left_rect(f: NSRect, primary_h: f64) -> Rect {
    Rect {
        x: f.origin.x.round() as i32,
        y: (primary_h - f.origin.y - f.size.height).round() as i32,
        w: f.size.width.round() as i32,
        h: f.size.height.round() as i32,
    }
}

/// The usable area of the display nearest to `r`.
fn work_area_for(r: &Rect) -> Rect {
    <Os as Desktop>::work_area(r)
}

// ------------------------------------------------------------------------------ rendering

/// Premultiplied alpha first, 32-bit little endian: the bytes of each pixel are B, G, R, A.
fn bitmap_info() -> u32 {
    CGImageAlphaInfo::PremultipliedFirst.0 | CGImageByteOrderInfo::Order32Little.0
}

/// A cleared `w`×`h` sRGB bitmap context that owns its pixels (premultiplied BGRA, top row first).
fn new_context(w: usize, h: usize) -> Option<CFRetained<CGContext>> {
    // SAFETY: reading an immutable extern constant of Core Graphics.
    let space = CGColorSpace::with_name(Some(unsafe { kCGColorSpaceSRGB }))?;
    // SAFETY: a null data pointer makes the context allocate and own `h` rows of `w * 4` bytes.
    let ctx = unsafe { CGBitmapContextCreate(std::ptr::null_mut(), w, h, 8, w * 4, Some(&space), bitmap_info()) }?;
    CGContext::clear_rect(Some(&ctx), CGRect::new(CGPoint::ZERO, CGSize::new(w as f64, h as f64)));
    Some(ctx)
}

/// The pixels of a context made by [`new_context`]: premultiplied BGRA, top row first, tightly
/// packed.
fn context_pixels(ctx: &CGContext, w: usize, h: usize) -> Vec<u8> {
    let data = CGBitmapContextGetData(Some(ctx)) as *const u8;
    let stride = CGBitmapContextGetBytesPerRow(Some(ctx));
    let mut out = Vec::with_capacity(w * h * 4);
    if data.is_null() || stride < w * 4 {
        out.resize(w * h * 4, 0);
        return out;
    }
    for row in 0..h {
        // SAFETY: the context owns `h` rows of `stride` bytes at `data` and is alive for the whole
        // loop; each row read is `w * 4 <= stride` bytes from the start of its row.
        let line = unsafe { std::slice::from_raw_parts(data.add(row * stride), w * 4) };
        out.extend_from_slice(line);
    }
    out
}

/// An image of premultiplied BGRA pixels (`w`×`h`, top row first, tightly packed).
pub(crate) fn image_from_bgra(px: &[u8], w: usize, h: usize) -> Option<CFRetained<CGImage>> {
    if px.len() < w * h * 4 {
        return None;
    }
    let ctx = new_context(w, h)?;
    let data = CGBitmapContextGetData(Some(&ctx)) as *mut u8;
    let stride = CGBitmapContextGetBytesPerRow(Some(&ctx));
    if data.is_null() || stride < w * 4 {
        return None;
    }
    for row in 0..h {
        // SAFETY: the destination row lies inside the `h * stride` bytes the context owns, the
        // source row inside `px` (checked above); the two buffers never overlap.
        unsafe { std::ptr::copy_nonoverlapping(px.as_ptr().add(row * w * 4), data.add(row * stride), w * 4) };
    }
    CGBitmapContextCreateImage(Some(&ctx))
}

/// Premultiplied pixels to straight alpha, keeping the byte order.
pub(crate) fn unpremultiply(px: &[u8]) -> Vec<u8> {
    let mut out = px.to_vec();
    for p in out.as_chunks_mut::<4>().0 {
        let a = u32::from(p[3]);
        if a == 0 {
            p[..3].fill(0);
        } else if a < 255 {
            for c in &mut p[..3] {
                *c = ((u32::from(*c) * 255 + a / 2) / a).min(255) as u8;
            }
        }
    }
    out
}

/// Write premultiplied BGRA pixels as PNG with straight alpha.
pub(crate) fn write_png(path: &Path, w: usize, h: usize, premultiplied: &[u8]) -> Result<(), String> {
    let png = encode_png(w as u32, h as u32, &unpremultiply(premultiplied));
    std::fs::write(path, png).map_err(|e| format!("write {}: {e}", path.display()))
}

fn cg_color(c: Color) -> CFRetained<CGColor> {
    let (r, g, b) = c.channels();
    CGColor::new_srgb(f64::from(r), f64::from(g), f64::from(b), f64::from(c.a))
}

/// Corner radius that fits a `w`×`h` rectangle (Core Graphics refuses a larger one).
fn fit_radius(w: f64, h: f64, radius: f64) -> f64 {
    radius.clamp(0.0, (w.min(h) / 2.0).max(0.0))
}

/// One run of glyphs from one font, with each glyph's advance.
struct Run {
    font: CFRetained<CTFont>,
    glyphs: Vec<u16>,
    advances: Vec<f64>,
}

/// The glyphs of `text`, character by character; a character the system font lacks comes from
/// the font the system picks for it, and one no font has is left out.
fn glyph_runs(font: &CFRetained<CTFont>, text: &str) -> Vec<Run> {
    let mut runs: Vec<Run> = Vec::new();
    for ch in text.chars() {
        let mut buf = [0u16; 2];
        let n = ch.encode_utf16(&mut buf).len();
        let mut glyphs = [0u16; 2];
        let lookup = |f: &CTFont, glyphs: &mut [u16; 2]| {
            let mut chars = buf;
            // SAFETY: both buffers hold at least `n` (1 or 2) elements and live across the call.
            unsafe {
                f.glyphs_for_characters(
                    NonNull::new_unchecked(chars.as_mut_ptr()),
                    NonNull::new_unchecked(glyphs.as_mut_ptr()),
                    n as isize,
                )
            }
        };
        let mut used = font.clone();
        if !lookup(font, &mut glyphs) {
            let s = CFString::from_str(&ch.to_string());
            // SAFETY: the range covers the whole string just made.
            let fallback = unsafe { font.for_string(&s, CFRange::new(0, n as isize)) };
            glyphs = [0; 2];
            if !lookup(&fallback, &mut glyphs) {
                continue;
            }
            used = fallback;
        }
        let mut gs: Vec<u16> = glyphs[..n].iter().copied().filter(|&g| g != 0).collect();
        if gs.is_empty() {
            continue;
        }
        let mut adv = vec![CGSize::new(0.0, 0.0); gs.len()];
        // SAFETY: `gs` and `adv` both hold `gs.len()` elements and live across the call.
        unsafe {
            used.advances_for_glyphs(
                CTFontOrientation::Horizontal,
                NonNull::new_unchecked(gs.as_mut_ptr()),
                adv.as_mut_ptr(),
                gs.len() as isize,
            );
        }
        let advances: Vec<f64> = adv.iter().map(|a| a.width).collect();
        match runs.last_mut() {
            Some(r) if CFRetained::as_ptr(&r.font) == CFRetained::as_ptr(&used) => {
                r.glyphs.extend(gs);
                r.advances.extend(advances);
            }
            _ => runs.push(Run { font: used, glyphs: gs, advances }),
        }
    }
    runs
}

/// Draws a scene's primitives into a context whose user space is scaled to DIPs. The context's
/// y axis points up, so every y of the scene (top-left origin) is flipped against its height.
struct Painter<'a> {
    ctx: &'a CGContext,
    h: f64,
}

impl Painter<'_> {
    fn fy(&self, y: f32) -> f64 {
        self.h - f64::from(y)
    }

    fn fill(&self, c: Color) {
        CGContext::set_fill_color_with_color(Some(self.ctx), Some(&cg_color(c)));
    }

    fn stroke(&self, c: Color, w: f32) {
        CGContext::set_stroke_color_with_color(Some(self.ctx), Some(&cg_color(c)));
        CGContext::set_line_width(Some(self.ctx), f64::from(w));
    }

    fn square(&self, cx: f32, cy: f32, r: f32) -> CGRect {
        let r = f64::from(r);
        CGRect::new(CGPoint::new(f64::from(cx) - r, self.fy(cy) - r), CGSize::new(2.0 * r, 2.0 * r))
    }

    /// The path of a rounded rectangle given by its left, top, right and bottom edges.
    fn rounded(&self, l: f32, t: f32, r: f32, b: f32, radius: f32) -> Option<CFRetained<CGPath>> {
        let (w, h) = (f64::from(r - l), f64::from(b - t));
        if w <= 0.0 || h <= 0.0 {
            return None;
        }
        let rect = CGRect::new(CGPoint::new(f64::from(l), self.fy(b)), CGSize::new(w, h));
        let rad = fit_radius(w, h, f64::from(radius));
        // SAFETY: a null transform is allowed; the radius fits the rectangle as Core Graphics
        // requires.
        Some(unsafe { CGPath::with_rounded_rect(rect, rad, rad, std::ptr::null()) })
    }

    fn circle(&self, cx: f32, cy: f32, r: f32, c: Color) {
        if r <= 0.0 {
            return;
        }
        self.fill(c);
        CGContext::fill_ellipse_in_rect(Some(self.ctx), self.square(cx, cy, r));
    }

    fn ring(&self, cx: f32, cy: f32, r: f32, w: f32, c: Color) {
        if r <= 0.0 || w <= 0.0 {
            return;
        }
        self.stroke(c, w);
        CGContext::stroke_ellipse_in_rect(Some(self.ctx), self.square(cx, cy, r));
    }

    fn rrect(&self, l: f32, t: f32, r: f32, b: f32, radius: f32, c: Color) {
        let Some(path) = self.rounded(l, t, r, b, radius) else { return };
        self.fill(c);
        CGContext::add_path(Some(self.ctx), Some(&path));
        CGContext::fill_path(Some(self.ctx));
    }

    #[allow(clippy::too_many_arguments)]
    fn rrect_stroke(&self, l: f32, t: f32, r: f32, b: f32, radius: f32, w: f32, c: Color) {
        if w <= 0.0 {
            return;
        }
        let Some(path) = self.rounded(l, t, r, b, radius) else { return };
        self.stroke(c, w);
        CGContext::add_path(Some(self.ctx), Some(&path));
        CGContext::stroke_path(Some(self.ctx));
    }

    /// One line in the system font, its line box centred on (cx, cy), with the scene's letter
    /// spacing after every glyph (the last one included), over its shadow.
    fn text(&self, text: &str, size: f32, cx: f32, cy: f32, color: Color, shadow: Option<Color>) {
        if text.is_empty() || size <= 0.0 {
            return;
        }
        // SAFETY: a plain font lookup; no language is passed.
        let Some(font) = (unsafe { CTFont::new_ui_font_for_language(CTFontUIFontType::System, f64::from(size), None) })
        else {
            return;
        };
        let runs = glyph_runs(&font, text);
        let spacing = f64::from(LETTER_SPACING_EM * size);
        let width: f64 = runs.iter().flat_map(|r| r.advances.iter()).map(|a| a + spacing).sum();
        // SAFETY: plain metric reads of a live font.
        let (ascent, descent) = unsafe { (font.ascent(), font.descent()) };
        let baseline = f64::from(cy) + (ascent - descent) / 2.0;
        let x0 = f64::from(cx) - width / 2.0;
        // SAFETY: reading an immutable extern constant of Core Graphics.
        CGContext::set_text_matrix(Some(self.ctx), unsafe { CGAffineTransformIdentity });
        let layers = shadow.map(|s| (f64::from(TEXT_SHADOW_DY), s)).into_iter().chain([(0.0, color)]);
        for (dy, c) in layers {
            self.fill(c);
            let y = self.h - (baseline + dy);
            let mut x = x0;
            for run in &runs {
                let mut pos = Vec::with_capacity(run.glyphs.len());
                for a in &run.advances {
                    pos.push(CGPoint::new(x, y));
                    x += a + spacing;
                }
                let mut glyphs = run.glyphs.clone();
                // SAFETY: `glyphs` and `pos` hold the same, non-zero number of elements and live
                // across the call; the context is a live bitmap context.
                unsafe {
                    run.font.draw_glyphs(
                        NonNull::new_unchecked(glyphs.as_mut_ptr()),
                        NonNull::new_unchecked(pos.as_mut_ptr()),
                        glyphs.len(),
                        self.ctx,
                    );
                }
            }
        }
    }

    fn draw(&self, scene: &Scene) {
        for p in &scene.prims {
            match p {
                Prim::Circle { cx, cy, r, color } => self.circle(*cx, *cy, *r, *color),
                Prim::Ring { cx, cy, r, w, color } => self.ring(*cx, *cy, *r, *w, *color),
                Prim::RoundRect { l, t, r, b, radius, color } => self.rrect(*l, *t, *r, *b, *radius, *color),
                Prim::RoundRectStroke { l, t, r, b, radius, w, color } => {
                    self.rrect_stroke(*l, *t, *r, *b, *radius, *w, *color)
                }
                Prim::Text { text, size, cx, cy, color, shadow } => self.text(text, *size, *cx, *cy, *color, *shadow),
            }
        }
    }
}

/// A scene drawn into a bitmap of `w`×`h` pixels.
struct Drawn {
    ctx: CFRetained<CGContext>,
    w: usize,
    h: usize,
}

impl Drawn {
    fn image(&self) -> Option<CFRetained<CGImage>> {
        CGBitmapContextCreateImage(Some(&self.ctx))
    }

    /// Premultiplied BGRA, top row first.
    fn pixels(&self) -> Vec<u8> {
        context_pixels(&self.ctx, self.w, self.h)
    }
}

/// Draw `scene` at `scale` pixels per DIP, anti-aliased, with grey-scale text.
fn render(scene: &Scene, scale: f64) -> Result<Drawn, String> {
    let scale = if scale.is_finite() && scale > 0.0 { scale } else { 1.0 };
    let w = (f64::from(scene.w) * scale).round().max(1.0) as usize;
    let h = (f64::from(scene.h) * scale).round().max(1.0) as usize;
    let ctx = new_context(w, h).ok_or_else(|| format!("bitmap context {w}x{h} could not be created"))?;
    CGContext::set_should_antialias(Some(&ctx), true);
    CGContext::set_should_smooth_fonts(Some(&ctx), false);
    CGContext::scale_ctm(Some(&ctx), scale, scale);
    Painter { ctx: &ctx, h: f64::from(scene.h) }.draw(scene);
    Ok(Drawn { ctx, w, h })
}

// -------------------------------------------------------------------------------- window

define_class!(
    // SAFETY: NSPanel has no subclassing requirements; the class only answers two queries and
    // implements no `Drop`.
    #[unsafe(super(NSPanel, NSWindow, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "SessionmuxFloatingPanel"]
    pub(crate) struct FloatingPanel;

    impl FloatingPanel {
        /// Never the window that receives key presses.
        #[unsafe(method(canBecomeKeyWindow))]
        fn can_become_key_window(&self) -> bool {
            false
        }

        #[unsafe(method(canBecomeMainWindow))]
        fn can_become_main_window(&self) -> bool {
            false
        }
    }
);

impl FloatingPanel {
    /// A transparent, borderless, non-activating panel at `frame` (screen coordinates), above
    /// normal windows on every Space and beside full-screen apps, kept out of window cycling.
    /// It is not shown yet.
    pub(crate) fn create(mtm: MainThreadMarker, frame: NSRect) -> Retained<FloatingPanel> {
        let this = Self::alloc(mtm).set_ivars(());
        let style = NSWindowStyleMask::Borderless | NSWindowStyleMask::NonactivatingPanel;
        // SAFETY: NSPanel's designated initialiser with a valid style and backing type.
        let panel: Retained<FloatingPanel> = unsafe {
            msg_send![
                super(this),
                initWithContentRect: frame,
                styleMask: style,
                backing: NSBackingStoreType::Buffered,
                defer: false
            ]
        };
        // SAFETY: the window is owned by the returned `Retained` alone, so closing it must not
        // release it a second time.
        unsafe { panel.setReleasedWhenClosed(false) };
        panel.setFloatingPanel(true);
        panel.setLevel(NSStatusWindowLevel);
        panel.setOpaque(false);
        panel.setBackgroundColor(Some(&NSColor::clearColor()));
        panel.setHasShadow(false);
        panel.setHidesOnDeactivate(false);
        panel.setBecomesKeyOnlyIfNeeded(true);
        panel.setCollectionBehavior(
            NSWindowCollectionBehavior::CanJoinAllSpaces
                | NSWindowCollectionBehavior::FullScreenAuxiliary
                | NSWindowCollectionBehavior::Stationary
                | NSWindowCollectionBehavior::IgnoresCycle,
        );
        panel.setAnimationBehavior(NSWindowAnimationBehavior::None);
        panel
    }
}

/// A layer the caller fills, hosted by `view` (AppKit never draws into it), and the view's
/// layer.
pub(crate) fn host_layer(view: &NSView) -> Retained<CALayer> {
    let layer = CALayer::new();
    view.setLayer(Some(&layer));
    view.setWantsLayer(true);
    layer
}

/// Show `image` (drawn at `scale` pixels per point) as the layer's contents, at once.
pub(crate) fn set_layer_image(layer: &CALayer, image: &CGImage, scale: f64) {
    CATransaction::begin();
    CATransaction::setDisableActions(true);
    layer.setContentsScale(scale);
    // SAFETY: a CGImage is a valid object for a layer's contents (Core Foundation types are
    // Objective-C objects); the layer retains it.
    unsafe {
        let obj = &*(image as *const CGImage as *const AnyObject);
        layer.setContents(Some(obj));
    }
    CATransaction::commit();
}

struct ViewIvars {
    inner: RefCell<Weak<Inner>>,
}

define_class!(
    // SAFETY: NSView has no subclassing requirements beyond its designated initialiser, which
    // `BubbleView::new` calls; the class implements no `Drop`.
    #[unsafe(super(NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "SessionmuxBubbleView"]
    #[ivars = ViewIvars]
    struct BubbleView;

    impl BubbleView {
        /// Top-left origin, so points in the view are the scene's coordinates.
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }

        /// The first click acts even while Sessionmux is not the active app.
        #[unsafe(method(acceptsFirstMouse:))]
        fn accepts_first_mouse(&self, _event: Option<&NSEvent>) -> bool {
            true
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, event: &NSEvent) {
            self.route(|inner| inner.mouse_down(self.point(event), event));
        }

        #[unsafe(method(mouseDragged:))]
        fn mouse_dragged(&self, _event: &NSEvent) {
            self.route(|inner| inner.mouse_dragged());
        }

        #[unsafe(method(mouseUp:))]
        fn mouse_up(&self, event: &NSEvent) {
            self.route(|inner| inner.mouse_up(self.point(event)));
        }

        #[unsafe(method(rightMouseDown:))]
        fn right_mouse_down(&self, _event: &NSEvent) {
            self.route(|inner| inner.right_click());
        }

        #[unsafe(method(mouseMoved:))]
        fn mouse_moved(&self, event: &NSEvent) {
            self.route(|inner| inner.hover(Some(self.point(event))));
        }

        #[unsafe(method(mouseEntered:))]
        fn mouse_entered(&self, event: &NSEvent) {
            self.route(|inner| inner.hover(Some(self.point(event))));
        }

        #[unsafe(method(mouseExited:))]
        fn mouse_exited(&self, _event: &NSEvent) {
            self.route(|inner| inner.hover(None));
        }

        /// Moved to a display with another backing scale: redraw at the new one.
        #[unsafe(method(viewDidChangeBackingProperties))]
        fn backing_changed(&self) {
            // SAFETY: the superclass method takes no arguments and returns nothing.
            let _: () = unsafe { msg_send![super(self), viewDidChangeBackingProperties] };
            self.route(|inner| {
                inner.sync_scale();
                None
            });
        }

        /// Displays were added, removed, rearranged or resized.
        #[unsafe(method(screensChanged:))]
        fn screens_changed(&self, _n: &NSNotification) {
            self.route(|inner| {
                inner.displays_changed();
                None
            });
        }
    }
);

impl BubbleView {
    fn new(mtm: MainThreadMarker, frame: NSRect) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(ViewIvars { inner: RefCell::new(Weak::new()) });
        // SAFETY: NSView's designated initialiser.
        unsafe { msg_send![super(this), initWithFrame: frame] }
    }

    /// Where `event` happened, in the view (DIPs, top-left origin).
    fn point(&self, event: &NSEvent) -> (f32, f32) {
        let p = self.convertPoint_fromView(event.locationInWindow(), None);
        (p.x as f32, p.y as f32)
    }

    /// Run `f` on the bubble this view belongs to, then deliver the event it returns with no
    /// borrow held. Nothing happens once the bubble is gone.
    fn route(&self, f: impl FnOnce(&Inner) -> Option<BubbleEvent>) {
        let Some(inner) = self.ivars().inner.borrow().upgrade() else { return };
        if let Some(ev) = f(&inner) {
            if matches!(ev, BubbleEvent::AvatarClick(_)) {
                // Still inside the click: Sessionmux becomes the active app, so it can pass the
                // activation on to the account's Claude.
                super::claude::take_activation(self.mtm());
            }
            (inner.on_event)(ev);
        }
    }
}

fn set_cursor(hand: bool) {
    if hand {
        NSCursor::pointingHandCursor().set();
    } else {
        NSCursor::arrowCursor().set();
    }
}

struct Press {
    /// Pointer position (screen coordinates) at the press.
    start: NSPoint,
    /// Window origin (screen coordinates) at the press.
    origin: NSPoint,
    target: Hit,
    dragging: bool,
}

struct Model {
    state: BubbleState,
    /// Backing scale the shown frame was drawn at.
    scale: f64,
    visual: Visual,
    press: Option<Press>,
    /// A frame has been shown.
    drawn: bool,
}

struct Inner {
    mtm: MainThreadMarker,
    window: Retained<FloatingPanel>,
    view: Retained<BubbleView>,
    layer: Retained<CALayer>,
    model: RefCell<Model>,
    on_event: Box<dyn Fn(BubbleEvent)>,
}

impl Inner {
    fn rect(&self) -> Rect {
        top_left_rect(self.window.frame(), primary_height(self.mtm))
    }

    fn move_to(&self, x: i32, y: i32) {
        let h = self.window.frame().size.height;
        let ph = primary_height(self.mtm);
        self.window.setFrameOrigin(NSPoint::new(f64::from(x), ph - f64::from(y) - h));
    }

    fn backing_scale(&self) -> f64 {
        let s = self.window.backingScaleFactor();
        if s.is_finite() && s > 0.0 {
            s
        } else {
            1.0
        }
    }

    /// Size the window for the current state (at `pos`, or where it is), then draw and show the
    /// frame at the backing scale of the display it is on.
    fn present(&self, m: &mut Model, pos: Option<(i32, i32)>) {
        let (w, h) = dip_size(effective_mode(&m.state), m.state.accounts.len());
        let (w, h) = (w.round() as i32, h.round() as i32);
        let cur = self.rect();
        let (x, y) = pos.unwrap_or((cur.x, cur.y));
        if pos.is_some() || (cur.w, cur.h) != (w, h) {
            let frame = screen_frame(&Rect { x, y, w, h }, primary_height(self.mtm));
            self.window.setFrame_display(frame, true);
        }
        let scale = self.backing_scale();
        match render(&model::scene(&m.state, m.visual), scale) {
            Ok(d) => match d.image() {
                Some(img) => {
                    set_layer_image(&self.layer, &img, scale);
                    m.scale = scale;
                    m.drawn = true;
                }
                None => log::warn("bubble present: the frame could not be turned into an image"),
            },
            Err(e) => log::warn(&format!("bubble render: {e}")),
        }
    }

    /// Redraw keeping the snapped edge when the size changes (mode, account count).
    fn relayout(&self, m: &mut Model) {
        let (w, h) = dip_size(effective_mode(&m.state), m.state.accounts.len());
        let (w, h) = (w.round() as i32, h.round() as i32);
        let cur = self.rect();
        let pos = if (cur.w, cur.h) != (w, h) { Some(anchored_rect(&cur, w, h, &work_area_for(&cur))) } else { None };
        self.present(m, pos);
    }

    /// Redraw when the backing scale changed. A change reported while the model is borrowed (a
    /// drag moving the window to another display) is caught up by the end of the drag.
    fn sync_scale(&self) {
        let Ok(mut m) = self.model.try_borrow_mut() else { return };
        if m.drawn && (self.backing_scale() - m.scale).abs() > 0.001 {
            self.present(&mut m, None);
        }
    }

    /// Keep the window inside the visible area of its display after a display change.
    fn displays_changed(&self) {
        let Ok(mut m) = self.model.try_borrow_mut() else { return };
        let cur = self.rect();
        let pos = clamp_to_area(cur.x, cur.y, cur.w, cur.h, &work_area_for(&cur));
        self.present(&mut m, Some(pos));
    }

    /// Snap to the nearest left / right edge of the display's visible area; the new top-left.
    fn snap(&self) -> (i32, i32) {
        let r = self.rect();
        let (x, y) = snap_rect(&r, &work_area_for(&r), EDGE_MARGIN_DIP.round() as i32);
        self.move_to(x, y);
        (x, y)
    }

    fn hit(m: &Model, (x, y): (f32, f32)) -> Hit {
        hit_test(effective_mode(&m.state), m.state.accounts.len(), x, y)
    }

    fn mouse_down(&self, at: (f32, f32), event: &NSEvent) -> Option<BubbleEvent> {
        if event.modifierFlags().contains(NSEventModifierFlags::Control) {
            return self.right_click();
        }
        let Ok(mut m) = self.model.try_borrow_mut() else { return None };
        let hit = Self::hit(&m, at);
        if hit == Hit::None || m.press.is_some() {
            return None;
        }
        m.press = Some(Press {
            start: NSEvent::mouseLocation(),
            origin: self.window.frame().origin,
            target: hit,
            dragging: false,
        });
        m.visual.pressed = hit;
        m.visual.hover = hit;
        self.present(&mut m, None);
        None
    }

    fn mouse_dragged(&self) -> Option<BubbleEvent> {
        let Ok(mut m) = self.model.try_borrow_mut() else { return None };
        let size = self.window.frame().size;
        let p = m.press.as_mut()?;
        let loc = NSEvent::mouseLocation();
        let (dx, dy) = (loc.x - p.start.x, loc.y - p.start.y);
        let mut started = false;
        if !p.dragging && dx.hypot(dy) >= f64::from(DRAG_THRESHOLD_DIP) {
            p.dragging = true;
            started = true;
        }
        if p.dragging {
            let ph = primary_height(self.mtm);
            let (w, h) = (size.width.round() as i32, size.height.round() as i32);
            let x = (p.origin.x + dx).round() as i32;
            let y = (ph - (p.origin.y + dy) - size.height).round() as i32;
            let (x, y) = clamp_to_area(x, y, w, h, &work_area_for(&Rect { x, y, w, h }));
            self.move_to(x, y);
        }
        if started {
            m.visual.dragging = true;
            m.visual.hover = Hit::None;
            self.present(&mut m, None);
            set_cursor(false);
        }
        None
    }

    fn mouse_up(&self, at: (f32, f32)) -> Option<BubbleEvent> {
        let Ok(mut m) = self.model.try_borrow_mut() else { return None };
        let p = m.press.take()?;
        m.visual.pressed = Hit::None;
        m.visual.dragging = false;
        if p.dragging {
            drop(m);
            self.sync_scale();
            let (x, y) = self.snap();
            if let Ok(mut m) = self.model.try_borrow_mut() {
                // The pointer is where it was pressed in the window, which has moved with it.
                m.visual.hover = Self::hit(&m, at);
                self.present(&mut m, None);
                set_cursor(m.visual.hover.clickable());
            }
            return Some(BubbleEvent::DragEnd(x, y));
        }
        let ev = match p.target {
            Hit::Bubble => Some(BubbleEvent::Click),
            Hit::Avatar(i) => m.state.accounts.get(i).map(|a| BubbleEvent::AvatarClick(a.id.clone())),
            Hit::More => Some(BubbleEvent::MoreClick),
            Hit::Grip | Hit::None => None,
        };
        m.visual.hover = Self::hit(&m, at);
        self.present(&mut m, None);
        ev
    }

    fn right_click(&self) -> Option<BubbleEvent> {
        let m = self.model.try_borrow().ok()?;
        m.press.is_none().then_some(BubbleEvent::RightClick)
    }

    /// The pointer moved over the window (`Some`, in DIPs) or left it (`None`).
    fn hover(&self, at: Option<(f32, f32)>) -> Option<BubbleEvent> {
        let Ok(mut m) = self.model.try_borrow_mut() else { return None };
        if m.press.is_some() {
            return None;
        }
        let hit = at.map(|p| Self::hit(&m, p)).unwrap_or(Hit::None);
        set_cursor(hit.clickable());
        if hit != m.visual.hover {
            m.visual.hover = hit;
            self.present(&mut m, None);
        }
        None
    }
}

// ------------------------------------------------------------------------------- public API

pub struct Bubble {
    inner: Rc<Inner>,
}

impl BubbleHost for Bubble {
    fn create(pos: Option<(i32, i32)>, on_event: Box<dyn Fn(BubbleEvent)>) -> Result<Bubble, String> {
        let mtm = MainThreadMarker::new()
            .ok_or_else(|| "the floating button must be created on the main thread".to_string())?;
        let (w, h) = dip_size(BubbleMode::Icon, 0);
        let (w, h) = (w.round() as i32, h.round() as i32);
        let (x, y) = match pos {
            Some((x, y)) => clamp_to_area(x, y, w, h, &work_area_for(&Rect { x, y, w, h })),
            None => {
                let a = work_area_for(&Rect { x: 0, y: 0, w: 1, h: 1 });
                let m = DEFAULT_MARGIN_DIP.round() as i32;
                (a.x + a.w - w - m, a.y + a.h - h - m)
            }
        };
        let frame = screen_frame(&Rect { x, y, w, h }, primary_height(mtm));
        let window = FloatingPanel::create(mtm, frame);
        window.setAcceptsMouseMovedEvents(true);
        let view = BubbleView::new(mtm, NSRect::new(NSPoint::new(0.0, 0.0), frame.size));
        let layer = host_layer(&view);
        let options = NSTrackingAreaOptions::MouseEnteredAndExited
            | NSTrackingAreaOptions::MouseMoved
            | NSTrackingAreaOptions::ActiveAlways
            | NSTrackingAreaOptions::InVisibleRect;
        let owner: &AnyObject = &view;
        // SAFETY: the view implements the entered / moved / exited handlers the area calls and
        // outlives it (the area belongs to the view); no user info is passed.
        let area = unsafe {
            NSTrackingArea::initWithRect_options_owner_userInfo(
                NSTrackingArea::alloc(),
                NSRect::ZERO,
                options,
                Some(owner),
                None,
            )
        };
        view.addTrackingArea(&area);
        window.setContentView(Some(&view));

        let state = BubbleState { mode: BubbleMode::Icon, active: None, accounts: Vec::new(), waiting: 0 };
        let inner = Rc::new(Inner {
            mtm,
            window,
            view,
            layer,
            model: RefCell::new(Model { state, scale: 1.0, visual: Visual::default(), press: None, drawn: false }),
            on_event,
        });
        *inner.view.ivars().inner.borrow_mut() = Rc::downgrade(&inner);
        // SAFETY: the view implements `screensChanged:` taking the notification; it is removed
        // as an observer when the bubble is dropped, while it is still alive.
        unsafe {
            NSNotificationCenter::defaultCenter().addObserver_selector_name_object(
                &inner.view,
                sel!(screensChanged:),
                Some(NSApplicationDidChangeScreenParametersNotification),
                None,
            );
        }
        {
            let mut m = inner.model.borrow_mut();
            inner.present(&mut m, Some((x, y)));
        }
        let b = Bubble { inner };
        b.show(true);
        Ok(b)
    }

    /// Apply a new state: resize (anchored to the snapped edge) and redraw.
    fn set_state(&mut self, s: BubbleState) {
        let mut s = s;
        s.accounts.truncate(MAX_TRAY);
        let Ok(mut m) = self.inner.model.try_borrow_mut() else {
            log::warn("bubble: set_state while busy; ignored");
            return;
        };
        if m.state == s && m.drawn {
            return;
        }
        if effective_mode(&m.state) != effective_mode(&s) || m.state.accounts.len() != s.accounts.len() {
            m.visual.hover = Hit::None;
        }
        m.state = s;
        self.inner.relayout(&mut m);
    }

    fn show(&self, visible: bool) {
        if visible {
            self.inner.window.orderFrontRegardless();
        } else {
            self.inner.window.orderOut(None);
        }
    }

    fn is_visible(&self) -> bool {
        self.inner.window.isVisible()
    }

    fn rect(&self) -> Rect {
        self.inner.rect()
    }

    fn set_position(&self, x: i32, y: i32) {
        self.inner.move_to(x, y);
    }

    /// Draw the shown state at a Retina scale and write it as PNG (straight alpha).
    fn render_to_png(&self, path: &Path) -> Result<(), String> {
        let m = self.inner.model.try_borrow().map_err(|_| "bubble busy".to_string())?;
        if !m.drawn {
            return Err("bubble: nothing rendered yet".into());
        }
        let d = render(&model::scene(&m.state, m.visual), PNG_SCALE)?;
        write_png(path, d.w, d.h, &d.pixels())
    }
}

impl Drop for Bubble {
    fn drop(&mut self) {
        let inner = &self.inner;
        // SAFETY: the view was added as an observer in `create` and is still alive.
        unsafe { NSNotificationCenter::defaultCenter().removeObserver(&inner.view) };
        *inner.view.ivars().inner.borrow_mut() = Weak::new();
        inner.window.orderOut(None);
        inner.window.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::bubble::model::{ACCENT, WHITE};
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

    /// Straight-alpha (r, g, b, a) of the pixel at (x, y) of premultiplied BGRA pixels.
    fn pixel(px: &[u8], w: usize, x: usize, y: usize) -> (u8, u8, u8, u8) {
        let i = (y * w + x) * 4;
        let s = unpremultiply(&px[i..i + 4]);
        (s[2], s[1], s[0], s[3])
    }

    fn near(a: (u8, u8, u8, u8), rgb: u32) -> bool {
        let want = [(rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8];
        [a.0, a.1, a.2].iter().zip(want).all(|(&g, w)| g.abs_diff(w) <= 3) && a.3 == 255
    }

    #[test]
    fn frames_flip_between_screen_and_top_left_coordinates() {
        let r = Rect { x: 100, y: 25, w: 80, h: 80 };
        let f = screen_frame(&r, 900.0);
        assert_eq!((f.origin.x, f.origin.y, f.size.width, f.size.height), (100.0, 795.0, 80.0, 80.0));
        assert_eq!(top_left_rect(f, 900.0), r);
        // A display above the primary one has a negative top.
        let above = Rect { x: 0, y: -1080, w: 230, h: 72 };
        assert_eq!(top_left_rect(screen_frame(&above, 900.0), 900.0), above);
    }

    #[test]
    fn alpha_is_unpremultiplied() {
        assert_eq!(unpremultiply(&[10, 20, 30, 255]), [10, 20, 30, 255]);
        assert_eq!(unpremultiply(&[64, 32, 0, 128]), [128, 64, 0, 128]);
        assert_eq!(unpremultiply(&[5, 5, 5, 0]), [0, 0, 0, 0]);
        assert_eq!(unpremultiply(&[200, 0, 0, 100]), [255, 0, 0, 100]);
    }

    #[test]
    fn radii_fit_their_rectangle() {
        assert_eq!(fit_radius(100.0, 48.0, 24.0), 24.0);
        assert_eq!(fit_radius(100.0, 48.0, 30.0), 24.0);
        assert_eq!(fit_radius(10.0, 48.0, -1.0), 0.0);
    }

    #[test]
    fn icon_frames_are_drawn_at_the_backing_scale() {
        let st =
            BubbleState { mode: BubbleMode::Icon, active: Some(fake(1)[0].clone()), accounts: fake(1), waiting: 0 };
        let d = render(&model::scene(&st, Visual::default()), 2.0).unwrap();
        assert_eq!((d.w, d.h), (160, 160));
        let px = d.pixels();
        assert_eq!(px.len(), 160 * 160 * 4);
        // Corners are transparent; the accent ring and the account colour are where the scene
        // puts them (centre 40, radius 26 DIPs; ring 2.5 DIPs wide).
        assert_eq!(pixel(&px, 160, 0, 0).3, 0);
        assert!(near(pixel(&px, 160, 80, 80 - 51), ACCENT), "ring: {:?}", pixel(&px, 160, 80, 29));
        assert!(near(pixel(&px, 160, 80 - 36, 80), 0xF08A5D), "fill: {:?}", pixel(&px, 160, 44, 80));
        // The initials are drawn in white inside the circle.
        let white = (60..100)
            .flat_map(|y| (50..110).map(move |x| (x, y)))
            .filter(|&(x, y)| near(pixel(&px, 160, x, y), WHITE))
            .count();
        assert!(white > 20, "{white} white pixels");
    }

    #[test]
    fn tray_frames_match_the_layout() {
        let st = BubbleState { mode: BubbleMode::Tray, active: None, accounts: fake(3), waiting: 12 };
        let d = render(&model::scene(&st, Visual::default()), 1.0).unwrap();
        assert_eq!((d.w, d.h), (230, 72));
        let px = d.pixels();
        // Outside the pill it is clear; the first avatar's colour fills its slot (centre 48, 36).
        assert_eq!(pixel(&px, 230, 1, 1).3, 0);
        assert!(near(pixel(&px, 230, 48 - 13, 36), 0xF08A5D), "{:?}", pixel(&px, 230, 35, 36));
    }

    #[test]
    fn text_from_other_scripts_and_emoji_is_drawn() {
        // SAFETY: a plain font lookup; no language is passed.
        let font = unsafe { CTFont::new_ui_font_for_language(CTFontUIFontType::System, 15.0, None) }.unwrap();
        let runs = glyph_runs(&font, "AБ");
        assert_eq!(runs.iter().map(|r| r.glyphs.len()).sum::<usize>(), 2);
        let emoji = glyph_runs(&font, "😀");
        assert_eq!(emoji.iter().map(|r| r.glyphs.len()).sum::<usize>(), 1);
        assert!(emoji[0].advances[0] > 0.0);
        assert!(glyph_runs(&font, "").is_empty());
    }

    #[test]
    fn png_captures_have_straight_alpha() {
        let dir = std::env::temp_dir().join(format!("sessionmux-bubble-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let st = BubbleState { mode: BubbleMode::Icon, active: None, accounts: vec![], waiting: 0 };
        let d = render(&model::scene(&st, Visual::default()), PNG_SCALE).unwrap();
        let path = dir.join("bubble.png");
        write_png(&path, d.w, d.h, &d.pixels()).unwrap();
        let png = std::fs::read(&path).unwrap();
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
        assert_eq!(u32::from_be_bytes(png[16..20].try_into().unwrap()), 160);
        assert_eq!(u32::from_be_bytes(png[20..24].try_into().unwrap()), 160);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn images_are_made_from_pixels() {
        let px = vec![255u8; 4 * 3 * 2];
        let img = image_from_bgra(&px, 3, 2).unwrap();
        assert_eq!((CGImage::width(Some(&img)), CGImage::height(Some(&img))), (3, 2));
        assert!(image_from_bgra(&px, 4, 4).is_none());
    }

    #[test]
    fn creation_needs_the_main_thread() {
        // Tests normally run on worker threads; on the main thread there is nothing to check.
        if MainThreadMarker::new().is_some() {
            return;
        }
        assert!(Bubble::create(None, Box::new(|_| {})).is_err());
    }
}

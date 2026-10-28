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

//! The tamper gesture's window on macOS: the bubble's kind of floating panel, `WIN_PX` points
//! square, that ignores the mouse (clicks go to whatever is below) and never activates. Each step
//! of `gesture::timeline` shows its frame as the contents of the window's layer, drawn with
//! `gesture::pixel_scale` pixels per point, at the step's opacity; a one-shot run-loop timer
//! moves to the next step and the window closes after the last one.

use std::cell::RefCell;
use std::path::Path;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::{MainThreadMarker, MainThreadOnly};
use objc2_app_kit::NSView;
use objc2_core_foundation::{kCFRunLoopCommonModes, CFAbsoluteTimeGetCurrent, CFRetained, CFRunLoop, CFRunLoopTimer};
use objc2_core_graphics::CGImage;
use objc2_foundation::{NSPoint, NSRect};
use objc2_quartz_core::CALayer;

use super::bubble::{
    host_layer, image_from_bgra, primary_height, screen_frame, set_layer_image, write_png, FloatingPanel,
};
use crate::platform::{Desktop, EventLoop, GestureHost, Os, Rect};
use crate::ui::gesture::{pixel_scale, place, render, timeline, Step, FRAMES, GAP_PX, WIN_PX};
use crate::util::log;

struct Live {
    window: Retained<FloatingPanel>,
    layer: Retained<CALayer>,
    steps: Vec<Step>,
    at: usize,
    /// Pixels per point of the frames.
    k: i32,
    /// Each frame's image once drawn.
    images: Vec<Option<CFRetained<CGImage>>>,
    /// The timer that shows the next step.
    timer: Option<CFRetained<CFRunLoopTimer>>,
}

thread_local! {
    static LIVE: RefCell<Option<Live>> = const { RefCell::new(None) };
}

/// Where the window goes: beside `anchor` (the bubble) on its display, or in the bottom-right
/// corner of the primary display.
fn placement(anchor: Option<&Rect>) -> Rect {
    let probe = anchor.copied().unwrap_or(Rect { x: 0, y: 0, w: 1, h: 1 });
    let area = <Os as Desktop>::work_area(&probe);
    let (x, y) = place(anchor, &area, WIN_PX, GAP_PX);
    Rect { x, y, w: WIN_PX, h: WIN_PX }
}

/// Call `tick` once after `ms` milliseconds, also while a menu is open.
fn one_shot(ms: u32) -> Option<CFRetained<CFRunLoopTimer>> {
    let block = RcBlock::new(|_t: *mut CFRunLoopTimer| tick());
    let fire = CFAbsoluteTimeGetCurrent() + f64::from(ms.max(1)) / 1000.0;
    // SAFETY: the default allocator; an interval of 0 makes a one-shot timer; the timer copies the
    // block, which captures nothing.
    let timer = unsafe { CFRunLoopTimer::with_handler(None, fire, 0.0, 0, 0, Some(&block)) }?;
    // SAFETY: reading an immutable extern constant of Core Foundation.
    CFRunLoop::main()?.add_timer(Some(&timer), unsafe { kCFRunLoopCommonModes });
    Some(timer)
}

/// Show the current step and arm the timer for the next one.
fn show_step(l: &mut Live) {
    let s = l.steps[l.at];
    if l.images[s.frame].is_none() {
        let (px, side) = render(s.frame, l.k);
        l.images[s.frame] = image_from_bgra(&px, side as usize, side as usize);
    }
    match &l.images[s.frame] {
        Some(img) => set_layer_image(&l.layer, img, f64::from(l.k)),
        None => log::debug("gesture: a frame could not be drawn"),
    }
    l.window.setAlphaValue(f64::from(s.alpha) / 255.0);
    if let Some(t) = l.timer.take() {
        t.invalidate();
    }
    l.timer = one_shot(s.ms);
}

fn start(mtm: MainThreadMarker, anchor: Option<Rect>) {
    let frame = screen_frame(&placement(anchor.as_ref()), primary_height(mtm));
    let (window, layer) = match LIVE.with(|l| l.borrow_mut().take()) {
        Some(old) => {
            if let Some(t) = old.timer {
                t.invalidate();
            }
            (old.window, old.layer)
        }
        None => {
            let window = FloatingPanel::create(mtm, frame);
            window.setIgnoresMouseEvents(true);
            let view = NSView::initWithFrame(NSView::alloc(mtm), NSRect::new(NSPoint::new(0.0, 0.0), frame.size));
            let layer = host_layer(&view);
            window.setContentView(Some(&view));
            (window, layer)
        }
    };
    window.setFrame_display(frame, false);
    let k = pixel_scale(window.backingScaleFactor() as f32);
    let mut live = Live { window, layer, steps: timeline(), at: 0, k, images: vec![None; FRAMES.len()], timer: None };
    show_step(&mut live);
    live.window.orderFrontRegardless();
    LIVE.with(|l| *l.borrow_mut() = Some(live));
}

/// Advance one step; close the window after the last one.
fn tick() {
    let done = LIVE.with(|cell| {
        let mut g = cell.borrow_mut();
        let l = g.as_mut()?;
        l.at += 1;
        if l.at < l.steps.len() {
            show_step(l);
            None
        } else {
            g.take()
        }
    });
    if let Some(l) = done {
        destroy(l);
    }
}

fn destroy(l: Live) {
    if let Some(t) = l.timer {
        t.invalidate();
    }
    l.window.orderOut(None);
    l.window.close();
}

/// Whether the animation is currently on screen.
#[cfg(test)]
fn is_playing() -> bool {
    LIVE.with(|l| l.borrow().is_some())
}

impl GestureHost for Os {
    /// Returns at once; run-loop timers drive the rest. Called off the main thread, it plays
    /// from there.
    fn play(anchor: Option<Rect>) {
        match MainThreadMarker::new() {
            Some(mtm) => start(mtm, anchor),
            None => <Os as EventLoop>::post(Box::new(move || {
                if let Some(mtm) = MainThreadMarker::new() {
                    start(mtm, anchor);
                }
            })),
        }
    }

    fn close() {
        if MainThreadMarker::new().is_none() {
            <Os as EventLoop>::post(Box::new(<Os as GestureHost>::close));
            return;
        }
        if let Some(l) = LIVE.with(|l| l.borrow_mut().take()) {
            destroy(l);
        }
    }

    fn render_to_png(path: &Path) -> Result<(), String> {
        let (px, side) = render(FRAMES.len() - 1, 1);
        write_png(path, side as usize, side as usize, &px)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_held_frame_is_written_at_full_size() {
        let dir = std::env::temp_dir().join(format!("sessionmux-gesture-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("gesture.png");
        <Os as GestureHost>::render_to_png(&path).unwrap();
        let png = std::fs::read(&path).unwrap();
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
        assert_eq!(u32::from_be_bytes(png[16..20].try_into().unwrap()), WIN_PX as u32);
        assert_eq!(u32::from_be_bytes(png[20..24].try_into().unwrap()), WIN_PX as u32);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn frames_become_images_at_each_pixel_scale() {
        for k in [1, 2] {
            let (px, side) = render(FRAMES.len() - 1, k);
            let img = image_from_bgra(&px, side as usize, side as usize).unwrap();
            assert_eq!(CGImage::width(Some(&img)), (WIN_PX * k) as usize);
        }
    }

    #[test]
    fn placement_stays_on_the_display() {
        // Off the main thread the display geometry is nominal; the rules are the same.
        if MainThreadMarker::new().is_some() {
            return;
        }
        let r = placement(None);
        assert_eq!((r.w, r.h), (WIN_PX, WIN_PX));
        let area = <Os as Desktop>::work_area(&r);
        assert_eq!((r.x + r.w + GAP_PX, r.y + r.h + GAP_PX), (area.x + area.w, area.y + area.h));
        let bubble = Rect { x: area.x + area.w - 92, y: area.y + 300, w: 80, h: 80 };
        let r = placement(Some(&bubble));
        assert_eq!((r.x, r.y), (bubble.x - GAP_PX - WIN_PX, bubble.y + bubble.h - WIN_PX));
    }

    #[test]
    fn playing_off_the_main_thread_shows_nothing_here() {
        if MainThreadMarker::new().is_some() {
            return;
        }
        Os::play(None);
        <Os as GestureHost>::close();
        assert!(!is_playing());
    }
}

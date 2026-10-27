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
//! This module holds what both systems share: the art, six `const` grids of [`COLS`]×[`ROWS`]
//! cells, [`CELL_PX`] px per cell, rendered by [`render`] without any anti-aliasing (hand
//! `#E3B58A`, outline `#101114` and a flat 25 % black drop shadow one cell down and right), the
//! [`timeline`] (five frames of 90 ms, the last frame for 90 + 1500 ms, then six 50 ms fade
//! steps) and the placement beside the bubble ([`place`]). The window is `platform::gesture`: a
//! click-through, never activated [`WIN_PX`]² window, destroyed when the timeline ends. Playing
//! again while visible restarts it.

use std::time::{Duration, Instant};

use crate::platform::Rect;

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

// ----------------------------------------------------------------------------- placement

/// Top-left of a `side`² window `gap` px beside `anchor` (left of it when there is room, else
/// right), bottom-aligned with it and kept inside `area`; without an anchor, the bottom-right
/// corner of `area`.
pub fn place(anchor: Option<&Rect>, area: &Rect, side: i32, gap: i32) -> (i32, i32) {
    let clamp = |v: i32, lo: i32, hi: i32| if hi < lo { lo } else { v.clamp(lo, hi) };
    let (right, bottom) = (area.x + area.w, area.y + area.h);
    let Some(a) = anchor else {
        return (right - side - gap, bottom - side - gap);
    };
    let left = a.x - gap - side;
    let x = if left >= area.x { left } else { a.x + a.w + gap };
    let y = a.y + a.h - side;
    (clamp(x, area.x, right - side), clamp(y, area.y, bottom - side))
}

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
        let area = Rect { x: 0, y: 0, w: 1920, h: 1040 };
        // Bubble on the right edge: the gesture goes to its left, bottom-aligned.
        let b = Rect { x: 1800, y: 900, w: 80, h: 80 };
        assert_eq!(place(Some(&b), &area, 176, 12), (1800 - 12 - 176, 980 - 176));
        // Bubble on the left edge: to its right, kept inside the work area.
        let b = Rect { x: 10, y: 20, w: 80, h: 80 };
        assert_eq!(place(Some(&b), &area, 176, 12), (102, 0));
        // No bubble: bottom-right corner.
        assert_eq!(place(None, &area, 176, 12), (1920 - 188, 1040 - 188));
        // A work area that does not start at the origin (a second display on the left).
        let area = Rect { x: -1280, y: 0, w: 1280, h: 1000 };
        assert_eq!(place(None, &area, 176, 12), (-188, 1000 - 188));
    }
}

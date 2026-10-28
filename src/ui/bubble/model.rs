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

//! The bubble's layout, hit-testing, edge snapping and look, the same on both systems. Sizes are
//! in DIPs unless a function takes a scale; positions on screen are desktop units ([`Rect`]).
//! Geometry and flat colours follow docs/design.md.

use super::scene::{rgba, Color, Scene};
use super::{BubbleAccount, BubbleMode, BubbleState};
use crate::platform::Rect;

/// Icon-mode window side (DIP): 52 px bubble plus room for the glow.
pub const ICON_DIP: f32 = 80.0;
/// Tray-mode window height (DIP).
pub const TRAY_H_DIP: f32 = 72.0;
pub const MAX_TRAY: usize = 3;
/// Snap margin from the work-area edge (DIP).
pub const EDGE_MARGIN_DIP: f32 = 12.0;
/// Initial margin from the bottom-right corner when no position is persisted (DIP).
pub const DEFAULT_MARGIN_DIP: f32 = 24.0;
/// Movement (DIP) that turns a press into a drag.
pub const DRAG_THRESHOLD_DIP: f32 = 4.0;

// Flat colours (docs/design.md).
pub const ACCENT: u32 = 0x8A7CFF;
pub const SUCCESS: u32 = 0x34D399;
/// `--warn` (dark theme; the bubble is always dark).
pub const WARN: u32 = 0xFBBF24;
/// `--danger` (dark theme).
pub const DANGER: u32 = 0xF87171;
pub const BG_SOLID: u32 = 0x101114;
pub const FG: u32 = 0xECEDEF;
pub const FG_MUTED: u32 = 0x9A9DA6;
pub const FG_SUBTLE: u32 = 0x737784;
pub const WHITE: u32 = 0xFFFFFF;
pub const BLACK: u32 = 0x000000;

/// Icon geometry (DIP): circle centre and outer radius (border-box 52 px).
const ICON_C: f32 = 40.0;
const ICON_R: f32 = 26.0;
/// Tray geometry (DIP).
const TRAY_CY: f32 = 36.0;
const PILL_TOP: f32 = 12.0;
const PILL_BOTTOM: f32 = 60.0;
const SLOT: f32 = 44.0;
const SLOT_GAP: f32 = 6.0;
const FACE_R: f32 = 18.0;
const MORE_R: f32 = 16.0;

// ------------------------------------------------------------------------- pure geometry

/// Tray only when there is something to show (otherwise the icon).
pub fn effective_mode(s: &BubbleState) -> BubbleMode {
    if s.mode == BubbleMode::Tray && !s.accounts.is_empty() {
        BubbleMode::Tray
    } else {
        BubbleMode::Icon
    }
}

/// Clamp the shown account count to 1..=3.
fn tray_n(n: usize) -> usize {
    n.clamp(1, MAX_TRAY)
}

/// Window size in DIP. Tray: 16 + 14 + n*44 + (n-1)*6 + 8 + 32 + 16 by 72.
pub fn dip_size(mode: BubbleMode, n: usize) -> (f32, f32) {
    match mode {
        BubbleMode::Icon => (ICON_DIP, ICON_DIP),
        BubbleMode::Tray => {
            let n = tray_n(n) as f32;
            (16.0 + 14.0 + n * SLOT + (n - 1.0) * SLOT_GAP + 8.0 + 32.0 + 16.0, TRAY_H_DIP)
        }
    }
}

/// Window size in device pixels at `scale`.
pub fn px_size(mode: BubbleMode, n: usize, scale: f32) -> (i32, i32) {
    let (w, h) = dip_size(mode, n);
    ((w * scale).round().max(1.0) as i32, (h * scale).round().max(1.0) as i32)
}

/// Tray layout in DIP.
#[derive(Clone, Debug, PartialEq)]
struct TrayLayout {
    pill_l: f32,
    pill_r: f32,
    grip_l: f32,
    /// Left edge of each 44 px avatar slot.
    slots: Vec<f32>,
    more_cx: f32,
}

fn tray_layout(n: usize) -> TrayLayout {
    let k = tray_n(n);
    let (w, _) = dip_size(BubbleMode::Tray, k);
    let kf = k as f32;
    // Content (grip, avatars, 8 px margin, more) + padding 4/12 + 1 px border each side.
    let pill_w = 14.0 + kf * SLOT + (kf - 1.0) * SLOT_GAP + 8.0 + 32.0 + 16.0 + 2.0;
    let pill_l = (w - pill_w) / 2.0;
    let grip_l = pill_l + 1.0 + 4.0;
    let first = grip_l + 14.0;
    let slots: Vec<f32> = (0..k).map(|i| first + i as f32 * (SLOT + SLOT_GAP)).collect();
    let last_end = first + kf * SLOT + (kf - 1.0) * SLOT_GAP;
    TrayLayout { pill_l, pill_r: pill_l + pill_w, grip_l, slots, more_cx: last_end + 8.0 + MORE_R }
}

/// The centre of tray avatar slot `i` of `n` (DIP, window coordinates), if the tray shows it.
#[cfg(any(test, target_os = "macos"))]
pub fn avatar_centre(n: usize, i: usize) -> Option<(f32, f32)> {
    tray_layout(n).slots.get(i).map(|&sx| (sx + SLOT / 2.0, TRAY_CY))
}

/// What a point (DIP, window coordinates) is over.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hit {
    None,
    /// Icon-mode bubble.
    Bubble,
    /// Tray avatar slot index.
    Avatar(usize),
    /// Tray "⋯" button.
    More,
    /// Tray pill outside any button (drag only).
    Grip,
}

impl Hit {
    /// Whether a press here is a click (a pointing-hand cursor).
    pub fn clickable(self) -> bool {
        matches!(self, Hit::Bubble | Hit::Avatar(_) | Hit::More)
    }
}

pub fn hit_test(mode: BubbleMode, n: usize, x: f32, y: f32) -> Hit {
    match mode {
        BubbleMode::Icon => {
            let d = ((x - ICON_C).powi(2) + (y - ICON_C).powi(2)).sqrt();
            if d <= ICON_R * 1.06 {
                Hit::Bubble
            } else {
                Hit::None
            }
        }
        BubbleMode::Tray => {
            let l = tray_layout(n);
            if x < l.pill_l || x > l.pill_r || y < PILL_TOP || y > PILL_BOTTOM {
                return Hit::None;
            }
            for (i, &sx) in l.slots.iter().enumerate() {
                if x >= sx && x < sx + SLOT && (y - TRAY_CY).abs() <= SLOT / 2.0 {
                    return Hit::Avatar(i);
                }
            }
            let d = ((x - l.more_cx).powi(2) + (y - TRAY_CY).powi(2)).sqrt();
            if d <= MORE_R {
                Hit::More
            } else {
                Hit::Grip
            }
        }
    }
}

fn clampi(v: i32, min: i32, max: i32) -> i32 {
    if max < min {
        min
    } else {
        v.clamp(min, max)
    }
}

/// Keep a `w`×`h` rect at (x,y) fully inside `area`.
pub fn clamp_to_area(x: i32, y: i32, w: i32, h: i32, area: &Rect) -> (i32, i32) {
    (clampi(x, area.x, area.x + area.w - w), clampi(y, area.y, area.y + area.h - h))
}

/// Snap to the nearest horizontal (left/right) edge of `area`, `margin` units in; y clamped.
pub fn snap_rect(r: &Rect, area: &Rect, margin: i32) -> (i32, i32) {
    let cx2 = r.x + (r.x + r.w); // 2 × centre, avoids rounding
    let mid2 = area.x + (area.x + area.w);
    let x = if cx2 < mid2 { area.x + margin } else { area.x + area.w - r.w - margin };
    let y = clampi(r.y, area.y + margin, area.y + area.h - r.h - margin);
    (x, y)
}

/// Resize `r` to `w`×`h` anchored to the edge it sits on (right half → keep right edge),
/// vertical centre kept, then clamped into `area`.
pub fn anchored_rect(r: &Rect, w: i32, h: i32, area: &Rect) -> (i32, i32) {
    let cx2 = r.x + (r.x + r.w);
    let mid2 = area.x + (area.x + area.w);
    let x = if cx2 >= mid2 { r.x + r.w - w } else { r.x };
    let cy2 = r.y + (r.y + r.h);
    let y = (cy2 - h) / 2;
    clamp_to_area(x, y, w, h, area)
}

// ---------------------------------------------------------------------------------- look

/// Interaction state that changes the look.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Visual {
    pub hover: Hit,
    pub pressed: Hit,
    pub dragging: bool,
}

impl Default for Visual {
    fn default() -> Self {
        Visual { hover: Hit::None, pressed: Hit::None, dragging: false }
    }
}

/// Text of the waiting-count badge.
fn badge_label(n: usize) -> String {
    if n > 9 {
        "9+".into()
    } else {
        n.to_string()
    }
}

fn initials_of(a: &BubbleAccount) -> String {
    let src = if a.initials.trim().is_empty() { a.name.as_str() } else { a.initials.as_str() };
    let s: String = src.trim().chars().take(2).collect::<String>().to_uppercase();
    if s.is_empty() {
        "C".into()
    } else {
        s
    }
}

/// Colour of an account's status dot (running, or limited).
fn status_color(a: &BubbleAccount) -> u32 {
    if a.limited {
        DANGER
    } else if a.needs_sign_in {
        WARN
    } else {
        SUCCESS
    }
}

/// White text with a 1 px rgba(0,0,0,.35) drop shadow (CSS `.bubble__initials` / `.tray__face`).
fn label(sc: &mut Scene, text: &str, size: f32, cx: f32, cy: f32) {
    const SHADOW: Color = rgba(BLACK, 0.35);
    sc.text(text, size, cx, cy, rgba(WHITE, 1.0), Some(SHADOW));
}

/// The frame for `st` with the pointer state `v`, sized for its mode.
pub fn scene(st: &BubbleState, v: Visual) -> Scene {
    let mode = effective_mode(st);
    let (w, h) = dip_size(mode, st.accounts.len());
    let mut sc = Scene::new(w, h);
    match mode {
        BubbleMode::Icon => icon(&mut sc, st, v),
        BubbleMode::Tray => tray(&mut sc, st, v),
    }
    sc
}

/// Icon mode: 52 px account circle, 2.5 px accent ring, soft shadow, running dot.
fn icon(sc: &mut Scene, st: &BubbleState, v: Visual) {
    let s = if v.dragging || v.hover == Hit::Bubble && v.pressed == Hit::None {
        1.06
    } else if v.pressed == Hit::Bubble {
        0.97
    } else {
        1.0
    };
    let (c, r) = (ICON_C, ICON_R * s);
    // Soft shadow: 3 concentric alpha rings (accent glow 0 4px 12px .30 + dark 0 1px 3px .35).
    sc.circle(c, c + 4.0, r + 7.0, rgba(ACCENT, 0.04));
    sc.circle(c, c + 3.0, r + 4.5, rgba(ACCENT, 0.07));
    sc.circle(c, c + 1.0, r + 1.5, rgba(BLACK, 0.10));
    match &st.active {
        None => {
            sc.circle(c, c, r, rgba(ACCENT, 1.0));
            label(sc, "C", 20.0 * s, c, c);
        }
        Some(a) => {
            sc.circle(c, c, r, rgba(ACCENT, 1.0));
            sc.circle(c, c, r - 2.5 * s, rgba(a.color, 1.0));
            // Faint dark hairline between fill and ring.
            sc.ring(c, c, r - 3.0 * s, 1.0 * s, rgba(BLACK, 0.18));
            label(sc, &initials_of(a), 15.0 * s, c, c);
            if a.running || a.limited {
                let d = c + 19.5 * s;
                sc.circle(d, d, 7.0 * s, rgba(BG_SOLID, 1.0));
                sc.circle(d, d, 5.0 * s, rgba(status_color(a), 1.0));
            }
        }
    }
    if st.waiting > 0 {
        count_badge(sc, st.waiting, c + 19.0 * s, c - 19.0 * s, s);
    }
}

/// Accent count badge (`9+` above nine) centred on (cx, cy), ringed in the bubble colour.
fn count_badge(sc: &mut Scene, n: usize, cx: f32, cy: f32, s: f32) {
    let text = badge_label(n);
    let half = if text.chars().count() > 1 { 3.5 * s } else { 0.0 };
    let r = 8.0 * s;
    sc.rrect(
        cx - r - half - 2.0 * s,
        cy - r - 2.0 * s,
        cx + r + half + 2.0 * s,
        cy + r + 2.0 * s,
        r + 2.0 * s,
        rgba(BG_SOLID, 1.0),
    );
    sc.rrect(cx - r - half, cy - r, cx + r + half, cy + r, r, rgba(ACCENT, 1.0));
    label(sc, &text, 10.0 * s, cx, cy);
}

/// Tray mode: glass pill, grip, up to 3 avatars, "⋯" button.
fn tray(sc: &mut Scene, st: &BubbleState, v: Visual) {
    let n = tray_n(st.accounts.len());
    let l = tray_layout(n);
    let (pl, pr, pt, pb) = (l.pill_l, l.pill_r, PILL_TOP, PILL_BOTTOM);
    let rad = (pb - pt) / 2.0;
    // Shadow (0 4px 9px .5, 0 1px 3px .35) as 3 stacked alpha layers.
    sc.rrect(pl - 4.0, pt + 1.0, pr + 4.0, pb + 7.0, rad + 4.0, rgba(BLACK, 0.05));
    sc.rrect(pl - 2.5, pt + 1.0, pr + 2.5, pb + 4.5, rad + 2.5, rgba(BLACK, 0.08));
    sc.rrect(pl - 1.0, pt + 0.5, pr + 1.0, pb + 2.0, rad + 1.0, rgba(BLACK, 0.12));
    // Pill: #101114 @ 94 %, 1 px rgba(255,255,255,.14) border.
    sc.rrect(pl, pt, pr, pb, rad, rgba(BG_SOLID, 0.94));
    sc.rrect_stroke(pl + 0.5, pt + 0.5, pr - 0.5, pb - 0.5, rad - 0.5, 1.0, rgba(WHITE, 0.14));
    // Grip: 2×3 dots of 3 px, 3 px gap, centred in a 14 px column.
    for col in 0..2 {
        for row in 0..3 {
            let x = l.grip_l + 2.5 + 1.5 + col as f32 * 6.0;
            let y = TRAY_CY + (row as f32 - 1.0) * 6.0;
            sc.circle(x, y, 1.5, rgba(FG_SUBTLE, 1.0));
        }
    }
    // Avatars.
    for (i, a) in st.accounts.iter().take(MAX_TRAY).enumerate() {
        let cx = l.slots[i] + SLOT / 2.0;
        let cy = TRAY_CY;
        let s = if !v.dragging && v.pressed == Hit::Avatar(i) {
            0.97
        } else if !v.dragging && v.hover == Hit::Avatar(i) && v.pressed == Hit::None {
            1.08
        } else {
            1.0
        };
        let r = FACE_R * s;
        if a.active {
            // Accent glow (0 3px 10px rgba(138,124,255,.30)) then 2.5 px accent ring.
            sc.circle(cx, cy + 3.0, r + 4.0, rgba(ACCENT, 0.07));
            sc.circle(cx, cy + 2.0, r + 2.0, rgba(ACCENT, 0.12));
            sc.circle(cx, cy, r, rgba(ACCENT, 1.0));
            sc.circle(cx, cy, r - 2.5 * s, rgba(a.color, 1.0));
        } else {
            // 2 px rgba(255,255,255,.14) ring outside the fill.
            sc.ring(cx, cy, r + 1.0 * s, 2.0 * s, rgba(WHITE, 0.14));
            sc.circle(cx, cy, r, rgba(a.color, 1.0));
        }
        label(sc, &initials_of(a), 12.0 * s, cx, cy);
        if a.running || a.limited {
            let (dx, dy) = (cx + 15.0 * s, cy + 15.0 * s);
            sc.circle(dx, dy, 6.0 * s, rgba(BG_SOLID, 1.0));
            sc.circle(dx, dy, 4.0 * s, rgba(status_color(a), 1.0));
        }
    }
    // "⋯" button: 32 px round glass.
    let hovered = !v.dragging && v.hover == Hit::More;
    let fill = if v.pressed == Hit::More && !v.dragging {
        0.10
    } else if hovered {
        0.07
    } else {
        0.045
    };
    let mc = l.more_cx;
    sc.circle(mc, TRAY_CY, MORE_R, rgba(WHITE, fill));
    sc.ring(mc, TRAY_CY, MORE_R - 0.5, 1.0, rgba(WHITE, 0.08));
    let dot = if hovered { rgba(FG, 1.0) } else { rgba(FG_MUTED, 1.0) };
    for k in -1..=1 {
        sc.circle(mc + k as f32 * 5.0, TRAY_CY, 1.3, dot);
    }
    if st.waiting > 0 {
        count_badge(sc, st.waiting, mc + 11.0, TRAY_CY - 11.0, 0.875);
    }
}

#[cfg(test)]
mod tests {
    use super::super::scene::Prim;
    use super::*;

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

    fn rect(l: i32, t: i32, r: i32, b: i32) -> Rect {
        Rect { x: l, y: t, w: r - l, h: b - t }
    }

    #[test]
    fn status_dot_colours() {
        let mut a = acct("a", "Work", 0x5B9CF6, true, false);
        assert_eq!(status_color(&a), SUCCESS);
        a.needs_sign_in = true;
        assert_eq!(status_color(&a), WARN);
        a.limited = true;
        assert_eq!(status_color(&a), DANGER);
    }

    #[test]
    fn waiting_badge_label() {
        assert_eq!(badge_label(1), "1");
        assert_eq!(badge_label(9), "9");
        assert_eq!(badge_label(10), "9+");
    }

    #[test]
    fn bubble_sizes_scale_with_dpi() {
        assert_eq!(dip_size(BubbleMode::Icon, 0), (80.0, 80.0));
        assert_eq!(dip_size(BubbleMode::Tray, 1).0, 130.0);
        assert_eq!(dip_size(BubbleMode::Tray, 2).0, 180.0);
        assert_eq!(dip_size(BubbleMode::Tray, 3).0, 230.0);
        assert_eq!(dip_size(BubbleMode::Tray, 9).0, 230.0);
        assert_eq!(px_size(BubbleMode::Tray, 3, 1.5), (345, 108));
    }

    #[test]
    fn avatar_centres_hit_their_own_slot() {
        for n in 1..=3 {
            for i in 0..n {
                let (x, y) = avatar_centre(n, i).unwrap();
                assert_eq!(hit_test(BubbleMode::Tray, n, x, y), Hit::Avatar(i));
            }
            assert_eq!(avatar_centre(n, n), None);
        }
    }

    #[test]
    fn tray_layout_matches_css() {
        let l = tray_layout(1);
        assert_eq!(l.pill_l, 7.0);
        assert_eq!(l.pill_r, 123.0);
        assert_eq!(l.slots, vec![26.0]);
        assert_eq!(l.more_cx, 94.0);
        let l3 = tray_layout(3);
        assert_eq!(l3.slots, vec![26.0, 76.0, 126.0]);
        assert_eq!(l3.more_cx, 194.0);
        assert_eq!(l3.pill_r + 7.0, 230.0);
    }

    #[test]
    fn hit_testing() {
        assert_eq!(hit_test(BubbleMode::Icon, 0, 40.0, 40.0), Hit::Bubble);
        assert_eq!(hit_test(BubbleMode::Icon, 0, 2.0, 2.0), Hit::None);
        assert_eq!(hit_test(BubbleMode::Tray, 2, 48.0, 36.0), Hit::Avatar(0));
        assert_eq!(hit_test(BubbleMode::Tray, 2, 98.0, 36.0), Hit::Avatar(1));
        assert_eq!(hit_test(BubbleMode::Tray, 2, 144.0, 36.0), Hit::More);
        assert_eq!(hit_test(BubbleMode::Tray, 2, 16.0, 36.0), Hit::Grip);
        assert_eq!(hit_test(BubbleMode::Tray, 2, 73.0, 36.0), Hit::Grip); // gap between slots
        assert_eq!(hit_test(BubbleMode::Tray, 2, 3.0, 36.0), Hit::None);
        assert_eq!(hit_test(BubbleMode::Tray, 2, 48.0, 5.0), Hit::None);
        assert!(Hit::More.clickable() && !Hit::Grip.clickable());
    }

    #[test]
    fn effective_mode_falls_back_to_icon() {
        let mut s = BubbleState { mode: BubbleMode::Tray, active: None, accounts: vec![], waiting: 0 };
        assert_eq!(effective_mode(&s), BubbleMode::Icon);
        s.accounts = fake(1);
        assert_eq!(effective_mode(&s), BubbleMode::Tray);
    }

    #[test]
    fn snapping_and_anchoring() {
        let area = rect(0, 0, 1000, 800);
        let r = rect(100, -50, 180, 30);
        assert_eq!(snap_rect(&r, &area, 12), (12, 12));
        let r = rect(700, 790, 780, 870);
        assert_eq!(snap_rect(&r, &area, 12), (1000 - 80 - 12, 800 - 80 - 12));
        // Right half: keep the right edge; vertical centre kept.
        let r = rect(908, 400, 988, 480);
        assert_eq!(anchored_rect(&r, 230, 72, &area), (758, 404));
        // Left half: keep the left edge.
        let r = rect(12, 400, 242, 472);
        assert_eq!(anchored_rect(&r, 80, 80, &area), (12, 396));
        // Clamped into the area.
        assert_eq!(clamp_to_area(-5, 900, 80, 80, &area), (0, 720));
        // A display left of the primary one.
        let left = rect(-1280, 0, 0, 1000);
        assert_eq!(snap_rect(&rect(-300, 500, -220, 580), &left, 12), (-80 - 12, 500));
        assert_eq!(clamp_to_area(10, 10, 80, 80, &left), (-80, 10));
    }

    #[test]
    fn initials_fallbacks() {
        assert_eq!(initials_of(&acct("x", "work", 0, false, false)), "WO");
        let mut a = acct("x", "", 0, false, false);
        assert_eq!(initials_of(&a), "C");
        a.initials = "ab".into();
        assert_eq!(initials_of(&a), "AB");
    }

    fn texts(sc: &Scene) -> Vec<(String, f32)> {
        sc.prims
            .iter()
            .filter_map(|p| match p {
                Prim::Text { text, size, .. } => Some((text.clone(), *size)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn scenes() {
        let empty = BubbleState { mode: BubbleMode::Icon, active: None, accounts: vec![], waiting: 0 };
        let sc = scene(&empty, Visual::default());
        assert_eq!((sc.w, sc.h), (80.0, 80.0));
        assert_eq!(texts(&sc), vec![("C".to_string(), 20.0)]);
        assert_eq!(sc.prims.len(), 5);

        let mut st =
            BubbleState { mode: BubbleMode::Icon, active: Some(fake(1)[0].clone()), accounts: fake(1), waiting: 2 };
        let sc = scene(&st, Visual::default());
        assert_eq!(texts(&sc), vec![("MA".to_string(), 15.0), ("2".to_string(), 10.0)]);
        // Hovering grows the bubble by 6 %.
        let hover = Visual { hover: Hit::Bubble, ..Visual::default() };
        assert!(texts(&scene(&st, hover)).iter().all(|(_, s)| *s > 10.0));

        st.mode = BubbleMode::Tray;
        st.accounts = fake(3);
        st.waiting = 12;
        let sc = scene(&st, Visual::default());
        assert_eq!((sc.w, sc.h), (230.0, 72.0));
        let t = texts(&sc);
        assert_eq!(t.iter().map(|(s, _)| s.as_str()).collect::<Vec<_>>(), ["MA", "WO", "SI", "9+"]);
        assert!(sc.prims.iter().all(|p| match p {
            Prim::Text { shadow, color, .. } => shadow.is_some() && color.rgb == WHITE,
            _ => true,
        }));
    }
}

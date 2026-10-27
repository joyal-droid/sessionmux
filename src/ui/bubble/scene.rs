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

//! What the bubble draws, as a list of primitives in DIPs (origin top-left, y down). Each
//! system's renderer draws them in order, every one with anti-aliasing and source-over blending.

/// `0xRRGGBB` with an opacity of 0.0–1.0.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Color {
    pub rgb: u32,
    pub a: f32,
}

pub const fn rgba(rgb: u32, a: f32) -> Color {
    Color { rgb, a }
}

impl Color {
    /// The red, green and blue channels as 0.0–1.0.
    pub fn channels(self) -> (f32, f32, f32) {
        (
            ((self.rgb >> 16) & 0xFF) as f32 / 255.0,
            ((self.rgb >> 8) & 0xFF) as f32 / 255.0,
            (self.rgb & 0xFF) as f32 / 255.0,
        )
    }
}

/// Letter spacing of [`Prim::Text`], in em; it is also added after the last glyph.
pub const LETTER_SPACING_EM: f32 = 0.04;
/// Offset of a text shadow below the text, in DIPs.
pub const TEXT_SHADOW_DY: f32 = 1.0;

#[derive(Clone, Debug, PartialEq)]
pub enum Prim {
    /// A filled circle; nothing is drawn for a radius of 0 or less.
    Circle { cx: f32, cy: f32, r: f32, color: Color },
    /// A circle outline centred on radius `r`, `w` wide.
    Ring { cx: f32, cy: f32, r: f32, w: f32, color: Color },
    /// A filled rounded rectangle (left, top, right, bottom).
    RoundRect { l: f32, t: f32, r: f32, b: f32, radius: f32, color: Color },
    /// A rounded rectangle outline centred on its edges, `w` wide.
    RoundRectStroke { l: f32, t: f32, r: f32, b: f32, radius: f32, w: f32, color: Color },
    /// One line of text in the system UI font at `size` DIPs, regular weight, centred on
    /// (cx, cy) with [`LETTER_SPACING_EM`]; `shadow` is drawn first, [`TEXT_SHADOW_DY`] lower.
    Text { text: String, size: f32, cx: f32, cy: f32, color: Color, shadow: Option<Color> },
}

/// A frame: its size in DIPs and its primitives, back to front.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Scene {
    pub w: f32,
    pub h: f32,
    pub prims: Vec<Prim>,
}

impl Scene {
    pub fn new(w: f32, h: f32) -> Scene {
        Scene { w, h, prims: Vec::new() }
    }

    pub fn circle(&mut self, cx: f32, cy: f32, r: f32, color: Color) {
        self.prims.push(Prim::Circle { cx, cy, r, color });
    }

    pub fn ring(&mut self, cx: f32, cy: f32, r: f32, w: f32, color: Color) {
        self.prims.push(Prim::Ring { cx, cy, r, w, color });
    }

    pub fn rrect(&mut self, l: f32, t: f32, r: f32, b: f32, radius: f32, color: Color) {
        self.prims.push(Prim::RoundRect { l, t, r, b, radius, color });
    }

    #[allow(clippy::too_many_arguments)]
    pub fn rrect_stroke(&mut self, l: f32, t: f32, r: f32, b: f32, radius: f32, w: f32, color: Color) {
        self.prims.push(Prim::RoundRectStroke { l, t, r, b, radius, w, color });
    }

    pub fn text(&mut self, text: &str, size: f32, cx: f32, cy: f32, color: Color, shadow: Option<Color>) {
        self.prims.push(Prim::Text { text: text.to_string(), size, cx, cy, color, shadow });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colours_and_primitives() {
        assert_eq!(rgba(0xFF8000, 0.5).channels(), (1.0, 128.0 / 255.0, 0.0));
        let mut s = Scene::new(80.0, 72.0);
        s.circle(1.0, 2.0, 3.0, rgba(0, 1.0));
        s.ring(1.0, 2.0, 3.0, 0.5, rgba(0, 1.0));
        s.rrect(0.0, 0.0, 4.0, 4.0, 2.0, rgba(0, 1.0));
        s.rrect_stroke(0.0, 0.0, 4.0, 4.0, 2.0, 1.0, rgba(0, 1.0));
        s.text("AB", 12.0, 5.0, 5.0, rgba(0xFFFFFF, 1.0), None);
        assert_eq!(s.prims.len(), 5);
        assert_eq!((s.w, s.h), (80.0, 72.0));
        assert!(matches!(&s.prims[4], Prim::Text { text, .. } if text == "AB"));
    }
}

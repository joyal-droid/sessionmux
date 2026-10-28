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

//! The floating button. Not built yet: creating it fails with a clear message, so the app runs
//! without it.

use std::path::Path;

use super::unsupported;
use crate::platform::{BubbleHost, Rect};
use crate::ui::bubble::{BubbleEvent, BubbleState};

/// The floating button window (never created in this version).
pub struct Bubble {
    rect: Rect,
}

impl BubbleHost for Bubble {
    fn create(_pos: Option<(i32, i32)>, _on_event: Box<dyn Fn(BubbleEvent)>) -> Result<Self, String> {
        Err(unsupported("The floating button"))
    }

    fn set_state(&mut self, _s: BubbleState) {}

    fn show(&self, _visible: bool) {}

    fn is_visible(&self) -> bool {
        false
    }

    fn rect(&self) -> Rect {
        self.rect
    }

    fn set_position(&self, _x: i32, _y: i32) {}

    fn render_to_png(&self, _path: &Path) -> Result<(), String> {
        Err(unsupported("Drawing the floating button"))
    }
}

/// Dropping the bubble closes its window; none is ever open in this version.
impl Drop for Bubble {
    fn drop(&mut self) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creation_fails_cleanly() {
        assert!(Bubble::create(None, Box::new(|_| {})).is_err());
        let b = Bubble { rect: Rect::default() };
        assert!(!b.is_visible());
        assert_eq!(b.rect(), Rect::default());
        assert!(b.render_to_png(Path::new("x.png")).is_err());
    }
}

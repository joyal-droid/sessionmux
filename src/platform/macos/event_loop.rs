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

//! The UI event loop and the display geometry. The menu bar app is not built yet: `init` fails
//! with a clear message, so a normal start ends with that error instead of running without a UI.

use super::unsupported;
use crate::platform::{Desktop, EventLoop, Os, PlatformEvent, Rect, TimerId, WindowId};
use crate::util::log;

/// The area assumed for layout while no display can be read: a common laptop screen in points,
/// below the menu bar.
const NOMINAL_WORK_AREA: Rect = Rect { x: 0, y: 25, w: 1440, h: 875 };

impl EventLoop for Os {
    fn init(_on_event: fn(PlatformEvent)) -> Result<(), String> {
        Err(unsupported("The menu bar app"))
    }

    fn run() -> i32 {
        1
    }

    /// There is no loop to run `f` on: it is dropped.
    fn post(_f: Box<dyn FnOnce() + Send>) {
        log::debug("event loop: a task was dropped (no event loop)");
    }

    fn set_timer(_id: TimerId, _ms: u32) {}

    fn kill_timer(_id: TimerId) {}

    fn quit() {}

    fn shutdown() {}
}

impl Desktop for Os {
    fn work_area(_r: &Rect) -> Rect {
        NOMINAL_WORK_AREA
    }

    /// AppKit lays out in points already.
    fn units_per_dip(_r: &Rect) -> f32 {
        1.0
    }

    fn is_foreground(_w: WindowId) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init_fails_cleanly_and_the_rest_does_nothing() {
        assert!(Os::init(|_| {}).is_err());
        Os::post(Box::new(|| unreachable!("no loop runs posted tasks")));
        Os::set_timer(TimerId::Pin, 10);
        Os::kill_timer(TimerId::Pin);
        Os::quit();
        Os::shutdown();
        assert_ne!(Os::run(), 0);
    }

    #[test]
    fn geometry_is_in_points() {
        let r = Rect { x: 10, y: 10, w: 1, h: 1 };
        assert_eq!(Os::units_per_dip(&r), 1.0);
        let wa = Os::work_area(&r);
        assert!(wa.w > 0 && wa.h > 0);
        assert!(!Os::is_foreground(WindowId(1)));
    }
}

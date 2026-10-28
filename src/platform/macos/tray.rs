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

//! The menu bar icon. Not built yet: creating it fails with a clear message.

use super::unsupported;
use crate::platform::{Rect, TrayHost};
use crate::ui::tray::TrayMenu;

/// The menu bar icon (never created in this version).
pub struct Tray {
    _private: (),
}

impl TrayHost for Tray {
    fn create() -> Result<Self, String> {
        Err(unsupported("The menu bar icon"))
    }

    fn set_menu(&mut self, _m: &TrayMenu) {}

    fn set_waiting(&self, _n: usize) {}

    fn anchor(&self) -> Option<Rect> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creation_fails_cleanly() {
        assert!(<Tray as TrayHost>::create().is_err());
        let t = Tray { _private: () };
        t.set_waiting(2);
        assert_eq!(t.anchor(), None);
    }
}

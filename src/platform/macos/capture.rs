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

//! What the smoke run needs: output on the starting terminal, a UI pump, window captures and
//! memory figures. Captures are not built yet and report an error; memory figures read as zero.

use std::path::Path;
use std::time::Duration;

use super::unsupported;
use crate::platform::{Capture, Footprint, Os, WindowId};

impl Capture for Os {
    /// A macOS process started from a terminal already writes to it.
    fn attach_console() {}

    /// No UI events are delivered yet: waits for `ms` milliseconds.
    fn pump(ms: u64) {
        std::thread::sleep(Duration::from_millis(ms));
    }

    fn panel_png(_w: WindowId, _path: &Path) -> Result<(u32, u32), String> {
        Err(unsupported("Capturing the panel"))
    }

    fn footprint(_helpers: bool) -> Footprint {
        Footprint::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn captures_report_an_error() {
        Os::attach_console();
        Os::pump(1);
        assert!(Os::panel_png(WindowId(0), Path::new("x.png")).is_err());
        assert_eq!(Os::footprint(true), Footprint::default());
    }
}
